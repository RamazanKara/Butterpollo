//! Display decisions that do not need Windows: whether a session gets a
//! virtual display, which mode to report, stable virtual display identities,
//! render sizes, letterboxing and display arrangements.
use crate::topology::{Node, Position};
use anyhow::{Result, bail};

/// A client request can enable a virtual display; a zero/absent request leaves
/// the host, app and device settings in control (as in the GameStream host).
#[derive(Default)]
pub struct VirtualDisplayRequest<'a> {
    pub client_requested: bool,
    pub client_forced: bool,
    pub app_requested: bool,
    pub configured: bool,
    pub output_override: Option<&'a str>,
    pub configured_output: &'a str,
    /// No display is active (a headless host, or every monitor off): there
    /// is no physical display to stream, so a virtual one is used whatever
    /// the settings say, as in Vibepollo.
    pub headless: bool,
}
impl VirtualDisplayRequest<'_> {
    /// Whether the stream gets a virtual display. Without a usable driver a
    /// request from the client, the app or the host settings streams the
    /// physical display (the caller warns), as Apollo and Vibepollo do: an
    /// Artemis client asked to proceed without the driver must still get a
    /// stream. Only an output that names the virtual display itself has no
    /// physical display to fall back to.
    pub fn uses_virtual(
        &self,
        physical_only: bool,
        driver_available: impl FnOnce() -> bool,
    ) -> bool {
        !physical_only && self.requested() && (self.output_virtual() || driver_available())
    }
    pub fn output_virtual(&self) -> bool {
        matches!(
            self.output_override
                .unwrap_or(self.configured_output)
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "sunshine:virtual_display" | "virtual" | "virtual_display" | "virtual-display"
        )
    }
    pub fn requested(&self) -> bool {
        self.client_requested
            || self.client_forced
            || self.headless
            || self.output_virtual()
            || (self.output_override.is_none() && (self.app_requested || self.configured))
    }
}

/// The physical display mode to apply for a stream, given the modes the display
/// supports as (width, height, refresh Hz), the requested resolution and refresh
/// (millihertz), and the current mode.
///
/// Windows substitutes an arbitrary mode for one a display does not support:
/// a 2560x1600 request on a 32:9 monitor became 3840x1080 at 60 Hz, halving a
/// 120 fps stream. An unsupported resolution keeps the current one (the stream
/// is scaled on the GPU), and a refresh is applied only where it exists.
pub fn physical_mode(
    supported: &[(u32, u32, u32)],
    requested: (Option<(u32, u32)>, Option<u32>),
    current: (u32, u32, u32),
) -> (u32, u32, u32) {
    let (current_width, current_height, current_rate) = current;
    let hz = |millihertz: u32| millihertz.saturating_add(500) / 1000;
    let (width, height) = requested
        .0
        .filter(|(w, h)| supported.iter().any(|m| m.0 == *w && m.1 == *h))
        .unwrap_or((current_width, current_height));
    let rate = requested
        .1
        .filter(|rate| {
            supported
                .iter()
                .any(|m| m.0 == width && m.1 == height && m.2 == hz(*rate))
        })
        .or_else(|| {
            // Keep the current refresh when it exists at the chosen resolution;
            // otherwise the highest that does.
            let at = |rate: u32| {
                supported
                    .iter()
                    .any(|m| m.0 == width && m.1 == height && m.2 == hz(rate))
            };
            if at(current_rate) {
                Some(current_rate)
            } else {
                supported
                    .iter()
                    .filter(|m| m.0 == width && m.1 == height)
                    .map(|m| m.2 * 1000)
                    .max()
            }
        })
        .unwrap_or(current_rate);
    (width, height, rate)
}
pub fn report_mode(
    warnings: &crate::session::Warnings,
    requested: (Option<(u32, u32)>, Option<u32>, Option<bool>),
    actual: (u32, u32, u32, bool),
    stream_rate: u32,
) {
    if let Some((width, height)) = requested.0
        && (width, height) != (actual.0, actual.1)
    {
        warnings.set("display_resolution", format!("Display resolution {width}x{height} was not applied; using {}x{} and scaling the stream. The mode may be unsupported or shared with another stream; select a supported mode or a virtual display.", actual.0, actual.1));
    } else {
        warnings.clear("display_resolution");
    }
    if requested
        .1
        .is_some_and(|rate| rate.abs_diff(actual.2) > 500)
        || actual.2.saturating_add(500) < stream_rate
    {
        warnings.set("display_refresh", format!("Display refresh is {:.3} Hz for a {:.3} fps stream; the requested rate may be unsupported or held by another stream. Fresh frames cannot exceed the display rate. Select a supported refresh or a virtual display.", f64::from(actual.2) / 1000., f64::from(stream_rate) / 1000.));
    } else {
        warnings.clear("display_refresh");
    }
    if requested.2.is_some_and(|hdr| hdr != actual.3) {
        warnings.set("display_hdr", format!("Requested display HDR state was not applied; capture source HDR is {}. The display may lack HDR or be shared. Check Windows HDR and the display policy, or reconnect with HDR disabled.", if actual.3 { "on" } else { "off" }));
    } else {
        warnings.clear("display_hdr");
    }
}

