//! The display restore hotkey: a thread that owns the registration and
//! reports each press.
#![warn(clippy::undocumented_unsafe_blocks)]

use anyhow::{Context, Result};
use std::sync::mpsc;
use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{
            HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
        },
        WindowsAndMessaging::{
            GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
        },
    },
};

const ID: i32 = 1;

pub struct Hotkey {
    thread: u32,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Hotkey {
    /// Register `key` (a virtual-key code) with `modifiers` (MOD_* bits) and
    /// call `pressed` on each press until the hotkey is dropped.
    pub fn register(key: u32, modifiers: u32, pressed: impl Fn() + Send + 'static) -> Result<Self> {
        let (ready, registered) = mpsc::channel();
        // SAFETY: `message` is a live local for every call, and the hotkey is registered and
        // unregistered on this one thread with no window.
        let worker = std::thread::Builder::new()
            .name("restore-hotkey".into())
            .spawn(move || unsafe {
                let mut message = MSG::default();
                // The thread needs a message queue before it registers.
                let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                let result =
                    RegisterHotKey(None, ID, HOT_KEY_MODIFIERS(modifiers) | MOD_NOREPEAT, key);
                let failed = result.is_err();
                let _ = ready.send(result.map(|()| GetCurrentThreadId()));
                if failed {
                    return;
                }
                while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    if message.message == WM_HOTKEY && message.wParam.0 == ID as usize {
                        pressed();
                    }
                }
                let _ = UnregisterHotKey(None, ID);
            })?;
        match registered.recv() {
            Ok(Ok(thread)) => Ok(Self {
                thread,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error).context("another program may already use this key combination")
            }
            Err(_) => {
                let _ = worker.join();
                anyhow::bail!("the hotkey thread stopped")
            }
        }
    }
}
impl Drop for Hotkey {
    fn drop(&mut self) {
        // SAFETY: PostThreadMessageW only takes the worker's thread id and plain values; a stale id
        // fails.
        unsafe {
            let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
