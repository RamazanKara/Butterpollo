//! Saved display layouts and stream settings leases with restoration.

use super::hdr::{HdrAction, hdr_action, restore_hdr};
use super::modes::{Timing, supported_modes};
use super::virtual_display::{DisplayLease, display_lease};
use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub nodes: Vec<butterpollo_core::topology::Node>,
    pub hdr: std::collections::BTreeMap<String, bool>,
    #[serde(default)]
    pub clone_groups: Vec<Vec<String>>,
    #[serde(default)]
    pub scale: std::collections::BTreeMap<String, u32>,
    #[serde(default)]
    pub rotation: std::collections::BTreeMap<String, u32>,
}
impl Snapshot {
    pub fn read(path: &std::path::Path) -> Result<Self> {
        if std::fs::metadata(path)?.len() > 4 * 1024 * 1024 {
            bail!("display snapshot exceeds its limit");
        }
        let value = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut snapshot = Self::decode(&value)?;
        let available = Topology::query_all()?.monitors();
        let normalized = |id: &str| {
            available
                .iter()
                .find(|m| m.matches(id))
                .map_or_else(|| id.to_owned(), |m| m.device_id.clone())
        };
        for node in &mut snapshot.nodes {
            node.id = normalized(&node.id);
            node.device_id = normalized(&node.device_id);
        }
        snapshot.hdr = snapshot
            .hdr
            .into_iter()
            .map(|(id, v)| (normalized(&id), v))
            .collect();
        snapshot.scale = snapshot
            .scale
            .into_iter()
            .map(|(id, v)| (normalized(&id), v))
            .collect();
        snapshot.rotation = snapshot
            .rotation
            .into_iter()
            .map(|(id, v)| (normalized(&id), v))
            .collect();
        snapshot.clone_groups = snapshot
            .clone_groups
            .iter()
            .map(|group| group.iter().map(|id| normalized(id)).collect())
            .collect();
        Ok(snapshot)
    }
    pub fn decode(value: &serde_json::Value) -> Result<Self> {
        if value.get("version").is_some() {
            let snapshot: Self = serde_json::from_value(value.clone())?;
            if snapshot.version != 1
                || snapshot.nodes.is_empty()
                || snapshot.nodes.len() > 64
                || snapshot.nodes.iter().any(|node| {
                    node.mode.width == 0
                        || node.mode.height == 0
                        || !node.mode.refresh_hz.is_finite()
                        || !(1.0..=1000.0).contains(&node.mode.refresh_hz)
                })
            {
                bail!("invalid Rust display snapshot");
            }
            return Ok(snapshot);
        }
        use butterpollo_core::topology::{Kind, Mode, Node, Position};
        let groups: Vec<Vec<String>> = serde_json::from_value(
            value
                .get("topology")
                .cloned()
                .context("saved display topology is missing")?,
        )?;
        let modes = value["modes"]
            .as_object()
            .context("saved display modes are missing")?;
        if groups.len() > 64 || modes.len() > 64 {
            bail!("saved display count exceeds its limit");
        }
        let active: std::collections::BTreeSet<_> = groups.iter().flatten().collect();
        let mut snapshot = Self {
            version: 1,
            nodes: vec![],
            hdr: Default::default(),
            clone_groups: groups.iter().filter(|g| g.len() > 1).cloned().collect(),
            scale: Default::default(),
            rotation: Default::default(),
        };
        for (id, mode) in modes {
            let number = |key| mode[key].as_u64().context("invalid saved display mode");
            let (width, height, num, den) = (
                u32::try_from(number("w")?)?,
                u32::try_from(number("h")?)?,
                number("num")?,
                number("den")?,
            );
            if width == 0 || height == 0 || width > 16384 || height > 16384 || den == 0 || num == 0
            {
                bail!("invalid saved display mode");
            }
            let x = i32::try_from(value["origins"][id]["x"].as_i64().unwrap_or(0))?;
            let y = i32::try_from(value["origins"][id]["y"].as_i64().unwrap_or(0))?;
            snapshot.nodes.push(Node {
                id: id.clone(),
                device_id: id.clone(),
                label: id.clone(),
                kind: Kind::Physical,
                active: active.contains(id),
                primary: value["primary"].as_str() == Some(id),
                desired_position: Position { x, y },
                mode: Mode {
                    width,
                    height,
                    refresh_hz: num as f64 / den as f64,
                },
            });
            if let Some(enabled) = match value["hdr"][id].as_str() {
                Some("on") => Some(true),
                Some("off") => Some(false),
                _ => value["hdr"][id].as_bool(),
            } {
                snapshot.hdr.insert(id.clone(), enabled);
            }
            if let Some(degrees) = value["layouts"][id]["rotation"].as_u64()
                && matches!(degrees, 0 | 90 | 180 | 270)
            {
                snapshot
                    .rotation
                    .insert(id.clone(), degrees as u32 / 90 + 1);
            }
        }
        if snapshot.nodes.is_empty() {
            bail!("saved display snapshot is empty");
        }
        Ok(snapshot)
    }
    pub fn capture() -> Result<Self> {
        let topology = Topology::query()?;
        Ok(Self {
            version: 1,
            nodes: topology.nodes()?,
            hdr: topology
                .monitors()
                .into_iter()
                .map(|m| (m.device_id, m.hdr_enabled))
                .collect(),
            rotation: topology.rotations(),
            clone_groups: topology.clone_groups(),
            scale: topology
                .monitors()
                .iter()
                .filter_map(|m| {
                    dpi_scale(m)
                        .ok()
                        .map(|percent| (m.device_id.clone(), percent))
                })
                .collect(),
        })
    }
    pub fn restore(&self) -> Result<()> {
        self.restore_excluding(&[])
    }
    /// Whether every display this layout turns on is connected now.
    pub fn displays_connected(&self) -> bool {
        let Ok(available) = Topology::query_all().map(|t| t.monitors()) else {
            return false;
        };
        let mut active = self.nodes.iter().filter(|n| n.active).peekable();
        active.peek().is_some()
            && active.all(|n| available.iter().any(|m| m.device_id == n.device_id))
    }
    pub fn restore_excluding(&self, excluded: &[String]) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported Rust display snapshot version");
        }
        let current = Topology::query()?;
        let available = Topology::query_all()?.monitors();
        let preserve = |id: &str| excluded.iter().any(|e| id.eq_ignore_ascii_case(e));
        let mut ids = self
            .nodes
            .iter()
            .filter(|n| {
                n.active
                    && !preserve(&n.device_id)
                    && available.iter().any(|m| m.device_id == n.device_id)
            })
            .map(|n| n.device_id.clone())
            .collect::<Vec<_>>();
        ids.extend(
            current
                .monitors()
                .iter()
                .filter(|m| preserve(&m.device_id))
                .map(|m| m.device_id.clone()),
        );
        Topology::set_active(&ids).context("cannot activate the saved displays")?;
        // A display that cannot take back its old setting must not leave the
        // rest of the layout unrestored; report every such failure at the end.
        let mut failures = Vec::new();
        if let Err(error) = Topology::query().and_then(|mut t| {
            t.set_rotations(
                &self
                    .rotation
                    .iter()
                    .filter(|(id, _)| !preserve(id))
                    .map(|(id, r)| (id.clone(), *r))
                    .collect(),
            )
        }) {
            failures.push(format!("rotation: {error:#}"));
        }
        let monitors = monitors()?;
        for n in &self.nodes {
            if preserve(&n.device_id) {
                continue;
            }
            let Some(m) = monitors.iter().find(|m| m.device_id == n.device_id) else {
                continue;
            };
            let mut step = |what: &str, result: Result<()>| {
                if let Err(error) = result {
                    failures.push(format!(
                        "{} ({}) {what}: {error:#}",
                        n.label, m.display_name
                    ));
                }
            };
            step(
                &format!(
                    "mode {}x{}@{:.3}",
                    n.mode.width, n.mode.height, n.mode.refresh_hz
                ),
                Topology::set_mode_rate(
                    &m.display_name,
                    n.mode.width,
                    n.mode.height,
                    butterpollo_core::framegen::Rate((n.mode.refresh_hz * 1000.0).round() as u32),
                ),
            );
            if let Some(enabled) = self.hdr.get(&n.device_id) {
                step(&format!("HDR {enabled}"), set_hdr(m, *enabled));
            }
            if let Some(percent) = self.scale.get(&n.device_id) {
                step(&format!("scale {percent}%"), set_dpi_scale(m, *percent));
            }
        }
        // set_active gave every display its own desktop source. Join clone
        // groups first: placing their members separately would put two
        // desktops at the same origin.
        let mut topology = Topology::query()?;
        topology
            .restore_clone_groups(
                &self
                    .clone_groups
                    .iter()
                    .filter(|group| group.iter().all(|id| !preserve(id)))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .context("cannot restore the cloned displays")?;
        topology = Topology::query()?;
        topology
            .set_positions(
                &self
                    .nodes
                    .iter()
                    .filter(|n| !preserve(&n.device_id))
                    .map(|n| (n.device_id.clone(), n.desired_position))
                    .collect(),
            )
            .context("cannot restore the display positions")?;
        // Moving displays can renegotiate a display's timing (a TV at 120 Hz
        // came back at 60 Hz), so check every rate again. A clone member's
        // GDI name changed when it joined its group; find it by device.
        for n in self.nodes.iter().filter(|n| !preserve(&n.device_id)) {
            let Some(m) = monitors.iter().find(|m| m.device_id == n.device_id) else {
                continue;
            };
            if failures
                .iter()
                .any(|f| f.contains(&format!("({})", m.display_name)))
            {
                continue;
            }
            if let Err(error) = Topology::set_mode_rate(
                &n.device_id,
                n.mode.width,
                n.mode.height,
                butterpollo_core::framegen::Rate((n.mode.refresh_hz * 1000.0).round() as u32),
            ) {
                failures.push(format!(
                    "{} ({}) mode after layout: {error:#}",
                    n.label, m.display_name
                ));
            }
        }
        if !failures.is_empty() {
            bail!("display layout restored except {}", failures.join("; "));
        }
        Ok(())
    }
}

