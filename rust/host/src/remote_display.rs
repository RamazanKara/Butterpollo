use crate::state::Shared;
use anyhow::{Context, Result, bail};
use butterpollo_core::{
    rtsp::Negotiated,
    session::Role,
    topology::{Kind, Layout, Node},
};
use butterpollo_windows::display::{self, Retained};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn layout(h: &Shared) -> Result<Layout> {
    let state = h.paired.read().unwrap();
    match state.document["root"].get("remote_display_layout") {
        Some(Value::String(s)) => Ok(serde_json::from_str(s)?),
        Some(value) => Ok(serde_json::from_value(value.clone())?),
        None => Ok(Layout::default()),
    }
}
fn nodes(h: &Shared) -> Result<Vec<Node>> {
    let monitors = display::monitors()?;
    let retained = h.monitors.lock().unwrap();
    let paired = h.paired.read().unwrap();
    let mut nodes = display::baseline_nodes()?;
    for (id, lease) in retained.iter() {
        let monitor = monitors
            .iter()
            .find(|m| m.display_name == lease.current_output())
            .context("retained display disappeared")?;
        let node = nodes
            .iter_mut()
            .find(|n| n.device_id == monitor.device_id)
            .context("retained display mode unavailable")?;
        node.kind = Kind::Client;
        node.id = id.clone();
        node.label = paired
            .clients
            .iter()
            .find(|c| &c.uuid == id)
            .map_or(id.clone(), |c| c.name.clone());
    }
    Ok(nodes)
}
pub fn snapshot(h: &Shared) -> Result<Value> {
    let layout = layout(h)?;
    let current = nodes(h)?;
    let desired = layout.compose(&current)?;
    let nodes: Vec<_> = desired
        .iter()
        .map(|n| {
            let mut value = serde_json::to_value(n).unwrap();
            value["current_position"] = current
                .iter()
                .find(|old| old.id == n.id)
                .map(|old| json!(old.desired_position))
                .unwrap_or(Value::Null);
            value
        })
        .collect();
    let clients: Vec<_> = h
        .paired
        .read()
        .unwrap()
        .clients
        .iter()
        .map(|c| json!({"uuid":c.uuid,"name":c.name,"enabled":c.enabled}))
        .collect();
    let runtime: serde_json::Map<_,_> = h.monitors.lock().unwrap().keys().map(|id|(id.clone(),json!({"lifecycle":"ready","ready":true,"retryable":false,"lease_held":true,"warning":"","plaintext_rtsp_warning":""}))).collect();
    Ok(
        json!({"status":true,"version":1,"layout":layout,"capacity":{"max":4,"used":runtime.len()},"warnings":[],"nodes":nodes,"clients":clients,"runtime":runtime}),
    )
}
pub fn save(h: &Shared, value: &Value) -> Result<Value> {
    let next: Layout = serde_json::from_value(value.get("layout").unwrap_or(value).clone())?;
    let clients = h
        .paired
        .read()
        .unwrap()
        .clients
        .iter()
        .map(|c| c.uuid.clone())
        .collect::<Vec<_>>();
    let physical = nodes(h)?
        .into_iter()
        .filter(|n| n.kind == Kind::Physical)
        .map(|n| n.id)
        .collect::<Vec<_>>();
    next.validate(&clients, &physical)?;
    let mut state = h.paired.write().unwrap();
    let mut document = state.clone();
    document.document["root"]["remote_display_layout"] = json!(serde_json::to_string(&next)?);
    document.save(&h.paired_path)?;
    *state = document;
    Ok(json!({"status":true,"applies_on_next_activation":true,"layout":next}))
}
pub fn activate(h: &Shared, client: &str, config: &Negotiated) -> Result<Arc<Retained>> {
    let mut monitors = h.monitors.lock().unwrap();
    if let Some(existing) = monitors.get(client) {
        if existing.mode
            != (
                config.width,
                config.height,
                config.fps_millihz(),
                config.hdr,
            )
        {
            bail!("retained monitor uses a different mode; disconnect it before changing mode");
        }
        return Ok(existing.clone());
    }
    if monitors.len() >= 4 {
        bail!("remote monitor capacity reached");
    }
    let identity = h
        .paired
        .read()
        .unwrap()
        .clients
        .iter()
        .find(|c| c.uuid == client)
        .cloned();
    let options = display::VirtualOptions {
        label: identity
            .as_ref()
            .map_or_else(|| "Moonlight Client".into(), |c| c.name.clone()),
        peak_nits: identity
            .as_ref()
            .and_then(|c| c.extra.get("hdr_profile"))
            .and_then(Value::as_str)
            .and_then(|p| {
                butterpollo_windows::hdr_profile::peak_luminance(p)
                    .ok()
                    .flatten()
            })
            .unwrap_or(1000)
            .clamp(400, 2000),
    };
    let host = Arc::downgrade(h);
    let lease = Arc::new(Retained::create_options(
        client,
        config.width,
        config.height,
        butterpollo_core::framegen::Rate(config.fps_millihz()),
        config.hdr,
        &options,
        Some(Box::new(move || {
            if let Some(h) = host.upgrade() {
                let desired = layout(&h)?.compose(&nodes(&h)?)?;
                display::apply_layout(&desired)?;
            }
            Ok(())
        })),
    )?);
    monitors.insert(client.into(), lease.clone());
    drop(monitors);
    let result = (|| -> Result<()> { display::apply_layout(&layout(h)?.compose(&nodes(h)?)?) })();
    if let Err(error) = result {
        disconnect(h, Some(client));
        return Err(error);
    }
    Ok(lease)
}
pub fn disconnect(h: &Shared, client: Option<&str>) {
    h.sessions
        .lock()
        .unwrap()
        .stop_role(Role::RemoteMonitor, client);
    let mut monitors = h.monitors.lock().unwrap();
    let (kept, removed): (
        std::collections::BTreeMap<_, _>,
        std::collections::BTreeMap<_, _>,
    ) = std::mem::take(&mut *monitors)
        .into_iter()
        .partition(|(id, _)| client.is_some_and(|c| c != id));
    *monitors = kept;
    let empty = monitors.is_empty();
    drop(monitors);
    // Dropping the last lease joins its feeder, whose recovery callback takes
    // the monitors lock to lay out the displays: never drop one under it.
    drop(removed);
    let result = if empty {
        display::restore_positions()
    } else {
        layout(h)
            .and_then(|layout| layout.compose(&nodes(h)?))
            .and_then(|nodes| display::apply_layout(&nodes))
    };
    if let Err(error) = result {
        tracing::warn!(%error,"remote display topology restoration failed");
    }
}
