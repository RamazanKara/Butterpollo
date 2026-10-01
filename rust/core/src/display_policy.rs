use crate::topology::{Node, Position};
use anyhow::{Result, bail};

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
}
#[cfg(test)]
mod tests {
    use super::*;
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
