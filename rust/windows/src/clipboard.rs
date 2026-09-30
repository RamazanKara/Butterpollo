use anyhow::{Result, bail};
use windows::{
    Win32::{
        Foundation::*,
        System::{DataExchange::*, Memory::*},
        UI::WindowsAndMessaging::*,
    },
    core::w,
};
const MAX_BYTES: usize = 1024 * 1024;
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Open(HWND);
impl Open {
    fn new() -> Result<Self> {
        unsafe {
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Butterpollo clipboard"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )?;
            for _ in 0..10 {
                if OpenClipboard(Some(owner)).is_ok() {
                    return Ok(Self(owner));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let _ = DestroyWindow(owner);
            bail!("clipboard is busy")
        }
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
            let _ = DestroyWindow(self.0);
        }
    }
}
pub fn read() -> Result<String> {
    let _lock = LOCK.lock().unwrap();
    let _open = Open::new()?;
    unsafe {
        if IsClipboardFormatAvailable(13).is_err() {
            return Ok(String::new());
        }
        let handle = GetClipboardData(13)?;
        let global = HGLOBAL(handle.0);
        let size = GlobalSize(global);
        if size > MAX_BYTES || !size.is_multiple_of(2) {
            bail!("clipboard text exceeds the limit");
        }
        let pointer = GlobalLock(global).cast::<u16>();
        if pointer.is_null() {
            bail!("cannot lock clipboard text");
        }
        let text = std::slice::from_raw_parts(pointer, size / 2);
        let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
        let result = String::from_utf16(&text[..end]);
        let _ = GlobalUnlock(global);
        Ok(result?)
    }
}
pub fn write(text: &str) -> Result<()> {
    if text.len() > MAX_BYTES / 2 || text.contains('\0') {
        bail!("invalid clipboard text size or NUL character");
    }
    let _lock = LOCK.lock().unwrap();
    let _open = Open::new()?;
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let global = GlobalAlloc(GMEM_MOVEABLE, text.len() * 2)?;
        let pointer = GlobalLock(global).cast::<u16>();
        if pointer.is_null() {
            let _ = GlobalFree(Some(global));
            bail!("cannot allocate clipboard text");
        }
        std::ptr::copy_nonoverlapping(text.as_ptr(), pointer, text.len());
        let _ = GlobalUnlock(global);
        let result = EmptyClipboard().and_then(|_| SetClipboardData(13, Some(HANDLE(global.0))));
        if result.is_err() {
            let _ = GlobalFree(Some(global));
        }
        result?;
        Ok(())
    }
}
