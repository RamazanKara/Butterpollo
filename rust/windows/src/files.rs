//! Drives on this PC, for the console's file picker.

/// The drive roots Windows reports, e.g. `C:\`.
pub fn drives() -> Vec<String> {
    // SAFETY: GetLogicalDrives takes no arguments and has no preconditions.
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| format!("{}:\\", char::from(b'A' + bit)))
        .collect()
}
