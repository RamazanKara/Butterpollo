//! Prepare display and game integrations before running application commands.
//! The same leases move from an authorized pending launch into its transport.
use crate::state::Shared;
use anyhow::{Context, Result};
use butterpollo_core::{
    config::Config,
    framegen::{Policy, Rate},
    rtsp::Negotiated,
    session::{Launch, Role},
};
use butterpollo_windows::{
    display::{Guard, Retained},
    display_arrangement, hdr_profile, limiter, vulkan,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

/// A game may keep its display across transport disconnects. The heartbeat
/// owns the resources, not the Ready Arc, so final teardown cannot form a cycle.
pub struct Ready {
    // Retain the leases until after the worker has joined. Streaming readers
    // use the published target, never the lock held over native display I/O.
    _state: Arc<Mutex<Prepared>>,
    target: CaptureTarget,
    mode: (u32, u32, u32, bool),
    capture: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Ready {
    pub fn new(prepared: Prepared) -> Result<Arc<Self>> {
        let target = CaptureTarget::new(prepared.capture_target());
        let mode = prepared.mode;
        let capture = prepared.framegen.capture.clone();
        let state = Arc::new(Mutex::new(prepared));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_target = target.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("game-display-lease".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    let mut prepared = worker_state.lock().unwrap();
                    let result = prepared.feed();
                    let current = prepared.capture_target();
                    drop(prepared);
                    // Publish a recreated target even when a later restoration
                    // step needs a retry. Output and generation change together.
                    worker_target.publish(current);
                    if let Err(error) = result {
                        tracing::warn!(%error, "game display heartbeat failed");
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })?;
        Ok(Arc::new(Self {
            _state: state,
            target,
            mode,
            capture,
            stop,
            worker: Some(worker),
        }))
    }
    pub fn matches(&self, stream: &Negotiated) -> bool {
        self.mode
            == (
                stream.width,
                stream.height,
                stream.fps_millihz(),
                stream.hdr,
            )
    }
    pub fn output(&self) -> String {
        self.target.current().0
    }
    pub fn capture_target(&self) -> (String, u64) {
        self.target.current()
    }
    pub fn capture(&self) -> String {
        self.capture.clone()
    }
}

/// Only copies and publication hold this lock. Lease renewal, topology queries
/// and restoration must finish before publishing a new capture identity.
#[derive(Clone)]
struct CaptureTarget(Arc<Mutex<(String, u64)>>);
impl CaptureTarget {
    fn new(target: (String, u64)) -> Self {
        Self(Arc::new(Mutex::new(target)))
    }
    fn current(&self) -> (String, u64) {
        self.0.lock().unwrap().clone()
    }
    fn publish(&self, target: (String, u64)) {
        *self.0.lock().unwrap() = target;
    }
}
impl Drop for Ready {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct Prepared {
    pub display: Option<Guard>,
    pub output: String,
    pub framegen: Policy,
    // Restore topology while the selected monitor still exists, then remove it.
    _arrangement: Option<display_arrangement::Lease>,
    _activation: Option<display_arrangement::Activation>,
    _profile: Option<hdr_profile::Lease>,
    _retained: Option<Arc<Retained>>,
    _limiter: limiter::Lease,
    _vulkan: Option<vulkan::Lease>,
    _golden: Option<GoldenLease>,
    mode: (u32, u32, u32, bool),
    revision: u64,
    recovery_pending: bool,
    recovery_due: std::time::Instant,
    recovery_scale: i64,
    recovery_dimensions: (u32, u32),
    recovery_profile: Option<String>,
    host: std::sync::Weak<crate::state::Host>,
}
impl Prepared {
    fn capture_target(&self) -> (String, u64) {
        self._retained.as_ref().map_or_else(
            || (self.output.clone(), self.revision),
            |display| display.capture_target(),
        )
    }
    fn feed(&mut self) -> Result<()> {
        if let Some(display) = self.display.as_mut()
            && display.feed()?
        {
            self.output = display.output.clone();
            self.revision = self.revision.wrapping_add(1);
            self.recovery_pending = true;
            self.recovery_due = std::time::Instant::now();
        }
        if self.recovery_pending && std::time::Instant::now() >= self.recovery_due {
            self.recovery_due = std::time::Instant::now() + Duration::from_secs(1);
            butterpollo_windows::display::virtual_scale(
                &self.output,
                self.recovery_scale,
                self.recovery_dimensions.0,
                self.recovery_dimensions.1,
            )?;
            if let Some(arrangement) = &self._arrangement {
                let retained = self
                    .host
                    .upgrade()
                    .map(|h| {
                        h.monitors
                            .lock()
                            .unwrap()
                            .values()
                            .map(|m| m.current_output())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                arrangement.reapply(&self.output, &retained)?;
            }
            if let Some(profile) = &self.recovery_profile {
                self._profile.take();
                self._profile = Some(hdr_profile::Lease::acquire(&self.output, profile)?);
            }
            self.recovery_pending = false;
        }
        Ok(())
    }
    pub fn create(
        h: &Shared,
        launch: &Launch,
        stream: &Negotiated,
        config: &Config,
    ) -> Result<Self> {
        let app = h
            .apps
            .read()
            .unwrap()
            .iter()
            .find(|a| a.id() == launch.app_id || a.aliases.contains(&launch.app_id))
            .cloned();
        let output_override = app
            .as_ref()
            .and_then(|a| a.extra.get("display-output"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                launch
                    .client
                    .extra
                    .get("output_name_override")
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
            });
        let output = output_override
            .unwrap_or(config.get("output_name", ""))
            .trim();
        let output_virtual = matches!(
            output.to_ascii_lowercase().as_str(),
            "sunshine:virtual_display" | "virtual" | "virtual_display" | "virtual-display"
        );
        let golden = if launch.role == Role::Stream
            && config.boolean("dd_always_restore_from_golden", true)
        {
            crate::maintenance::baseline(h)?
                // A saved layout naming displays that are gone (an old
                // monitor) would switch off the ones in use; keep the
                // layout from before the stream instead.
                .filter(|snapshot| {
                    let connected = snapshot.displays_connected();
                    if !connected {
                        tracing::info!("saved display baseline names displays that are not connected; restoring the layout from before the stream");
                    }
                    connected
                })
                .map(|snapshot| GoldenLease::new(h, snapshot, config))
                .transpose()?
        } else {
            None
        };
        let option = |key: &str| {
            app.as_ref()
                .and_then(|a| a.extra.get(key))
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
        };
        let mode = launch
            .client
            .extra
            .get("virtual_display_mode")
            .and_then(serde_json::Value::as_str)
            // The old console stored "global" for the host setting.
            .filter(|s| !s.is_empty() && *s != "global")
            .or_else(|| option("virtual-display-mode"))
            .unwrap_or(config.virtual_display_mode(butterpollo_windows::display::windows_11()));
        let client_virtual = launch
            .options
            .get("virtualDisplay")
            .map(|value| value != "0");
        let explicit = client_virtual == Some(true)
            || launch
                .client
                .extra
                .get("always_use_virtual_display")
                .is_some_and(|v| v == true || v == "true")
            || app.as_ref().is_some_and(|a| {
                crate::process::app_bool(a, "virtual-display", false)
                    || crate::process::app_bool(a, "virtual-screen", false)
            });
        let inherited_virtual = explicit
            || (client_virtual != Some(false)
                && (mode != "disabled" || config.boolean("dd_activate_virtual_display", false)));
        let virtual_requested = client_virtual == Some(true)
            || match output_override {
                Some(_) => output_virtual,
                None => inherited_virtual || output_virtual,
            };
        let virtual_mode = virtual_requested
            && (explicit
                || output_virtual
                || butterpollo_windows::display::virtual_display_available());
        let generation = option("frame-generation-mode")
            .or_else(|| option("frame-generation-provider"))
            .unwrap_or("none");
        let generation_enabled = if option("frame-generation-mode").is_some() {
            butterpollo_core::framegen::generation_provider(generation) != "none"
        } else {
            app.as_ref().is_some_and(|a| {
                [
                    "frame-generation-enabled",
                    "gen1-framegen-fix",
                    "dlss-framegen-capture-fix",
                    "gen2-framegen-fix",
                    "frame-generation-capture-fix",
                ]
                .iter()
                .any(|key| crate::process::app_bool(a, key, false))
            })
        };
        let adapters = butterpollo_windows::capture::gpus()?;
        let framegen = Policy::resolve(
            config,
            Rate(stream.fps_millihz()),
            virtual_mode,
            generation,
            generation_enabled,
            adapters.iter().any(|a| a.vendor == 0x10de),
            adapters.iter().any(|a| matches!(a.vendor, 0x1002 | 0x1022)),
            launch
                .client
                .extra
                .get("config_overrides")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|o| o.contains_key("rtss_frame_limit_type"))
                || app
                    .as_ref()
                    .and_then(|a| a.extra.get("config-overrides"))
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|o| o.contains_key("rtss_frame_limit_type")),
        )?
        .with_vrr(
            config,
            virtual_mode,
            stream.vrr_low_latency || launch.vrr_requested,
        );
        let limiter = limiter::Lease::acquire(&h.directory, config, &framegen)?;
        let vulkan = if stream.hdr && config.boolean("vulkan_hdr_layer", true) {
            Some(vulkan::Lease::acquire()?)
        } else {
            None
        };
        let id = if app
            .as_ref()
            .is_some_and(|a| crate::process::app_bool(a, "use-app-identity", false))
        {
            let app_id = app
                .as_ref()
                .and_then(|a| a.extra.get("uuid"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("application");
            if app
                .as_ref()
                .is_some_and(|a| crate::process::app_bool(a, "per-client-app-identity", false))
            {
                butterpollo_core::display_policy::app_client_identity(
                    app_id,
                    launch
                        .options
                        .get("uniqueid")
                        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
                        .unwrap_or(&launch.client.uuid),
                )
            } else {
                app_id.to_owned()
            }
        } else {
            launch.client.uuid.clone()
        };
        let shared_id = if mode == "shared" {
            let mut paired = h.paired.write().unwrap();
            let existing = paired.document["root"]["shared_virtual_display_guid"]
                .as_str()
                .filter(|value| uuid::Uuid::parse_str(value).is_ok())
                .map(str::to_owned);
            if let Some(id) = existing {
                id
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                paired.document["root"]["shared_virtual_display_guid"] = id.clone().into();
                paired.save(&h.paired_path)?;
                id
            }
        } else {
            id
        };
        let stable_id = &shared_id;
        let app_scale = app
            .as_ref()
            .and_then(|a| a.extra.get("scale-factor"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(100);
        let client_scale = launch
            .options
            .get("scaleFactor")
            .and_then(|value| value.parse().ok())
            .unwrap_or(100);
        let (render_width, render_height) = butterpollo_core::display_policy::render_dimensions(
            stream.width,
            stream.height,
            client_scale,
            app_scale,
        );
        let mut request = config.display_request_rate(
            render_width,
            render_height,
            framegen.display_rate,
            stream.hdr,
            virtual_mode,
        )?;
        if request.prefer_highest && !virtual_mode {
            request.refresh =
                Some(butterpollo_windows::display::highest_refresh(output, request.resolution)?.0);
        }
        let (width, height) = if virtual_mode {
            request.resolution.unwrap_or((render_width, render_height))
        } else {
            (stream.width, stream.height)
        };
        let rate = if virtual_mode {
            Rate(request.refresh.unwrap_or(framegen.display_rate.0))
        } else {
            framegen.display_rate
        };
        let retained = if launch.role == Role::RemoteMonitor {
            Some(crate::remote_display::activate(
                h,
                &launch.client.uuid,
                stream,
            )?)
        } else {
            None
        };
        let activation = if !virtual_mode
            && matches!(
                config.get("dd_configuration_option", "verify_only"),
                "ensure_active" | "ensure_primary" | "ensure_only_display"
            ) {
            display_arrangement::Activation::acquire(output)?
        } else {
            None
        };
        let virtual_options = butterpollo_windows::display::VirtualOptions {
            label: if mode == "shared" {
                config.get("sunshine_name", "Butterpollo").into()
            } else if app
                .as_ref()
                .is_some_and(|a| crate::process::app_bool(a, "use-app-identity", false))
            {
                app.as_ref().unwrap().name.clone()
            } else {
                launch.client.name.clone()
            },
            peak_nits: config
                .integer("rtx_hdr_peak_brightness", 1000)
                .clamp(400, 2000) as u32,
        };
        let display = if retained.is_none() {
            Some(Guard::new_virtual_options(
                output,
                virtual_mode,
                stable_id,
                width,
                height,
                rate,
                request.hdr,
                request.resolution,
                request.refresh.map(Rate),
                &virtual_options,
            )?)
        } else {
            None
        };
        let output = retained
            .as_ref()
            .map(|d| d.current_output())
            .or_else(|| display.as_ref().map(|d| d.output.clone()))
            .context("display lease unavailable")?;
        if virtual_mode {
            butterpollo_windows::display::virtual_scale(
                &output,
                config.integer("dd_virtual_display_scale", 0),
                width,
                height,
            )?;
        }
        let selection = if virtual_mode {
            launch
                .client
                .extra
                .get("virtual_display_layout")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty() && *s != "global")
                .or_else(|| option("virtual-display-layout"))
                .unwrap_or(config.get("virtual_display_layout", "exclusive"))
        } else {
            config.get("dd_configuration_option", "verify_only")
        };
        let selection = if virtual_mode
            && app
                .as_ref()
                .is_some_and(|a| crate::process::app_bool(a, "virtual-display-primary", false))
        {
            "extended_primary"
        } else {
            selection
        };
        // An unknown layout falls back to the default: exclusive for a
        // virtual display, verify only (no arrangement) for a physical one.
        let parsed =
            if launch.role != Role::Stream || matches!(selection, "disabled" | "verify_only") {
                None
            } else {
                butterpollo_core::display_policy::Arrangement::parse(selection)
                    .inspect_err(|_| {
                        butterpollo_core::config::invalid(
                            if virtual_mode {
                                "virtual_display_layout"
                            } else {
                                "dd_configuration_option"
                            },
                            selection,
                        )
                    })
                    .ok()
                    .or(virtual_mode
                        .then_some(butterpollo_core::display_policy::Arrangement::Exclusive))
            };
        let arrangement = match parsed {
            Some(parsed) => {
                let retained: Vec<_> = h
                    .monitors
                    .lock()
                    .unwrap()
                    .values()
                    .map(|m| m.current_output())
                    .collect();
                Some(display_arrangement::Lease::acquire(
                    &output,
                    parsed,
                    &retained,
                    virtual_mode && display.is_some(),
                )?)
            }
            _ => None,
        };
        let recovery_profile = if stream.hdr {
            launch
                .client
                .extra
                .get("hdr_profile")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        let profile = recovery_profile
            .as_ref()
            .map(|p| hdr_profile::Lease::acquire(&output, p))
            .transpose()?;
        Ok(Self {
            display,
            output,
            framegen,
            _arrangement: arrangement,
            _activation: activation,
            _profile: profile,
            _retained: retained,
            _limiter: limiter,
            _vulkan: vulkan,
            _golden: golden,
            mode: (
                stream.width,
                stream.height,
                stream.fps_millihz(),
                stream.hdr,
            ),
            revision: 0,
            recovery_pending: false,
            recovery_due: std::time::Instant::now(),
            recovery_scale: config.integer("dd_virtual_display_scale", 0),
            recovery_dimensions: (width, height),
            recovery_profile,
            host: Arc::downgrade(h),
        })
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        self._profile.take();
        self._arrangement.take();
        self.display.take();
        self._activation.take();
        self._golden.take();
    }
}

struct GoldenLease {
    snapshot: butterpollo_windows::display::Snapshot,
    excluded: Vec<String>,
    host: std::sync::Weak<crate::state::Host>,
}
impl GoldenLease {
    fn new(
        h: &Shared,
        snapshot: butterpollo_windows::display::Snapshot,
        config: &Config,
    ) -> Result<Self> {
        let excluded = crate::maintenance::display_exclusions(config)?;
        butterpollo_windows::display_recovery::baseline(Some((
            snapshot.clone(),
            excluded.clone(),
        )))?;
        Ok(Self {
            snapshot,
            excluded,
            host: Arc::downgrade(h),
        })
    }
}
impl Drop for GoldenLease {
    fn drop(&mut self) {
        let mut excluded = self.excluded.clone();
        if let Some(host) = self.host.upgrade() {
            let outputs: Vec<_> = host
                .monitors
                .lock()
                .unwrap()
                .values()
                .map(|m| m.current_output())
                .collect();
            if let Ok(monitors) = butterpollo_windows::display::monitors() {
                excluded.extend(
                    monitors
                        .iter()
                        .filter(|m| outputs.contains(&m.display_name))
                        .map(|m| m.device_id.clone()),
                );
            }
        }
        if let Err(error) = self.snapshot.restore_excluding(&excluded) {
            tracing::warn!(%error,"saved display baseline restoration remains pending");
        } else {
            let _ = butterpollo_windows::display_recovery::baseline(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn capture_target_stays_readable_during_refresh_and_publishes_identity_together() {
        let target = CaptureTarget::new(("old-output".into(), 0));
        let publisher = target.clone();
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let refresh = std::thread::spawn(move || {
            // Stand in for a slow driver call: publication happens only after
            // maintenance returns, while current capture must remain usable.
            entered.send(()).unwrap();
            resume.recv().unwrap();
            publisher.publish(("new-output".into(), 1));
        });
        waiting.recv_timeout(Duration::from_secs(1)).unwrap();
        let reader = target.clone();
        let (read, result) = mpsc::channel();
        let read_thread = std::thread::spawn(move || read.send(reader.current()).unwrap());
        let current = result.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        refresh.join().unwrap();
        read_thread.join().unwrap();
        assert_eq!(current.unwrap(), ("old-output".into(), 0));
        assert_eq!(target.current(), ("new-output".into(), 1));

        let publisher = target.clone();
        let refresh = std::thread::spawn(move || {
            for generation in 2..10000 {
                publisher.publish((format!("output-{generation}"), generation));
            }
        });
        while !refresh.is_finished() {
            let (output, generation) = target.current();
            if generation > 1 {
                assert_eq!(output, format!("output-{generation}"));
            }
        }
        refresh.join().unwrap();
        assert_eq!(target.current(), ("output-9999".into(), 9999));
    }
}
