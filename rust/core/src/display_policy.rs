use crate::topology::{Node, Position};
use anyhow::{Result, bail};

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
#[cfg(test)]
mod tests {
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
}
