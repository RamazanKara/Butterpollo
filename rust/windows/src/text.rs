//! UTF-16 strings for Win32 calls.

/// `text` as a NUL-terminated UTF-16 buffer for a `PCWSTR` argument.
pub(crate) fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// The string in a fixed-size UTF-16 buffer, up to its first NUL.
/// Unpaired surrogates become U+FFFD.
pub(crate) fn from_wide(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_fixed_buffer() {
        let mut buffer = [0u16; 16];
        let wide = to_wide("Radeon");
        assert_eq!(wide.last(), Some(&0));
        buffer[..wide.len()].copy_from_slice(&wide);
        assert_eq!(from_wide(&buffer), "Radeon");
        assert_eq!(from_wide(&wide[..3]), "Rad");
    }
}