/// Preserve libdisplaydevice's UUIDv5 identity, including its UTF-16 byte order
/// and removal of the unstable parent portion of Windows' instance ID.
pub fn legacy_device_id(path: &str, instance: Option<&str>, edid: &[u8]) -> String {
    let mut bytes = Vec::new();
    if let Some(instance) = instance {
        let separators: Vec<_> = instance
            .match_indices('&')
            .map(|(at, _)| at)
            .take(3)
            .collect();
        if separators.len() == 3 {
            bytes.extend_from_slice(edid);
            for part in [&instance[..separators[1]], &instance[separators[2]..]] {
                bytes.extend(part.encode_utf16().flat_map(u16::to_le_bytes));
            }
        }
    }
    if bytes.is_empty() {
        bytes.extend(path.encode_utf16().flat_map(u16::to_le_bytes));
    }
    format!("{{{}}}", uuid::Uuid::new_v5(&uuid::Uuid::nil(), &bytes))
}
fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}
/// Match uuid_util::parse's Windows GUID byte order and the previous host's
/// deterministic fallback domains. The driver hashes those 16 memory bytes.
pub fn virtual_identity(stable_id: &str) -> [u8; 16] {
    if let Ok(uuid) = uuid::Uuid::parse_str(stable_id) {
        return uuid.to_bytes_le();
    }
    let mut bytes = [0; 16];
    for (at, domain) in [
        (0, "sunshine-virtual-display-a:"),
        (8, "sunshine-virtual-display-b:"),
    ] {
        bytes[at..at + 8]
            .copy_from_slice(&fnv(domain.bytes().chain(stable_id.bytes())).to_le_bytes());
    }
    bytes[6] = (bytes[6] & 15) | 0x50;
    bytes[8] = (bytes[8] & 63) | 0x80;
    bytes
}
pub fn virtual_display_id(stable_id: &str) -> u64 {
    fnv(virtual_identity(stable_id)).max(1)
}
pub fn app_client_identity(app: &str, client: &str) -> String {
    let mut bytes = virtual_identity(app);
    for (byte, client_byte) in bytes.iter_mut().zip(virtual_identity(client)) {
        *byte ^= client_byte;
    }
    uuid::Uuid::from_bytes_le(bytes).to_string()
}

