//! Stable control tile identities used by existing Artemis/Moonlight clients.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Resume,
    DisconnectMonitor,
    DisconnectInput,
    Terminate,
    Monitor,
    Input,
}
pub fn identify(id: u32, uuid: &str) -> Option<Control> {
    for (offset, control) in [
        (1, Control::Resume),
        (2, Control::DisconnectMonitor),
        (3, Control::DisconnectInput),
        (4, Control::Terminate),
        (5, Control::Monitor),
        (6, Control::Input),
    ] {
        if id == 2147483500 + offset
            || id == 2147483600 + offset
            || (matches!(offset, 1 | 4 | 5 | 6) && id == 2147483510 + offset)
            || uuid == format!("9a1c5a25-58fe-40e0-b9aa-7d3f0000000{offset}")
        {
            return Some(control);
        }
    }
    None
}
pub fn tiles() -> [(u32, &'static str); 6] {
    [
        (2147483501, "Resume"),
        (2147483502, "Disconnect Monitor"),
        (2147483503, "Disconnect Input"),
        (2147483504, "Terminate"),
        (2147483505, "Remote Monitor"),
        (2147483506, "Remote Input"),
    ]
}
