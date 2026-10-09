//! Prepare display and game integrations before running application commands.
//! The same leases move from an authorized pending launch into its transport.
use crate::state::{Launch, Shared};
use anyhow::{Context, Result};
use butterpollo_core::{
    config::Config,
    framegen::{Policy, Rate},
    rtsp::Negotiated,
    session::{Preparation, Role, Warnings},
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

fn stream_mode(stream: &Negotiated) -> (u32, u32, u32, bool, bool) {
    (
        stream.width,
        stream.height,
        stream.fps_millihz(),
        stream.hdr,
        stream.vrr_low_latency,
    )
}

fn virtual_display_mode<'a>(
    config: &'a Config,
    client: &'a butterpollo_core::state::Client,
    app: Option<&'a butterpollo_core::state::App>,
    windows_11: bool,
) -> &'a str {
    client
        .extra
        .get("virtual_display_mode")
        .and_then(serde_json::Value::as_str)
        // The old console stored "global" for the host setting.
        .filter(|s| !s.is_empty() && *s != "global")
        .or_else(|| {
            app.and_then(|app| app.extra.get("virtual-display-mode"))
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(config.virtual_display_mode(windows_11))
}

/// Resolution, refresh in millihertz and HDR a stream asked its display for.
type RequestedMode = (Option<(u32, u32)>, Option<u32>, Option<bool>);
/// Resolution, refresh in millihertz and HDR a display shows.
type AppliedMode = (u32, u32, u32, bool);

fn applied_mode(output: &str) -> Result<AppliedMode> {
    let topology = butterpollo_windows::display::Topology::query()?;
    let monitor = topology
        .monitors()
        .into_iter()
        .find(|m| m.matches(output))
        .context("selected display missing after preparation")?;
    let mode = butterpollo_windows::display::mode(output)?;
    let refresh = topology.refresh(&monitor.device_id)?;
    Ok((
        mode.dmPelsWidth,
        mode.dmPelsHeight,
        refresh.0,
        monitor.hdr_enabled,
    ))
}

/// What the display shows compared with the stream's request, on the card of
/// the launch that uses the display now. A retained game display moves to
/// the next launch, which sees the last finding at once.
#[derive(Clone)]
struct ModeReport(Arc<Mutex<ModeReportState>>);
struct ModeReportState {
    warnings: Arc<Warnings>,
    requested: RequestedMode,
    stream_rate: u32,
    last: Option<std::result::Result<AppliedMode, String>>,
}
impl ModeReportState {
    fn report(&self) {
        match &self.last {
            Some(Ok(actual)) => {
                self.warnings.clear("display_verify");
                butterpollo_core::display_policy::report_mode(
                    &self.warnings,
                    self.requested,
                    *actual,
                    self.stream_rate,
                );
            }
            Some(Err(error)) => self.warnings.set("display_verify", format!("Could not verify the applied display mode ({error}); refresh and HDR may differ from the request. Check Windows display settings and reconnect.")),
            None => {}
        }
    }
}
impl ModeReport {
    fn new(warnings: Arc<Warnings>, requested: RequestedMode, stream_rate: u32) -> Self {
        Self(Arc::new(Mutex::new(ModeReportState {
            warnings,
            requested,
            stream_rate,
            last: None,
        })))
    }
    fn publish(&self, actual: Result<AppliedMode>) {
        let mut state = self.0.lock().unwrap();
        state.last = Some(actual.map_err(|error| format!("{error:#}")));
        state.report();
    }
    fn attach(&self, warnings: Arc<Warnings>) {
        let mut state = self.0.lock().unwrap();
        state.warnings = warnings;
        state.report();
    }
}

/// A display recovery's steps after the heartbeat brought the display back.
/// The layout and HDR profile are retried each second until they apply. The
/// stream's mode is set once per recovery, after the layout: a mode Windows
/// refuses is reported on the stream card, not retried against Windows.
struct Recovery {
    pending: bool,
    due: std::time::Instant,
    mode: bool,
}
impl Recovery {
    fn new() -> Self {
        Self {
            pending: false,
            due: std::time::Instant::now(),
            mode: false,
        }
    }
    fn start(&mut self, now: std::time::Instant) {
        self.pending = true;
        self.due = now;
        self.mode = true;
    }
    /// Whether to run the steps now; a failed attempt waits a second.
    fn due(&mut self, now: std::time::Instant) -> bool {
        if self.pending && now >= self.due {
            self.due = now + Duration::from_secs(1);
            return true;
        }
        false
    }
    fn take_mode(&mut self) -> bool {
        std::mem::take(&mut self.mode)
    }
    fn finish(&mut self) {
        self.pending = false;
    }
}

/// Only pending and connected streams own limiter changes. The retained game
/// display must not keep a global frame cap active after transport disconnects.
pub struct StreamPreparation {
    pub display: Arc<Ready>,
    _limiter: limiter::Lease,
}
impl Preparation<StreamPreparation> for StreamPreparation {
    fn prepared(&self) -> &Self {
        self
    }
    fn into_prepared(self: Box<Self>) -> Self {
        *self
    }
}
impl StreamPreparation {
    pub fn report_limiter(
        &self,
        warnings: &butterpollo_core::session::Warnings,
        config: &butterpollo_core::config::Config,
    ) {
        let explicit = config.boolean("frame_limiter_enable", false);
        if let Some(message) = self._limiter.warning(&self.display.framegen, explicit) {
            warnings.set("display_limiter", message);
        } else {
            warnings.clear("display_limiter");
        }
    }
}

/// A game may keep its display across transport disconnects. The heartbeat
/// owns the resources, not the Ready Arc, so final teardown cannot form a cycle.
pub struct Ready {
    // Retain the leases until after the worker has joined. Streaming readers
    // use the published target, never the lock held over native display I/O.
    _state: Arc<Mutex<Prepared>>,
    target: CaptureTarget,
    mode: (u32, u32, u32, bool, bool),
    framegen: Policy,
    mode_report: ModeReport,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Ready {
    pub fn prepare((prepared, limiter): (Prepared, limiter::Lease)) -> Result<StreamPreparation> {
        let target = CaptureTarget::new(prepared.capture_target());
        let mode = prepared.mode;
        let framegen = prepared.framegen.clone();
        let mode_report = prepared.mode_report.clone();
        let state = Arc::new(Mutex::new(prepared));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_target = target.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::Builder::new()
            .name("game-display-lease".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    // Windows refuses display configuration from the normal
                    // desktop while it is locked (issue #6).
                    butterpollo_windows::input::keep_on_input_desktop();
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
        let display = Arc::new(Self {
            _state: state,
            target,
            mode,
            framegen,
            mode_report,
            stop,
            worker: Some(worker),
        });
        Ok(StreamPreparation {
            display,
            _limiter: limiter,
        })
    }
    pub fn resume(
        self: Arc<Self>,
        directory: &std::path::Path,
        config: &Config,
        warnings: Arc<Warnings>,
    ) -> Result<StreamPreparation> {
        let limiter = limiter::Lease::acquire(directory, config, &self.framegen)?;
        self.mode_report.attach(warnings);
        Ok(StreamPreparation {
            display: self,
            _limiter: limiter,
        })
    }
    pub fn matches(&self, stream: &Negotiated) -> bool {
        self.mode == stream_mode(stream)
    }
    pub fn output(&self) -> String {
        self.target.current().0
    }
    pub fn capture_target(&self) -> (String, u64) {
        self.target.current()
    }
    pub fn capture(&self) -> String {
        self.framegen.capture.clone()
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
    _vulkan: Option<vulkan::Lease>,
    _golden: Option<GoldenLease>,
    mode: (u32, u32, u32, bool, bool),
    revision: u64,
    recovery: Recovery,
    layout_watch: butterpollo_core::display_policy::LayoutWatch,
    mode_report: ModeReport,
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
        // A heartbeat step that keeps failing, such as restoring a recovered
        // display's HDR, must not hold back the layout recovery below.
        let fed = self.display.as_mut().map_or(Ok(false), Guard::feed);
        if let (Ok(true), Some(display)) = (&fed, &self.display) {
            self.output = display.output.clone();
            self.revision = self.revision.wrapping_add(1);
            self.recovery.start(std::time::Instant::now());
        }
        // A remote monitor's own lease recreates its display; the layout and
        // HDR profile this stream applied follow the new one.
        if let Some(retained) = &self._retained {
            let (output, generation) = retained.capture_target();
            if generation != self.revision {
                self.output = output;
                self.revision = generation;
                self.recovery.start(std::time::Instant::now());
            }
        }
        if self.recovery.due(std::time::Instant::now()) {
            // A remote monitor's scale is set by its lease.
            if self._retained.is_none() {
                butterpollo_windows::display::virtual_scale(
                    &self.output,
                    self.recovery_scale,
                    self.recovery_dimensions.0,
                    self.recovery_dimensions.1,
                )?;
            }
            if let Some(arrangement) = &self._arrangement {
                arrangement.reapply(&self.output, &self.remote_monitors())?;
            }
            if self.recovery.take_mode() {
                if let Some(display) = &self.display {
                    display.apply_virtual_mode("after recovery layout");
                }
                self.mode_report.publish(applied_mode(&self.output));
            }
            if let Some(profile) = &self.recovery_profile {
                self._profile.take();
                self._profile = Some(hdr_profile::Lease::acquire(&self.output, profile)?);
            }
            self.recovery.finish();
        } else {
            self.keep_layout()?;
        }
        fed.map(|_| ())
    }
    fn remote_monitors(&self) -> Vec<String> {
        self.host
            .upgrade()
            .map(|h| {
                h.monitors
                    .lock()
                    .unwrap()
                    .values()
                    .map(|m| m.current_output())
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Put the stream's layout back when Windows switches on a display it
    /// turned off. An exclusive-fullscreen game losing focus (the Win key,
    /// Alt+Tab) makes Windows recall its saved layout, which has the physical
    /// monitor on; the virtual display stays on, so the recovery above never
    /// starts.
    fn keep_layout(&mut self) -> Result<()> {
        let now = std::time::Instant::now();
        if self.recovery.pending || !self.layout_watch.due(now) {
            return Ok(());
        }
        let Some(arrangement) = &self._arrangement else {
            return Ok(());
        };
        let switched_on = match arrangement.switched_back_on() {
            Ok(switched_on) => switched_on,
            Err(error) => {
                self.layout_watch.failed(now);
                return Err(error);
            }
        };
        if !self.layout_watch.observe(!switched_on.is_empty(), now) {
            return Ok(());
        }
        tracing::info!(displays = ?switched_on, "displays the stream layout switched off came back on; reapplying it");
        let result = arrangement.reapply(&self.output, &self.remote_monitors());
        if result.is_err() {
            self.layout_watch.failed(now);
        }
        result
    }
    fn create(
        h: &Shared,
        launch: &Launch,
        stream: &Negotiated,
        config: &Config,
        physical_only: bool,
    ) -> Result<(Self, limiter::Lease)> {
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
        let mode = virtual_display_mode(
            config,
            &launch.client,
            app.as_ref(),
            butterpollo_windows::display::windows_11(),
        );
        let client_virtual = launch
            .options
            .get("virtualDisplay")
            .map(|value| value != "0");
        let display_request = butterpollo_core::display_policy::VirtualDisplayRequest {
            client_requested: client_virtual == Some(true),
            client_forced: launch
                .client
                .extra
                .get("always_use_virtual_display")
                .is_some_and(|v| v == true || v == "true"),
            app_requested: app.as_ref().is_some_and(|a| {
                crate::process::app_bool(a, "virtual-display", false)
                    || crate::process::app_bool(a, "virtual-screen", false)
            }),
            configured: mode != "disabled" || config.boolean("dd_activate_virtual_display", false),
            output_override,
            configured_output: config.get("output_name", ""),
        };
        let virtual_mode = display_request.uses_virtual(
            physical_only,
            butterpollo_windows::display::virtual_display_available,
        );
        if physical_only && display_request.requested() {
            launch.warnings.set("display_virtual", "Windows was locked and the virtual display could not be set up there, so this stream shows the physical display. Start the stream again after signing in to use the virtual display.");
        } else if display_request.requested() && !virtual_mode {
            launch.warnings.set("display_virtual", "Using a physical display because the virtual display driver is unavailable. The desktop is visible locally and its refresh can limit fresh frames; repair the virtual display driver or explicitly select the physical display.");
        } else {
            launch.warnings.clear("display_virtual");
        }
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
        // A device's own display mode replaces the host's resolution and
        // refresh policies for its display; the stream keeps the rate the
        // client asked for.
        let device_mode = launch
            .client
            .extra
            .get("display_mode")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .and_then(|text| {
                let mode = butterpollo_core::display_policy::parse_display_mode(text);
                if mode.is_none() {
                    tracing::warn!(
                        display_mode = text,
                        "ignoring a device display mode that is not WIDTHxHEIGHTxREFRESH"
                    );
                }
                mode
            });
        let mut framegen = Policy::resolve(
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
        )?;
        if let Some((_, _, rate)) = device_mode {
            // A saved display mode must not cap a faster stream's game at its refresh rate.
            framegen.display_rate = butterpollo_core::framegen::display_rate(
                rate,
                virtual_mode,
                butterpollo_core::framegen::virtual_refresh(config),
            );
        }
        framegen = framegen.with_vrr(
            config,
            virtual_mode,
            stream.vrr_low_latency || launch.vrr_requested,
        );
        // Lossless Scaling doubles the frames: the game runs at its limit.
        if let Some(limit) = app
            .as_ref()
            .and_then(|a| serde_json::to_value(a).ok())
            .and_then(|a| {
                butterpollo_core::lossless::options(
                    &a,
                    config,
                    f64::from(stream.fps_millihz()) / 1000.,
                )
            })
            .and_then(|options| options.frame_limit)
        {
            framegen.rate = Rate(limit.saturating_mul(1000).min(1_000_000));
            framegen.enabled = true;
        }
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
        if let Some((width, height, _)) = device_mode
            && (virtual_mode || config.get("dd_configuration_option", "verify_only") != "disabled")
        {
            request.resolution = Some((width, height));
            request.refresh = Some(framegen.display_rate.0);
            request.prefer_highest = false;
        }
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
            && retained.is_none()
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
        // The layout to return to after the stream, taken before the virtual
        // display exists: Windows can switch other displays on or off, change
        // the primary or retime one when it arrives.
        let original = if virtual_mode && retained.is_none() && launch.role == Role::Stream {
            butterpollo_windows::display::Snapshot::capture()
                .inspect_err(|error| {
                    tracing::warn!(error = %format!("{error:#}"), "display layout before the virtual display could not be read")
                })
                .ok()
        } else {
            None
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
        // A remote monitor is a virtual display even when this device's
        // streams otherwise use a physical one, so it follows the virtual
        // display layout.
        let virtual_layout = virtual_mode || retained.is_some();
        let selection = if virtual_layout {
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
        // A remote monitor takes which displays stay on and which is primary
        // from it; the remote monitor layout places it.
        let parsed = if !matches!(launch.role, Role::Stream | Role::RemoteMonitor)
            || matches!(selection, "disabled" | "verify_only")
        {
            None
        } else {
            butterpollo_core::display_policy::Arrangement::parse(selection)
                .inspect_err(|_| {
                    butterpollo_core::config::invalid(
                        if virtual_layout {
                            "virtual_display_layout"
                        } else {
                            "dd_configuration_option"
                        },
                        selection,
                    )
                })
                .ok()
                .or(virtual_layout
                    .then_some(butterpollo_core::display_policy::Arrangement::Exclusive))
                .and_then(|parsed| {
                    if launch.role == Role::RemoteMonitor {
                        parsed.for_remote_monitor()
                    } else {
                        Some(parsed)
                    }
                })
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
                match display_arrangement::Lease::acquire(
                    &output,
                    parsed,
                    &retained,
                    virtual_mode && display.is_some(),
                    original,
                ) {
                    Ok(lease) => Some(lease),
                    // The remote monitor still works where the remote monitor
                    // layout put it.
                    Err(error) if launch.role == Role::RemoteMonitor => {
                        tracing::warn!(error = %format!("{error:#}"), layout = selection, "remote monitor layout could not be applied");
                        launch.warnings.set("display_layout", format!("The virtual display layout ({selection}) could not be applied to this remote monitor ({error:#}), so the other displays stay as they are. Check Windows display settings and reconnect."));
                        None
                    }
                    Err(error) => return Err(error),
                }
            }
            _ => None,
        };
        if let Some(display) = &display {
            // Applying the layout can recall the display's saved mode.
            display.apply_virtual_mode("after layout");
        }
        tracing::info!(
            client = %launch.client.name,
            role = ?launch.role,
            client_virtual_display = ?client_virtual,
            virtual_display_mode = mode,
            output_override = ?output_override,
            virtual_display = virtual_mode,
            output = %output,
            layout = selection,
            arrangement = ?parsed,
            requested_refresh_millihz = request.refresh.or(virtual_mode.then_some(rate.0)),
            limiter_rate = %framegen.rate,
            limiter_enabled = framegen.enabled,
            "stream display selection"
        );
        // A virtual display is created for the stream at exactly this mode.
        let requested = if virtual_mode {
            (Some((width, height)), Some(rate.0), request.hdr)
        } else {
            (request.resolution, request.refresh, request.hdr)
        };
        let mode_report = ModeReport::new(launch.warnings.clone(), requested, stream.fps_millihz());
        mode_report.publish(applied_mode(&output));
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
        // A remote monitor's display has its own generation; see `feed`.
        let revision = retained.as_ref().map_or(0, |r| r.capture_target().1);
        Ok((
            Self {
                display,
                output,
                framegen,
                _arrangement: arrangement,
                _activation: activation,
                _profile: profile,
                _retained: retained,
                _vulkan: vulkan,
                _golden: golden,
                mode: stream_mode(stream),
                revision,
                recovery: Recovery::new(),
                layout_watch: Default::default(),
                mode_report,
                recovery_scale: config.integer("dd_virtual_display_scale", 0),
                recovery_dimensions: (width, height),
                recovery_profile,
                host: Arc::downgrade(h),
            },
            limiter,
        ))
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        // A stream ending while Windows is locked restores the layout too.
        butterpollo_windows::input::on_input_desktop(|| {
            self._profile.take();
            self._arrangement.take();
            self.display.take();
            self._activation.take();
            self._golden.take();
            self._retained.take();
            self._vulkan.take();
        });
    }
}

/// Prepare a stream's display. While Windows shows the lock or sign-in
/// screen it allows display configuration only from that screen's desktop,
/// so the work moves there (issue #6). If the virtual display or its layout
/// still cannot be set up, the stream shows the physical display, as
/// Vibepollo does while Windows is locked.
pub fn prepare_stream(
    h: &Shared,
    launch: &Launch,
    stream: &Negotiated,
    config: &Config,
) -> Result<StreamPreparation> {
    let prepare = |physical_only: bool| {
        butterpollo_windows::input::on_input_desktop(|| {
            Ready::prepare(Prepared::create(h, launch, stream, config, physical_only)?)
        })
    };
    match prepare(false) {
        Err(error) if butterpollo_windows::input::secure_desktop_shown() => {
            tracing::warn!(error = %format!("{error:#}"), "stream display could not be prepared while Windows is locked; trying the physical display");
            prepare(true).map_err(|fallback| {
                tracing::warn!(error = %format!("{fallback:#}"), "physical display could not be prepared either");
                error
            })
        }
        result => result,
    }
}

/// Streams share the saved layout's restoration, like the arrangement and
/// display settings leases: the first records it for crash recovery and the
/// last restores it. One client's end must not switch off another client's
/// display or reset its mode and HDR, nor clear the journal under it.
struct Baseline<T> {
    users: usize,
    held: Option<T>,
}
impl<T> Baseline<T> {
    const fn new() -> Self {
        Self {
            users: 0,
            held: None,
        }
    }
    /// The first user records the layout; later users join it.
    fn acquire(&mut self, record: impl FnOnce() -> Result<T>) -> Result<()> {
        if self.users == 0 {
            self.held = Some(record()?);
        }
        self.users += 1;
        Ok(())
    }
    /// Only the last user restores the layout.
    fn release(&mut self, restore: impl FnOnce(T)) {
        self.users -= 1;
        if self.users == 0
            && let Some(held) = self.held.take()
        {
            restore(held);
        }
    }
}
type SavedLayout = (butterpollo_windows::display::Snapshot, Vec<String>);
static BASELINE: Mutex<Baseline<SavedLayout>> = Mutex::new(Baseline::new());

struct GoldenLease {
    host: std::sync::Weak<crate::state::Host>,
}
impl GoldenLease {
    fn new(
        h: &Shared,
        snapshot: butterpollo_windows::display::Snapshot,
        config: &Config,
    ) -> Result<Self> {
        let excluded = crate::maintenance::display_exclusions(config)?;
        BASELINE.lock().unwrap().acquire(|| {
            butterpollo_windows::display_recovery::baseline(Some((
                snapshot.clone(),
                excluded.clone(),
            )))?;
            Ok((snapshot, excluded))
        })?;
        Ok(Self {
            host: Arc::downgrade(h),
        })
    }
}
impl Drop for GoldenLease {
    fn drop(&mut self) {
        // Held through the restore, so a stream starting meanwhile records
        // its journal entry after this one is cleared, not before.
        BASELINE
            .lock()
            .unwrap()
            .release(|(snapshot, mut excluded)| {
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
                // Displays still in use stay as they are: a stream without
                // this lease, a paused game display or a launch being prepared.
                excluded.extend(butterpollo_windows::display::leased_displays());
                if let Err(error) = snapshot.restore_excluding(&excluded) {
                    tracing::warn!(%error,"saved display baseline restoration remains pending");
                } else {
                    let _ = butterpollo_windows::display_recovery::baseline(None);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::mpsc;

    #[test]
    fn virtual_display_mode_keeps_device_app_host_precedence_and_legacy_global_inheritance() {
        let config = Config::parse("virtual_display_mode=shared").unwrap();
        let mut client: butterpollo_core::state::Client =
            serde_json::from_value(serde_json::json!({"name":"Device","uuid":"device","cert":""}))
                .unwrap();
        let app = serde_json::from_value(
            serde_json::json!({"name":"Game","virtual-display-mode":"per_client"}),
        )
        .unwrap();
        assert_eq!(
            virtual_display_mode(&Config::default(), &client, None, false),
            "disabled"
        );
        assert_eq!(
            virtual_display_mode(&Config::default(), &client, None, true),
            "per_client"
        );
        assert_eq!(virtual_display_mode(&config, &client, None, true), "shared");
        assert_eq!(
            virtual_display_mode(&config, &client, Some(&app), true),
            "per_client"
        );
        client
            .extra
            .insert("virtual_display_mode".into(), "disabled".into());
        assert_eq!(
            virtual_display_mode(&config, &client, Some(&app), true),
            "disabled"
        );
        for inherited in ["global", ""] {
            client
                .extra
                .insert("virtual_display_mode".into(), inherited.into());
            assert_eq!(
                virtual_display_mode(&config, &client, Some(&app), true),
                "per_client"
            );
            assert_eq!(virtual_display_mode(&config, &client, None, true), "shared");
        }
    }

    #[test]
    fn vrr_negotiated_after_launch_requires_new_display_preparation() {
        let mut launch = Negotiated {
            fps: 116,
            ..Default::default()
        };
        let negotiated =
            Negotiated::from_sdp(b"a=x-nv-video[0].maxFPS:116\na=x-ss-video[0].vrrLowLatency:1\n")
                .unwrap();
        assert_eq!(negotiated.fps_millihz(), 116_000);
        assert_ne!(stream_mode(&launch), stream_mode(&negotiated));
        launch.vrr_low_latency = true;
        assert_eq!(stream_mode(&launch), stream_mode(&negotiated));
    }

    fn codes(warnings: &Warnings) -> Vec<String> {
        warnings.snapshot().into_iter().map(|w| w.code).collect()
    }

    #[test]
    fn a_recovered_display_mode_is_reported_on_the_card_of_the_launch_using_it() {
        let first = Arc::new(Warnings::default());
        let report = ModeReport::new(
            first.clone(),
            (Some((3840, 2160)), Some(1_000_000), Some(true)),
            116_000,
        );
        report.publish(Ok((3840, 2160, 1_000_000, true)));
        assert!(codes(&first).is_empty());
        // Windows switched the display back on at its saved 60 Hz mode and
        // refused the stream's: the card says so, the stream continues.
        report.publish(Ok((3840, 2160, 60_000, true)));
        assert_eq!(codes(&first), ["display_refresh"]);
        assert!(first.snapshot()[0].message.contains("60.000 Hz"));
        // A resumed launch sees the current finding on its own card.
        let second = Arc::new(Warnings::default());
        report.attach(second.clone());
        assert_eq!(codes(&second), ["display_refresh"]);
        report.publish(Err(anyhow::anyhow!("display unavailable")));
        assert_eq!(codes(&second), ["display_refresh", "display_verify"]);
        report.publish(Ok((3840, 2160, 1_000_000, true)));
        assert!(codes(&second).is_empty());
        // Nothing found yet: attaching reports nothing.
        let fresh = ModeReport::new(first.clone(), (None, None, None), 60_000);
        let third = Arc::new(Warnings::default());
        fresh.attach(third.clone());
        assert!(codes(&third).is_empty());
    }

    #[test]
    fn a_recovery_sets_the_mode_once_while_its_layout_is_retried() {
        let now = std::time::Instant::now();
        let mut recovery = Recovery::new();
        assert!(!recovery.due(now));
        assert!(!recovery.take_mode());
        recovery.start(now);
        assert!(recovery.due(now));
        assert!(recovery.take_mode());
        // The layout or HDR profile failed: retried a second later, but a
        // refused mode is not requested from Windows again.
        assert!(!recovery.due(now + Duration::from_millis(100)));
        assert!(recovery.due(now + Duration::from_secs(1)));
        assert!(!recovery.take_mode());
        recovery.finish();
        assert!(!recovery.due(now + Duration::from_secs(5)));
        // The next recovery sets it again.
        recovery.start(now + Duration::from_secs(6));
        assert!(recovery.due(now + Duration::from_secs(6)));
        assert!(recovery.take_mode());
    }

    #[test]
    fn the_saved_layout_is_restored_once_after_the_last_stream() {
        let mut baseline = Baseline::new();
        let journal = Cell::new(false);
        let restores = Cell::new(0);
        baseline
            .acquire(|| {
                journal.set(true);
                Ok("layout")
            })
            .unwrap();
        baseline
            .acquire(|| panic!("a second stream must join the recorded layout"))
            .unwrap();
        let restore = |layout: &str| {
            assert_eq!(layout, "layout");
            restores.set(restores.get() + 1);
            journal.set(false);
        };
        baseline.release(restore);
        assert_eq!(restores.get(), 0);
        assert!(journal.get(), "the crash journal must stay pending");
        baseline.release(restore);
        assert_eq!(restores.get(), 1);
        assert!(!journal.get());
        // A failed record leaves no user behind; the next stream records again.
        assert!(baseline.acquire(|| anyhow::bail!("journal busy")).is_err());
        baseline.acquire(|| Ok("next")).unwrap();
        baseline.release(|layout| assert_eq!(layout, "next"));
    }

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