/// A non-default application scale takes precedence over the client's scale.
/// The previous host rounded down to even dimensions and ignored overflow.
pub fn render_dimensions(width: u32, height: u32, client_scale: i64, app_scale: i64) -> (u32, u32) {
    let scale = if app_scale != 100 {
        app_scale
    } else {
        client_scale
    };
    if scale <= 0 || scale == 100 {
        return (width, height);
    }
    let dimension = |value: u32| {
        u64::from(value)
            .checked_mul(scale as u64)
            .map(|value| (value / 100) & !1)
            .filter(|value| *value > 0 && *value <= i32::MAX as u64)
            .map(|value| value as u32)
    };
    match (dimension(width), dimension(height)) {
        (Some(width), Some(height)) => (width, height),
        _ => (width, height),
    }
}
/// Where a source of another shape sits in the stream, as `(x, y, width,
/// height)`: centred, keeping its aspect ratio, with black bars around it.
/// Sizes and offsets are even for 4:2:0 chroma. A shape within a pixel of
/// the stream's fills it, as in the GPU converter.
pub fn letterbox(source: (u32, u32), target: (u32, u32)) -> (u32, u32, u32, u32) {
    let full = (0, 0, target.0, target.1);
    if source.0 == 0 || source.1 == 0 || source == target {
        return full;
    }
    let (sw, sh) = (f64::from(source.0), f64::from(source.1));
    let (tw, th) = (f64::from(target.0), f64::from(target.1));
    let scale = (tw / sw).min(th / sh);
    let (w, h) = (sw * scale, sh * scale);
    if (w - tw).abs() < 1. && (h - th).abs() < 1. {
        return full;
    }
    let even = |value: f64, limit: u32| ((value.round() as u32) & !1).max(2).min(limit);
    let (w, h) = (even(w, target.0), even(h, target.1));
    (((target.0 - w) / 2) & !1, ((target.1 - h) / 2) & !1, w, h)
}
/// Where the display's picture sits in the stream, or None when it fills the
/// stream: a display of another shape is letterboxed (see `letterbox`).
pub fn picture(display: (u32, u32), stream: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    if stream.0 == 0 || stream.1 == 0 {
        return None;
    }
    let picture = letterbox(display, stream);
    (picture != (0, 0, stream.0, stream.1)).then_some(picture)
}
/// A point the client gives as fractions of the whole stream, black bars
/// included, as fractions of the picture, that is of the display. A point on
/// a bar moves to the picture's edge.
pub fn stream_to_picture(
    point: (f64, f64),
    stream: (u32, u32),
    picture: (u32, u32, u32, u32),
) -> (f64, f64) {
    let map = |value: f64, total: u32, offset: u32, size: u32| {
        ((value.clamp(0., 1.) * f64::from(total) - f64::from(offset)) / f64::from(size.max(1)))
            .clamp(0., 1.)
    };
    (
        map(point.0, stream.0, picture.0, picture.2),
        map(point.1, stream.1, picture.1, picture.3),
    )
}
/// A device's own display mode, `WIDTHxHEIGHTxREFRESH` (refresh in Hz, up to
/// three decimals, e.g. `1920x1080x59.94`).
pub fn parse_display_mode(text: &str) -> Option<(u32, u32, crate::framegen::Rate)> {
    let mut parts = text.trim().split(['x', 'X']);
    let width = parts.next()?.trim().parse::<u32>().ok()?;
    let height = parts.next()?.trim().parse::<u32>().ok()?;
    let rate = crate::framegen::Rate::parse(parts.next()?).ok()?;
    (parts.next().is_none()
        && (1..=16384).contains(&width)
        && (1..=16384).contains(&height)
        && rate.0 >= 1000)
        .then_some((width, height, rate))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrangement {
    Extended,
    Primary,
    Exclusive,
    Isolated,
    PrimaryIsolated,
}
impl Arrangement {
    pub fn parse(value: &str) -> Result<Self> {
        Ok(
            match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
                "extended" | "ensure_active" => Self::Extended,
                "extended_primary" | "ensure_primary" => Self::Primary,
                "exclusive" | "ensure_only_display" => Self::Exclusive,
                "extended_isolated" => Self::Isolated,
                "extended_primary_isolated" => Self::PrimaryIsolated,
                _ => bail!("unknown display arrangement"),
            },
        )
    }
    /// The layout a remote monitor's stream applies. Its position on the
    /// desktop comes from the remote monitor layout, so it takes only which
    /// displays stay on and which is primary: exclusive turns the others off,
    /// the primary layouts make it primary, and the extended and isolated
    /// placements leave it where the remote monitor layout put it.
    pub const fn for_remote_monitor(self) -> Option<Self> {
        match self {
            Self::Exclusive => Some(Self::Exclusive),
            Self::Primary | Self::PrimaryIsolated => Some(Self::Primary),
            Self::Extended | Self::Isolated => None,
        }
    }
    pub fn compose(self, nodes: &[Node], target: &str, retained: &[String]) -> Result<Vec<Node>> {
        let target_node = nodes
            .iter()
            .find(|n| n.device_id == target)
            .ok_or_else(|| anyhow::anyhow!("display arrangement target is missing"))?;
        let mut result = nodes.to_vec();
        if self == Self::Exclusive {
            for node in &mut result {
                node.active = node.device_id == target || retained.contains(&node.device_id);
            }
        }
        let primary = matches!(
            self,
            Self::Primary | Self::Exclusive | Self::PrimaryIsolated
        );
        if primary {
            for node in &mut result {
                node.primary = node.device_id == target;
                node.desired_position.x = node
                    .desired_position
                    .x
                    .checked_sub(target_node.desired_position.x)
                    .ok_or_else(|| anyhow::anyhow!("display position overflow"))?;
                node.desired_position.y = node
                    .desired_position
                    .y
                    .checked_sub(target_node.desired_position.y)
                    .ok_or_else(|| anyhow::anyhow!("display position overflow"))?;
            }
        }
        if self == Self::Isolated {
            result
                .iter_mut()
                .find(|n| n.device_id == target)
                .unwrap()
                .desired_position = Position { x: 64000, y: 64000 };
        }
        if self == Self::PrimaryIsolated {
            let min_x = result
                .iter()
                .filter(|n| n.device_id != target)
                .map(|n| n.desired_position.x)
                .min()
                .unwrap_or(0);
            let min_y = result
                .iter()
                .filter(|n| n.device_id != target)
                .map(|n| n.desired_position.y)
                .min()
                .unwrap_or(0);
            for node in result.iter_mut().filter(|n| n.device_id != target) {
                node.desired_position.x = 64000i32
                    .checked_add(
                        node.desired_position
                            .x
                            .checked_sub(min_x)
                            .ok_or_else(|| anyhow::anyhow!("display position overflow"))?,
                    )
                    .ok_or_else(|| anyhow::anyhow!("display position overflow"))?;
                node.desired_position.y = 64000i32
                    .checked_add(
                        node.desired_position
                            .y
                            .checked_sub(min_y)
                            .ok_or_else(|| anyhow::anyhow!("display position overflow"))?,
                    )
                    .ok_or_else(|| anyhow::anyhow!("display position overflow"))?;
            }
        }
        Ok(result)
    }
    /// One layout for several streams. The first target is laid out as for a
    /// single stream; each later target stays active and is placed to the right
    /// of the streamed displays, so no two of them overlap. An isolated layout
    /// keeps the streamed displays next to each other, away from the others.
    pub fn compose_all(
        self,
        nodes: &[Node],
        targets: &[String],
        retained: &[String],
    ) -> Result<Vec<Node>> {
        let (first, rest) = targets
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("display arrangement has no target"))?;
        let mut keep = retained.to_vec();
        keep.extend(rest.iter().cloned());
        let mut result = self.compose(nodes, first, &keep)?;
        let isolated = matches!(self, Self::Isolated | Self::PrimaryIsolated);
        let mut placed = vec![first.clone()];
        for target in rest {
            if !result.iter().any(|n| n.device_id == *target) {
                bail!("display arrangement target is missing");
            }
            let y = result
                .iter()
                .find(|n| n.device_id == *first)
                .map_or(0, |n| n.desired_position.y);
            let right = result
                .iter()
                .filter(|n| {
                    n.active
                        && if isolated {
                            placed.contains(&n.device_id)
                        } else {
                            !rest.contains(&n.device_id) || placed.contains(&n.device_id)
                        }
                })
                .map(|n| {
                    n.desired_position
                        .x
                        .saturating_add(i32::try_from(n.mode.width).unwrap_or(i32::MAX))
                })
                .max()
                .unwrap_or(0);
            let node = result
                .iter_mut()
                .find(|n| n.device_id == *target)
                .expect("checked above");
            node.active = true;
            node.primary = false;
            node.desired_position = Position { x: right, y };
            placed.push(target.clone());
        }
        Ok(result)
    }
}
/// Displays a stream's layout switched off that are on again. Windows puts
/// back the layout it has saved for the connected displays when an
/// exclusive-fullscreen game loses focus (the Win key, Alt+Tab, Ctrl+Alt+Del),
/// and that layout has the physical monitor on. `before`: the user's layout
/// from before the stream; `applied`: the stream's; `active`: the displays on
/// now. A display that was already off before the stream is the user's to
/// switch on, and one the stream never laid out (another client's display on
/// its way in) is not counted.
pub fn switched_back_on(before: &[Node], applied: &[Node], active: &[String]) -> Vec<String> {
    applied
        .iter()
        .filter(|n| !n.active && active.contains(&n.device_id))
        .filter(|n| {
            before
                .iter()
                .any(|b| b.device_id == n.device_id && b.active)
        })
        .map(|n| n.device_id.clone())
        .collect()
}