type PositionChanges = std::collections::BTreeMap<
    String,
    (
        butterpollo_core::topology::Position,
        butterpollo_core::topology::Position,
    ),
>;
static POSITIONS: std::sync::Mutex<PositionChanges> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
pub fn baseline_nodes() -> Result<Vec<butterpollo_core::topology::Node>> {
    let mut nodes = Topology::query()?.nodes()?;
    let changes = POSITIONS.lock().unwrap();
    for node in &mut nodes {
        if let Some((before, _)) = changes.get(&node.device_id) {
            node.desired_position = *before;
        }
    }
    Ok(nodes)
}
pub fn apply_layout(nodes: &[butterpollo_core::topology::Node]) -> Result<()> {
    use butterpollo_core::topology::Kind;
    let mut topology = Topology::query()?;
    let current = topology.nodes()?;
    let monitors = topology.monitors();
    let mut changes = POSITIONS.lock().unwrap();
    for node in nodes.iter().filter(|n| n.kind == Kind::Physical) {
        if let Some(old) = current.iter().find(|old| old.device_id == node.device_id) {
            let before = changes
                .get(&node.device_id)
                .map_or(old.desired_position, |(before, _)| *before);
            if before != node.desired_position {
                let output = &monitors
                    .iter()
                    .find(|m| m.device_id == node.device_id)
                    .context("display disappeared")?
                    .display_name;
                crate::display_recovery::position(
                    &node.device_id,
                    output,
                    before,
                    node.desired_position,
                )?;
                changes.insert(node.device_id.clone(), (before, node.desired_position));
            }
        }
    }
    topology.set_positions(
        &nodes
            .iter()
            .map(|n| (n.device_id.clone(), n.desired_position))
            .collect(),
    )
}
pub fn restore_positions() -> Result<()> {
    let mut changes = POSITIONS.lock().unwrap();
    if changes.is_empty() {
        return Ok(());
    }
    let mut topology = Topology::query()?;
    let nodes = topology.nodes()?;
    let positions: std::collections::BTreeMap<_, _> = changes
        .iter()
        .filter_map(|(id, (before, applied))| {
            nodes
                .iter()
                .find(|n| &n.device_id == id && n.desired_position == *applied)
                .map(|_| (id.clone(), *before))
        })
        .collect();
    if !positions.is_empty() {
        topology.set_positions(&positions)?;
    }
    for id in changes.keys() {
        crate::display_recovery::release_position(id)?;
    }
    changes.clear();
    Ok(())
}
// Every stream holds a settings lease, including streams that inherit an existing
// HDR mode. Restoration belongs to the last owner, so one client's departure
// cannot change the display underneath another client.
struct Settings {
    users: usize,
    /// (previous, applied).
    mode: Option<(Timing, Timing)>,
    color: Option<(Monitor, bool, bool)>,
}
static SETTINGS: std::sync::Mutex<std::collections::BTreeMap<String, Settings>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
/// The displays streams, paused game displays, launches being prepared and
/// remote monitors hold now, by device id.
pub fn leased_displays() -> Vec<String> {
    SETTINGS.lock().unwrap().keys().cloned().collect()
}
/// Whether a stream may change a display's mode or HDR. Only its sole user
/// may: a second stream keeps what the first one streams, whether or not
/// the first changed anything, and the encoder scales.
#[derive(Debug, PartialEq)]
enum Plan {
    Keep,
    Change,
}
/// `recorded`: the value an earlier stream applied to this display.
fn plan<T: PartialEq>(users: usize, recorded: Option<&T>, current: &T, requested: &T) -> Plan {
    if users > 1 || recorded.is_some() || current == requested {
        Plan::Keep
    } else {
        Plan::Change
    }
}
/// What a stream that leaves a setting as it is restores at its end: a
/// pending journal original, when the display no longer has it. An earlier
/// stream ended while the display was away (a TV in standby) and left its
/// change behind; without this the release would clear that entry.
fn adopt<T: PartialEq + Copy>(pending: Option<T>, current: T) -> Option<(T, T)> {
    pending
        .filter(|original| *original != current)
        .map(|original| (original, current))
}
/// Take a stream's part in a display's HDR: change it when the stream is
/// its sole user and the display can, and record what teardown restores.
/// `pending`: the journal's original, which wins over the current state.
fn lease_hdr(
    settings: &mut Settings,
    chosen: &Monitor,
    requested: Option<bool>,
    pending: Option<bool>,
    journal: impl FnOnce(bool, bool) -> Result<()>,
    set: impl FnOnce(&Monitor, bool) -> Result<()>,
) -> Result<()> {
    match hdr_action(chosen.hdr_supported, chosen.hdr_enabled, requested) {
        HdrAction::Keep => {}
        HdrAction::Skip => tracing::warn!(
            output = %chosen.display_name,
            "Display does not support HDR; capturing SDR content even if the wire stream is HDR. Use an HDR virtual display or disable HDR in the client"
        ),
        HdrAction::Set(enabled) => {
            let applied = settings.color.as_ref().map(|(_, _, applied)| applied);
            if plan(settings.users, applied, &chosen.hdr_enabled, &enabled) == Plan::Change {
                journal(chosen.hdr_enabled, enabled)?;
                let previous = pending.unwrap_or(chosen.hdr_enabled);
                settings.color = Some((chosen.clone(), previous, enabled));
                return set(chosen, enabled);
            }
            tracing::warn!(
                output = %chosen.display_name,
                hdr = chosen.hdr_enabled,
                "Another stream uses this display; keeping its HDR state. Use a separate virtual display or match the other stream HDR setting"
            );
        }
    }
    if settings.color.is_none() {
        settings.color = adopt(pending, chosen.hdr_enabled)
            .map(|(previous, applied)| (chosen.clone(), previous, applied));
    }
    Ok(())
}
/// Undo what a display's streams changed. False when its journal entry must
/// stay: a physical display that is gone gets its settings back when it
/// returns (Windows remembers them per display), and recovery retries a
/// failed restore.
fn restore_settings(
    settings: &Settings,
    identity: &str,
    output: &str,
    is_virtual: bool,
    now: Option<&[Monitor]>,
    set_hdr: impl FnOnce(&Monitor, bool) -> Result<()>,
    restore_mode: impl FnOnce(Timing, Timing) -> Result<()>,
) -> bool {
    let mut restored = true;
    let present = now.is_some_and(|all| all.iter().any(|m| m.device_id == identity));
    if !present && !is_virtual && (settings.color.is_some() || settings.mode.is_some()) {
        restored = false;
        tracing::info!(output = %output, "display is gone; its settings are restored when it returns");
    }
    if let Some((monitor, previous, applied)) = &settings.color
        && let Some(now) = now
        && let Err(e) = restore_hdr(monitor, *previous, *applied, now, set_hdr)
    {
        restored = false;
        tracing::warn!(error=%e,"HDR restoration failed");
    }
    if let Some((previous, applied)) = settings.mode
        && let Err(e) = restore_mode(previous, applied)
    {
        restored = false;
        tracing::warn!(error=%e,"display mode restoration failed");
    }
    restored
}
/// Set the previous mode back if the display still has the one applied.
fn restore_mode_rate(
    output: &str,
    identity: &str,
    previous: Timing,
    applied: Timing,
) -> Result<()> {
    if mode(output).is_ok_and(|m| (m.dmPelsWidth, m.dmPelsHeight) == (applied.0, applied.1))
        && Topology::query()
            .and_then(|t| t.refresh(identity))
            .is_ok_and(|r| r.0 == applied.2)
    {
        Topology::set_mode_rate(
            output,
            previous.0,
            previous.1,
            butterpollo_core::framegen::Rate(previous.2),
        )?;
    }
    Ok(())
}
pub struct Guard {
    pub output: String,
    virtual_display: Option<DisplayLease>,
    identity: String,
    generation: u64,
    hdr: Option<bool>,
    /// A recovered display's HDR and mode still to be restored.
    restore_pending: bool,
}
impl Guard {
    #[allow(clippy::too_many_arguments)] // Native display lease parameters.
    pub fn new(
        output: &str,
        virtual_mode: bool,
        stable_id: &str,
        width: u32,
        height: u32,
        fps: u32,
        hdr: bool,
        physical_resolution: Option<(u32, u32)>,
        physical_refresh: Option<u32>,
    ) -> Result<Self> {
        Self::new_options(
            output,
            virtual_mode,
            stable_id,
            width,
            height,
            butterpollo_core::framegen::Rate(fps.checked_mul(1000).context("refresh overflow")?),
            Some(hdr),
            physical_resolution,
            physical_refresh.map(|r| butterpollo_core::framegen::Rate(r.saturating_mul(1000))),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new_options(
        output: &str,
        virtual_mode: bool,
        stable_id: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
        hdr: Option<bool>,
        physical_resolution: Option<(u32, u32)>,
        physical_refresh: Option<butterpollo_core::framegen::Rate>,
    ) -> Result<Self> {
        Self::new_virtual_options(
            output,
            virtual_mode,
            stable_id,
            width,
            height,
            rate,
            hdr,
            physical_resolution,
            physical_refresh,
            &VirtualOptions::default(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new_virtual_options(
        output: &str,
        virtual_mode: bool,
        stable_id: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
        hdr: Option<bool>,
        physical_resolution: Option<(u32, u32)>,
        physical_refresh: Option<butterpollo_core::framegen::Rate>,
        options: &VirtualOptions,
    ) -> Result<Self> {
        let virtual_display = if virtual_mode {
            Some(display_lease(stable_id, width, height, rate.0, options)?)
        } else {
            None
        };
        let choices = monitors()?;
        let chosen = if let Some(v) = &virtual_display {
            let display = v.lock().unwrap();
            choices.iter().find(|d| display.owns_monitor(d))
        } else {
            choices.iter().find(|d| d.matches(output)).or_else(|| {
                if output.is_empty() {
                    choices
                        .iter()
                        .find(|d| d.primary)
                        .or_else(|| choices.first())
                } else {
                    None
                }
            })
        }
        .context("display unavailable")?
        .clone();
        let mut guard = Self {
            output: chosen.display_name.clone(),
            generation: virtual_display
                .as_ref()
                .map_or(0, |v| v.lock().unwrap().generation),
            hdr,
            virtual_display,
            identity: chosen.device_id.clone(),
            restore_pending: false,
        };
        let result = (|| -> Result<()> {
            let mut all = SETTINGS.lock().unwrap();
            let settings = all.entry(guard.identity.clone()).or_insert(Settings {
                users: 0,
                mode: None,
                color: None,
            });
            settings.users += 1;
            // The first user takes over what an earlier stream left pending
            // on this display, so that its original values come back.
            let pending = if settings.users == 1 {
                crate::display_recovery::pending(&guard.identity)
            } else {
                Default::default()
            };
            if (physical_resolution.is_some() || physical_refresh.is_some())
                && guard.virtual_display.is_none()
            {
                let previous = mode(&guard.output)?;
                let previous_rate = Topology::query()?.refresh(&guard.identity)?;
                let current = (previous.dmPelsWidth, previous.dmPelsHeight, previous_rate.0);
                let supported = supported_modes(&guard.output);
                let requested = if supported.is_empty() {
                    let (width, height) = physical_resolution
                        .unwrap_or((previous.dmPelsWidth, previous.dmPelsHeight));
                    (width, height, physical_refresh.unwrap_or(previous_rate).0)
                } else {
                    butterpollo_core::display_policy::physical_mode(
                        &supported,
                        (physical_resolution, physical_refresh.map(|rate| rate.0)),
                        current,
                    )
                };
                if let Some((width, height)) = physical_resolution
                    && (width, height) != (requested.0, requested.1)
                {
                    tracing::info!(
                        output = %guard.output,
                        "display has no {width}x{height} mode; keeping {}x{} and scaling the stream",
                        requested.0,
                        requested.1
                    );
                }
                let (width, height, fps) = requested;
                let applied = settings.mode.as_ref().map(|(_, applied)| applied);
                if plan(settings.users, applied, &current, &requested) == Plan::Keep {
                    // Another stream uses this display; keep its mode and let
                    // the encoder scale, rather than refuse the second stream.
                    let kept = *applied.unwrap_or(&current);
                    if kept != requested {
                        tracing::info!(
                            output = %guard.output,
                            "another stream uses this display at {}x{}; scaling this stream",
                            kept.0,
                            kept.1
                        );
                    }
                } else {
                    let original = pending.mode.map_or(current, |(original, _)| original);
                    crate::display_recovery::mode_rate(
                        &guard.identity,
                        &guard.output,
                        current,
                        requested,
                    )?;
                    settings.mode = Some((original, requested));
                    let applied = Topology::set_mode_rate(
                        &guard.output,
                        width,
                        height,
                        butterpollo_core::framegen::Rate(fps),
                    );
                    // Record what the display has now even when the rate
                    // was refused: a TV can take the resolution and keep
                    // its old rate, and teardown restores only the mode
                    // it finds recorded.
                    let actual = mode(&guard.output)?;
                    let actual_rate = Topology::query()?.refresh(&guard.identity)?;
                    let actual_mode = (actual.dmPelsWidth, actual.dmPelsHeight, actual_rate.0);
                    crate::display_recovery::mode_rate(
                        &guard.identity,
                        &guard.output,
                        current,
                        actual_mode,
                    )?;
                    settings.mode = Some((original, actual_mode));
                    applied?;
                }
            }
            // A stream that leaves the mode as it is, or requests none, takes
            // the earlier stream's change over: its end restores the original
            // if the display still has that change, as recovery would.
            if settings.mode.is_none() && guard.virtual_display.is_none() {
                settings.mode = pending
                    .mode
                    .filter(|(original, applied)| original != applied);
            }
            lease_hdr(
                settings,
                &chosen,
                hdr,
                pending.hdr,
                |before, applied| {
                    crate::display_recovery::hdr(&guard.identity, &guard.output, before, applied)
                },
                set_hdr,
            )
        })();
        result?;
        if let Some(display) = &guard.virtual_display {
            let mut display = display.lock().unwrap();
            display.finish_hotplug("after settings")?;
            guard.output = display.name.clone();
        }
        Ok(guard)
    }
    /// Set the stream's mode on an owned virtual display again, after a step
    /// that lets Windows recall another one, such as applying a layout. A
    /// refusal is logged; callers verify and report the mode themselves.
    pub fn apply_virtual_mode(&self, stage: &str) {
        if let Some(display) = &self.virtual_display {
            display.lock().unwrap().apply_mode(stage);
        }
    }
    /// True when the display was recovered, or a recovered display's HDR and
    /// mode were restored late: capture and the stream's layout follow it.
    pub fn feed(&mut self) -> Result<bool> {
        if let Some(display) = &mut self.virtual_display {
            let mut display = display.lock().unwrap();
            display.feed()?;
            if display.generation != self.generation {
                let chosen = monitors()?
                    .into_iter()
                    .find(|m| display.owns_monitor(m))
                    .context("recovered display unavailable")?;
                // A display switched back on can have another desktop name,
                // which the heartbeat's own lookup may not have caught.
                display.name = chosen.display_name.clone();
                {
                    let mut settings = SETTINGS.lock().unwrap();
                    if chosen.device_id != self.identity {
                        if let Some(old) = settings.get_mut(&self.identity) {
                            old.users -= 1;
                            if old.users == 0 {
                                settings.remove(&self.identity);
                            }
                        }
                        if !settings.contains_key(&self.identity) {
                            crate::display_recovery::release(&self.identity)?;
                        }
                        settings
                            .entry(chosen.device_id.clone())
                            .or_insert(Settings {
                                users: 0,
                                mode: None,
                                color: None,
                            })
                            .users += 1;
                        self.identity = chosen.device_id.clone();
                    }
                }
                let restored = restore_recovered(&mut display, &self.identity, self.hdr);
                // Capture moves to the display even when its HDR or mode
                // cannot be restored yet: the old one shows nothing, so the
                // picture would freeze until the restore succeeds.
                self.output = display.name.clone();
                self.generation = display.generation;
                self.restore_pending = restored.is_err();
                if let Err(error) = restored {
                    tracing::warn!(error = %format!("{error:#}"), output = %self.output, "recovered display's HDR and mode will be restored on the next heartbeat");
                }
                return Ok(true);
            }
            if self.restore_pending {
                restore_recovered(&mut display, &self.identity, self.hdr)?;
                self.restore_pending = false;
                self.output = display.name.clone();
                return Ok(true);
            }
        }
        Ok(false)
    }
}
/// Bring a recovered virtual display's HDR state and mode back to the
/// stream's: Windows recalls what it saved for the display when it returns.
fn restore_recovered(
    display: &mut VirtualDisplay,
    identity: &str,
    hdr: Option<bool>,
) -> Result<()> {
    if let Some(hdr) = hdr {
        let chosen = monitors()?
            .into_iter()
            .find(|m| display.owns_monitor(m))
            .context("recovered display unavailable")?;
        let mut settings = SETTINGS.lock().unwrap();
        if let Some(color) = settings.get_mut(identity).and_then(|s| s.color.as_mut()) {
            color.0 = chosen.clone();
        }
        if chosen.hdr_enabled != hdr {
            crate::display_recovery::hdr(identity, &chosen.display_name, chosen.hdr_enabled, hdr)?;
            let state = settings
                .get_mut(identity)
                .context("recovered display has no settings lease")?;
            if state.color.is_none() {
                state.color = Some((chosen.clone(), chosen.hdr_enabled, hdr));
            }
            set_hdr(&chosen, hdr)?;
        }
    }
    display.finish_hotplug("after recovery settings")
}
impl Drop for Guard {
    fn drop(&mut self) {
        let mut all = SETTINGS.lock().unwrap();
        let Some(settings) = all.get_mut(&self.identity) else {
            return;
        };
        settings.users -= 1;
        if settings.users != 0 {
            return;
        }
        let settings = all.remove(&self.identity).unwrap();
        // A physical display that is off or unplugged now keeps its journal
        // entry: Windows remembers HDR and modes per display, and the next
        // stream on it or host start restores them once it is back.
        let restored = restore_settings(
            &settings,
            &self.identity,
            &self.output,
            self.virtual_display.is_some(),
            monitors().ok().as_deref(),
            set_hdr,
            |previous, applied| restore_mode_rate(&self.output, &self.identity, previous, applied),
        );
        if restored && let Err(e) = crate::display_recovery::release(&self.identity) {
            tracing::warn!(error=%e,"display recovery journal cleanup failed");
        }
    }
}

/// A monitor lease outlives a streaming session. The feeder owns only the guard,
/// so dropping the final lease can always stop and join it before removing VDD.
pub struct Retained {
    pub output: String,
    pub mode: (u32, u32, u32, bool),
    guard: std::sync::Arc<std::sync::Mutex<Option<Guard>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Retained {
    pub fn capture_target(&self) -> (String, u64) {
        self.guard.lock().unwrap().as_ref().map_or_else(
            || (self.output.clone(), 0),
            |guard| (guard.output.clone(), guard.generation),
        )
    }
    pub fn current_output(&self) -> String {
        self.guard
            .lock()
            .unwrap()
            .as_ref()
            .map_or_else(|| self.output.clone(), |g| g.output.clone())
    }
    pub fn create(id: &str, width: u32, height: u32, fps: u32, hdr: bool) -> Result<Self> {
        Self::create_rate(
            id,
            width,
            height,
            butterpollo_core::framegen::Rate(fps.checked_mul(1000).context("refresh overflow")?),
            hdr,
        )
    }
    pub fn create_rate(
        id: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
        hdr: bool,
    ) -> Result<Self> {
        Self::create_options(
            id,
            width,
            height,
            rate,
            hdr,
            &VirtualOptions::default(),
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn create_options(
        id: &str,
        width: u32,
        height: u32,
        rate: butterpollo_core::framegen::Rate,
        hdr: bool,
        options: &VirtualOptions,
        on_recovery: Option<Box<dyn Fn() -> Result<()> + Send>>,
    ) -> Result<Self> {
        let guard = Guard::new_virtual_options(
            "",
            true,
            id,
            width,
            height,
            rate,
            Some(hdr),
            None,
            None,
            options,
        )?;
        let output = guard.output.clone();
        let guard = std::sync::Arc::new(std::sync::Mutex::new(Some(guard)));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_guard = guard.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("monitor-lease".into())
            .spawn(move || {
                let mut pending = false;
                let mut retry_at = Instant::now();
                while !worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                    // Windows refuses display configuration from the normal
                    // desktop while it is locked.
                    crate::input::keep_on_input_desktop();
                    let result = worker_guard
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(Guard::feed)
                        .transpose();
                    match result {
                        Ok(Some(true)) => {
                            pending = true;
                            retry_at = Instant::now();
                        }
                        Err(e) => {
                            tracing::warn!(error=%e,"retained monitor lease heartbeat failed")
                        }
                        _ => {}
                    }
                    if pending && Instant::now() >= retry_at {
                        retry_at = Instant::now() + Duration::from_secs(1);
                        match on_recovery.as_ref().map(|callback| callback()).transpose() {
                            Ok(applied) => {
                                pending = false;
                                // Applying the layout can recall a saved mode.
                                if applied.is_some()
                                    && let Some(guard) = worker_guard.lock().unwrap().as_ref()
                                {
                                    guard.apply_virtual_mode("after layout recovery");
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "retained monitor layout recovery failed")
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            })?;
        Ok(Self {
            output,
            mode: (width, height, rate.0, hdr),
            guard,
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for Retained {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.guard.lock().unwrap().take();
    }
}

#[cfg(test)]
mod tests {
    use super::super::monitor;
    use super::*;
    #[test]
    fn only_a_displays_sole_stream_changes_its_mode_or_hdr() {
        let (current, requested) = ((3840, 2160, 60000), (1920, 1080, 120000));
        assert_eq!(plan(1, None, &current, &requested), Plan::Change);
        assert_eq!(plan(1, None, &current, &current), Plan::Keep);
        // The first stream needed no change: the second keeps its mode.
        assert_eq!(plan(2, None, &current, &requested), Plan::Keep);
        assert_eq!(plan(2, Some(&current), &current, &requested), Plan::Keep);
        assert_eq!(plan(2, None, &false, &true), Plan::Keep);
        assert_eq!(plan(1, None, &false, &true), Plan::Change);
    }
    #[test]
    fn a_pending_original_comes_back_rather_than_being_cleared() -> Result<()> {
        // A stream turned the TV's HDR on and ended while it was in standby:
        // the journal kept the entry and the TV came back with HDR on.
        let directory = tempfile::tempdir()?;
        let journal = directory.path().join("display-recovery.json");
        std::fs::write(
            &journal,
            serde_json::to_vec(&serde_json::json!({
                "pid": 0,
                "started": 0,
                "entries": {"tv": {"output": r"\\.\DISPLAY1", "hdr": [false, true]}},
            }))?,
        )?;
        let pending = crate::display_recovery::pending_in(&journal, "tv")?;
        // The next stream wants HDR too, so it changes nothing.
        let mut settings = Settings {
            users: 1,
            mode: None,
            color: None,
        };
        let unchanged = |_: &Monitor, _| -> Result<()> { panic!("HDR is already on") };
        lease_hdr(
            &mut settings,
            &monitor("tv", 1, true),
            Some(true),
            pending.hdr,
            |_, _| panic!("nothing to journal"),
            unchanged,
        )?;
        let no_mode = |_, _| -> Result<()> { panic!("no mode recorded") };
        // Away again at its end: the entry stays for recovery.
        assert!(!restore_settings(
            &settings,
            "tv",
            "TV",
            false,
            Some(&[]),
            unchanged,
            no_mode
        ));
        assert_eq!(
            crate::display_recovery::pending_in(&journal, "tv")?,
            pending
        );
        // Present: the original comes back, then the entry is released.
        let mut restored = None;
        assert!(restore_settings(
            &settings,
            "tv",
            "TV",
            false,
            Some(&[monitor("tv", 2, true)]),
            |_, enabled| {
                restored = Some(enabled);
                Ok(())
            },
            no_mode
        ));
        assert_eq!(restored, Some(false));
        crate::display_recovery::release_in(&journal, "tv")?;
        assert_eq!(
            crate::display_recovery::pending_in(&journal, "tv")?,
            Default::default()
        );
        // Without a pending entry a stream that changes nothing records nothing.
        let mut settings = Settings {
            users: 1,
            mode: None,
            color: None,
        };
        lease_hdr(
            &mut settings,
            &monitor("tv", 1, true),
            Some(true),
            None,
            |_, _| panic!("nothing to journal"),
            unchanged,
        )?;
        assert!(settings.color.is_none());
        Ok(())
    }
    #[test]
    fn a_stream_that_changes_a_setting_restores_the_pending_original() -> Result<()> {
        // The TV was left in HDR; this stream wants SDR.
        let mut settings = Settings {
            users: 1,
            mode: None,
            color: None,
        };
        let mut journaled = None;
        let mut set = None;
        lease_hdr(
            &mut settings,
            &monitor("tv", 1, true),
            Some(false),
            Some(false),
            |before, applied| {
                journaled = Some((before, applied));
                Ok(())
            },
            |_, enabled| {
                set = Some(enabled);
                Ok(())
            },
        )?;
        assert_eq!((journaled, set), (Some((true, false)), Some(false)));
        let (_, previous, applied) = settings.color.unwrap();
        assert_eq!((previous, applied), (false, false));
        // Left as it is: the original wins, unless the display has it.
        assert_eq!(adopt(Some(false), true), Some((false, true)));
        assert_eq!(adopt(Some(false), false), None);
        assert_eq!(adopt(None, true), None);
        Ok(())
    }
    #[test]
    fn previous_golden_snapshot_import_preserves_fractional_rates_clones_hdr_and_origins() {
        let document = serde_json::json!({
            "topology":[["a","b"]],"primary":"a",
            "modes":{"a":{"w":1920,"h":1080,"num":60000,"den":1001},"b":{"w":1920,"h":1080,"num":60000,"den":1001}},
            "hdr":{"a":"on","b":"off"}, "origins":{"a":{"x":-1920,"y":0},"b":{"x":-1920,"y":0}},
            "layouts":{"a":{"rotation":90},"b":{"rotation":0}}
        });
        let snapshot = Snapshot::decode(&document).unwrap();
        assert!((snapshot.nodes[0].mode.refresh_hz - 59.94005994).abs() < 1e-8);
        assert_eq!(snapshot.nodes[0].desired_position.x, -1920);
        assert!(snapshot.nodes[0].primary);
        assert_eq!(
            snapshot.clone_groups,
            [vec!["a".to_owned(), "b".to_owned()]]
        );
        assert!(snapshot.hdr["a"]);
        assert!(!snapshot.hdr["b"]);
        assert_eq!(snapshot.rotation["a"], 2);
        let mut malformed = document.clone();
        malformed["modes"]["a"]["den"] = 0.into();
        assert!(Snapshot::decode(&malformed).is_err());
        assert!(Snapshot::decode(&serde_json::json!({"version":2,"nodes":[],"hdr":{}})).is_err());
    }
}