/// When the heartbeat checks [`switched_back_on`] and puts the stream's layout
/// back. A display must stay on for [`LayoutWatch::SETTLE`] first, so a game's
/// own mode switch passes without a fight; a failed attempt (the secure
/// desktop refuses display changes) waits [`LayoutWatch::RETRY`].
#[derive(Debug, Default)]
pub struct LayoutWatch {
    next_check: Option<std::time::Instant>,
    since: Option<std::time::Instant>,
}
impl LayoutWatch {
    pub const INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
    pub const SETTLE: std::time::Duration = std::time::Duration::from_millis(500);
    pub const RETRY: std::time::Duration = std::time::Duration::from_secs(2);
    /// Whether to query the displays now.
    pub fn due(&mut self, now: std::time::Instant) -> bool {
        if self.next_check.is_some_and(|next| now < next) {
            return false;
        }
        self.next_check = Some(now + Self::INTERVAL);
        true
    }
    /// Record a check; true when the layout should be put back now.
    pub fn observe(&mut self, switched_on: bool, now: std::time::Instant) -> bool {
        if !switched_on {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if now.duration_since(since) < Self::SETTLE {
            return false;
        }
        self.since = None;
        true
    }
    pub fn failed(&mut self, now: std::time::Instant) {
        self.since = None;
        self.next_check = Some(now + Self::RETRY);
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn virtual_opt_in_preserves_host_policy_and_device_force_overrides_physical_output() {
        use super::VirtualDisplayRequest;
        let host = VirtualDisplayRequest {
            configured: true,
            ..Default::default()
        };
        assert!(host.uses_virtual(false, || true));
        let physical_app = VirtualDisplayRequest {
            output_override: Some("physical-monitor"),
            ..host
        };
        assert!(!physical_app.requested());
        let forced = VirtualDisplayRequest {
            client_forced: true,
            ..physical_app
        };
        assert!(forced.uses_virtual(false, || true));
        let opt_in = VirtualDisplayRequest {
            client_requested: true,
            client_forced: false,
            ..forced
        };
        assert!(opt_in.uses_virtual(false, || true));
        assert!(!opt_in.uses_virtual(true, || panic!(
            "physical fallback must not query the driver"
        )));
    }
    #[test]
    fn a_requested_virtual_display_without_its_driver_streams_the_physical_display() {
        use super::VirtualDisplayRequest;
        let client = VirtualDisplayRequest {
            client_requested: true,
            ..Default::default()
        };
        assert!(client.uses_virtual(false, || true));
        assert!(!client.uses_virtual(false, || false));
        assert!(!client.uses_virtual(true, || true));
        for request in [
            VirtualDisplayRequest {
                client_forced: true,
                ..Default::default()
            },
            VirtualDisplayRequest {
                app_requested: true,
                ..Default::default()
            },
            VirtualDisplayRequest {
                configured: true,
                ..Default::default()
            },
        ] {
            assert!(request.uses_virtual(false, || true));
            assert!(!request.uses_virtual(false, || false));
        }
        let named = VirtualDisplayRequest {
            configured_output: "virtual_display",
            ..Default::default()
        };
        assert!(named.uses_virtual(false, || false));
        let none = VirtualDisplayRequest::default();
        assert!(!none.uses_virtual(false, || panic!("no request needs no driver check")));
        // With no active display there is nothing physical to stream, even
        // when the settings and the app name a physical display.
        let headless = VirtualDisplayRequest {
            headless: true,
            output_override: Some("physical-monitor"),
            ..Default::default()
        };
        assert!(headless.uses_virtual(false, || true));
        assert!(!headless.uses_virtual(false, || false));
        assert!(!headless.uses_virtual(true, || true));
    }
    #[test]
    fn unapplied_display_modes_are_visible_and_recovery_clears_them() {
        let warnings = crate::session::Warnings::default();
        let requested = (Some((3840, 2160)), Some(116_000), Some(true));
        super::report_mode(&warnings, requested, (1920, 1080, 60_000, false), 116_000);
        let entries = warnings.snapshot();
        assert_eq!(
            entries.iter().map(|w| w.code.as_str()).collect::<Vec<_>>(),
            ["display_hdr", "display_refresh", "display_resolution"]
        );
        assert!(entries[1].message.contains("60.000 Hz"));
        super::report_mode(&warnings, requested, (3840, 2160, 116_000, true), 116_000);
        assert!(warnings.snapshot().is_empty());
        super::report_mode(
            &warnings,
            (None, None, None),
            (3840, 2160, 59_940, false),
            60_000,
        );
        assert!(warnings.snapshot().is_empty());
        super::report_mode(
            &warnings,
            (None, None, None),
            (3840, 2160, 60_000, false),
            116_000,
        );
        assert_eq!(warnings.snapshot()[0].code, "display_refresh");
    }
    use super::*;
    #[test]
    fn other_shapes_are_centred_with_bars() {
        // The 32:9 desktop on a 16:10 tablet.
        assert_eq!(letterbox((5120, 1440), (2560, 1600)), (0, 440, 2560, 720));
        // A 16:9 source on a tall phone stream.
        assert_eq!(letterbox((1920, 1080), (1968, 2184)), (0, 538, 1968, 1106));
        assert_eq!(letterbox((3840, 2160), (1920, 1080)), (0, 0, 1920, 1080));
        // Within a pixel of the stream's shape fills it.
        assert_eq!(letterbox((1921, 1080), (1920, 1080)), (0, 0, 1920, 1080));
        assert_eq!(letterbox((0, 0), (1920, 1080)), (0, 0, 1920, 1080));
    }
    #[test]
    fn touch_on_a_letterboxed_stream_lands_on_the_picture_not_the_bars() {
        // A 16:9 display on a 4:3 iPad stream: bars above and below.
        let ipad = picture((2560, 1440), (2732, 2048)).unwrap();
        assert_eq!(ipad, (0, 256, 2732, 1536));
        let at = |x: f64, y: f64| stream_to_picture((x, y), (2732, 2048), ipad);
        assert_eq!(at(0., 256. / 2048.), (0., 0.));
        assert_eq!(at(1., 1792. / 2048.), (1., 1.));
        assert_eq!(at(0.5, 0.5), (0.5, 0.5));
        // On a bar: the picture's edge.
        assert_eq!(at(0.25, 0.), (0.25, 0.));
        assert_eq!(at(0.25, 1.), (0.25, 1.));
        // A display of the stream's shape fills it: nothing to map.
        assert_eq!(picture((3840, 2160), (1920, 1080)), None);
        assert_eq!(picture((1920, 1080), (0, 0)), None);
    }
    #[test]
    fn device_display_modes_parse_like_vibepollo() {
        use crate::framegen::Rate;
        assert_eq!(
            parse_display_mode("1920x1080x59.94"),
            Some((1920, 1080, Rate(59940)))
        );
        assert_eq!(
            parse_display_mode(" 2560X1600x120 "),
            Some((2560, 1600, Rate(120000)))
        );
        for bad in [
            "",
            "1920x1080",
            "1920x1080x0",
            "0x1080x60",
            "1920x1080x60x1",
            "axbxc",
        ] {
            assert_eq!(parse_display_mode(bad), None, "{bad}");
        }
    }
    #[test]
    fn unsupported_physical_resolutions_keep_the_current_mode_and_rate() {
        let odyssey = [
            (5120, 1440, 240),
            (5120, 1440, 120),
            (5120, 1440, 60),
            (3840, 1080, 60),
            (2560, 1440, 120),
            (2560, 1440, 60),
        ];
        let current = (5120, 1440, 240_000);
        // The tablet case: no 2560x1600 mode; keep 5120x1440, use 120 Hz.
        assert_eq!(
            physical_mode(&odyssey, (Some((2560, 1600)), Some(120_000)), current),
            (5120, 1440, 120_000)
        );
        // A supported request is applied as asked.
        assert_eq!(
            physical_mode(&odyssey, (Some((2560, 1440)), Some(120_000)), current),
            (2560, 1440, 120_000)
        );
        // A refresh the chosen resolution lacks keeps the current one.
        assert_eq!(
            physical_mode(&odyssey, (None, Some(144_000)), current),
            (5120, 1440, 240_000)
        );
        // A fractional request is kept when the nearest whole-hertz mode exists.
        assert_eq!(
            physical_mode(&odyssey, (None, Some(119_880)), current),
            (5120, 1440, 119_880)
        );
        // A resolution that only lacks the current refresh takes its highest.
        assert_eq!(
            physical_mode(&odyssey, (Some((3840, 1080)), Some(120_000)), current),
            (3840, 1080, 60_000)
        );
        assert_eq!(
            physical_mode(&[], (Some((1, 1)), Some(60_000)), current),
            current
        );
    }
    use crate::topology::{Kind, Mode};
    #[test]
    fn previous_virtual_driver_identity_vectors_use_windows_guid_bytes() {
        assert_eq!(
            virtual_display_id("f773d31b-43da-470c-80d5-02e777a6d993"),
            0xed9b7be9fc3f0b82
        );
        assert_eq!(virtual_display_id("example-client"), 0xbce6a166cf4c68bc);
        assert_eq!(
            virtual_identity("example-client"),
            *uuid::Uuid::parse_str("f12b4b23-a6df-5a22-8eed-f5aaef994ef8")
                .unwrap()
                .as_bytes()
        );
        let combined = app_client_identity(
            "f773d31b-43da-470c-80d5-02e777a6d993",
            "d9428888-122b-11e1-b85c-61cd3cbb3210",
        );
        assert_eq!(
            app_client_identity(&combined, "d9428888-122b-11e1-b85c-61cd3cbb3210"),
            "f773d31b-43da-470c-80d5-02e777a6d993"
        );
    }
    #[test]
    fn libdisplaydevice_uuid_vectors_survive_the_unstable_driver_instance_counter() {
        let path = r"\\?\DISPLAY#ACI27EC#5&4FD2DE4&5&UID4352";
        let edid: Vec<u8> = (0..128).collect();
        let expected = "{ccbebcd3-0583-5b71-92e4-6e28da2e07ff}";
        assert_eq!(
            legacy_device_id(path, Some(r"DISPLAY\ACI27EC\5&4FD2DE4&5&UID4352"), &edid),
            expected
        );
        assert_eq!(
            legacy_device_id(path, Some(r"DISPLAY\ACI27EC\5&4FD2DE4&99&UID4352"), &edid),
            expected
        );
        assert_eq!(
            legacy_device_id(path, None, &[]),
            "{3355dc9e-a978-5cee-9a57-36824dcba1e6}"
        );
        assert_ne!(
            legacy_device_id(path, Some(r"DISPLAY\ACI27EC\5&4FD2DE4&5&UID4353"), &edid),
            expected
        );
    }
    #[test]
    fn retained_render_scale_precedence_and_odd_resolution_vectors() {
        assert_eq!(render_dimensions(1920, 1080, 150, 100), (2880, 1620));
        assert_eq!(render_dimensions(1920, 1080, 150, 200), (3840, 2160));
        assert_eq!(render_dimensions(1365, 767, 75, 100), (1022, 574));
        assert_eq!(render_dimensions(1920, 1080, -1, 100), (1920, 1080));
        assert_eq!(render_dimensions(1920, 1080, i64::MAX, 100), (1920, 1080));
    }
    fn node(id: &str, x: i32) -> Node {
        Node {
            id: id.into(),
            label: id.into(),
            device_id: id.into(),
            kind: Kind::Physical,
            active: true,
            primary: x == 0,
            desired_position: Position { x, y: 0 },
            mode: Mode {
                width: 1920,
                height: 1080,
                refresh_hz: 60.,
            },
        }
    }
    #[test]
    fn several_streams_share_one_layout_without_overlapping() {
        let mut second = node("second", 1920);
        second.mode.width = 1968;
        let nodes = vec![node("physical", 0), node("first", 1920), second];
        let targets = ["first".to_owned(), "second".to_owned()];
        let at = |nodes: &[Node], id: &str| {
            let n = nodes.iter().find(|n| n.device_id == id).unwrap();
            (
                n.active,
                n.primary,
                n.desired_position.x,
                n.desired_position.y,
            )
        };
        // Exclusive: only the streamed displays stay on; the first is primary.
        let exclusive = Arrangement::Exclusive
            .compose_all(&nodes, &targets, &[])
            .unwrap();
        assert!(!at(&exclusive, "physical").0);
        assert_eq!(at(&exclusive, "first"), (true, true, 0, 0));
        assert_eq!(at(&exclusive, "second"), (true, false, 1920, 0));
        // Extended: the second display goes right of everything active.
        let extended = Arrangement::Extended
            .compose_all(&nodes, &targets, &[])
            .unwrap();
        assert_eq!(at(&extended, "physical"), (true, true, 0, 0));
        assert_eq!(at(&extended, "first"), (true, false, 1920, 0));
        assert_eq!(at(&extended, "second"), (true, false, 3840, 0));
        // Isolated: streamed displays sit next to each other, away from the rest.
        let isolated = Arrangement::Isolated
            .compose_all(&nodes, &targets, &[])
            .unwrap();
        assert_eq!(at(&isolated, "first"), (true, false, 64000, 64000));
        assert_eq!(at(&isolated, "second"), (true, false, 65920, 64000));
        assert_eq!(at(&isolated, "physical"), (true, true, 0, 0));
        // One target is the single-stream layout.
        let single = Arrangement::Exclusive
            .compose_all(&nodes, &targets[..1], &[])
            .unwrap();
        assert!(!at(&single, "second").0);
        assert!(
            Arrangement::Exclusive
                .compose_all(&nodes, &[], &[])
                .is_err()
        );
    }
    #[test]
    fn exclusive_retains_remote_monitors_and_isolation_keeps_physical_geometry() {
        let nodes = vec![
            node("physical", 0),
            node("game", 1920),
            node("remote", 3840),
        ];
        let exclusive = Arrangement::Exclusive
            .compose(&nodes, "game", &["remote".into()])
            .unwrap();
        assert!(!exclusive[0].active);
        assert!(exclusive[1].primary);
        assert!(exclusive[2].active);
        assert_eq!(exclusive[1].desired_position, Position { x: 0, y: 0 });
        let isolated = Arrangement::PrimaryIsolated
            .compose(&nodes, "game", &[])
            .unwrap();
        assert_eq!(
            isolated[0].desired_position,
            Position { x: 64000, y: 64000 }
        );
        assert_eq!(
            isolated[2].desired_position.x - isolated[0].desired_position.x,
            3840
        );
    }
    #[test]
    fn remote_monitors_follow_which_displays_stay_on_but_keep_their_place() {
        let layout = |value| Arrangement::parse(value).unwrap().for_remote_monitor();
        assert_eq!(layout("exclusive"), Some(Arrangement::Exclusive));
        assert_eq!(layout("extended_primary"), Some(Arrangement::Primary));
        assert_eq!(
            layout("extended_primary_isolated"),
            Some(Arrangement::Primary)
        );
        assert_eq!(layout("extended"), None);
        assert_eq!(layout("extended_isolated"), None);
        // Exclusive on the remote monitor: the physical display goes off and
        // the remote monitor becomes primary, with another device's remote
        // monitor kept on beside it.
        let nodes = vec![
            node("physical", 0),
            node("remote", 1920),
            node("other", 3840),
        ];
        let exclusive = Arrangement::Exclusive
            .compose(&nodes, "remote", &["other".into()])
            .unwrap();
        assert!(!exclusive[0].active && !exclusive[0].primary);
        assert!(exclusive[1].active && exclusive[1].primary);
        assert_eq!(exclusive[1].desired_position, Position { x: 0, y: 0 });
        assert!(exclusive[2].active);
    }
    #[test]
    fn a_display_the_exclusive_layout_switched_off_is_put_back_off_after_it_settles() {
        use std::time::{Duration, Instant};
        // Before the stream: the physical monitor on its own, a TV switched
        // off. The exclusive layout keeps only the virtual display on.
        let mut phys = node("phys", 0);
        phys.primary = true;
        let mut tv = node("tv", 1920);
        tv.active = false;
        let mut vdd = node("vdd", 0);
        vdd.active = false;
        let before = vec![phys.clone(), tv.clone(), vdd.clone()];
        let applied = Arrangement::Exclusive
            .compose(&[phys.clone(), tv.clone(), node("vdd", 3840)], "vdd", &[])
            .unwrap();
        let on = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(switched_back_on(&before, &applied, &on(&["vdd"])).is_empty());
        // The Win key over an exclusive-fullscreen game: Windows recalls its
        // saved layout and the physical monitor comes back beside the stream.
        assert_eq!(
            switched_back_on(&before, &applied, &on(&["vdd", "phys"])),
            ["phys"]
        );
        // The TV was off before the stream: switching it on is the user's call.
        assert!(switched_back_on(&before, &applied, &on(&["vdd", "tv"])).is_empty());
        // Another client's display arriving was never laid out by this stream.
        assert!(switched_back_on(&before, &applied, &on(&["vdd", "vdd2"])).is_empty());
        // The extended layout switches nothing off, so there is nothing to keep off.
        let extended = Arrangement::Extended
            .compose(&[phys.clone(), tv, node("vdd", 3840)], "vdd", &[])
            .unwrap();
        assert!(switched_back_on(&before, &extended, &on(&["vdd", "phys"])).is_empty());

        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut watch = LayoutWatch::default();
        assert!(watch.due(at(0)));
        assert!(!watch.due(at(100)));
        assert!(watch.due(at(250)));
        // A game's own mode switch that settles within the window is left alone.
        assert!(!watch.observe(true, at(0)));
        assert!(!watch.observe(true, at(250)));
        assert!(!watch.observe(false, at(500)));
        assert!(!watch.observe(true, at(750)));
        assert!(!watch.observe(true, at(1000)));
        // Still on after the settle time: put the layout back once.
        assert!(watch.observe(true, at(1250)));
        assert!(!watch.observe(true, at(1500)));
        // A refused attempt waits before the next check.
        watch.failed(at(1500));
        assert!(!watch.due(at(3000)));
        assert!(watch.due(at(3500)));
    }
}
