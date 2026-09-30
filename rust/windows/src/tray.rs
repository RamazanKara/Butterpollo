//! Native notification-area UI, owned by its Windows message thread.
use anyhow::{Context, Result, bail};
use std::{mem::size_of, path::PathBuf, sync::mpsc, thread};
use windows::{
    Win32::{
        Foundation::*,
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};
#[derive(Clone, Copy)]
pub enum Action {
    Open,
    StopSessions,
    Restart,
    Quit,
}
struct State {
    sender: mpsc::Sender<Action>,
    icon: NOTIFYICONDATAW,
    taskbar: u32,
}
unsafe extern "system" fn window(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if !state.is_null() {
            let s = &mut *state;
            if message == s.taskbar {
                let _ = Shell_NotifyIconW(NIM_ADD, &s.icon);
            }
            if message == WM_APP + 1 {
                match (lparam.0 & 0xffff) as u32 {
                    WM_LBUTTONDBLCLK => {
                        let _ = s.sender.send(Action::Open);
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => {
                        if let Ok(menu) = CreatePopupMenu() {
                            let _ = AppendMenuW(menu, MF_STRING, 1, w!("Open Butterpollo"));
                            let _ = AppendMenuW(menu, MF_STRING, 2, w!("Disconnect clients"));
                            let _ = AppendMenuW(menu, MF_STRING, 3, w!("Restart"));
                            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                            let _ = AppendMenuW(menu, MF_STRING, 4, w!("Quit"));
                            let mut point = POINT::default();
                            let _ = GetCursorPos(&mut point);
                            let _ = SetForegroundWindow(hwnd);
                            let id = TrackPopupMenu(
                                menu,
                                TPM_RETURNCMD | TPM_NONOTIFY,
                                point.x,
                                point.y,
                                None,
                                hwnd,
                                None,
                            )
                            .0;
                            let action = match id {
                                1 => Some(Action::Open),
                                2 => Some(Action::StopSessions),
                                3 => Some(Action::Restart),
                                4 => Some(Action::Quit),
                                _ => None,
                            };
                            if let Some(action) = action {
                                let _ = s.sender.send(action);
                            }
                            let _ = DestroyMenu(menu);
                            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                        }
                    }
                    _ => {}
                }
                return LRESULT(0);
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}
pub struct Tray {
    id: u32,
    worker: Option<thread::JoinHandle<()>>,
}
impl Tray {
    pub fn new(icon: PathBuf, port: u16) -> Result<(Self, mpsc::Receiver<Action>)> {
        let (sender, receiver) = mpsc::channel();
        let (ready, started) = mpsc::sync_channel(1);
        let worker = thread::Builder::new().name("tray".into()).spawn(move || {
            let result = (|| -> Result<()> {
                unsafe {
                    let module = GetModuleHandleW(None)?;
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(window),
                        hInstance: HINSTANCE(module.0),
                        lpszClassName: w!("ButterpolloRustTray"),
                        ..Default::default()
                    };
                    if RegisterClassW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                        bail!("cannot register tray window");
                    }
                    let path: Vec<u16> = icon
                        .to_string_lossy()
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                    let custom = LoadImageW(
                        None,
                        PCWSTR(path.as_ptr()),
                        IMAGE_ICON,
                        16,
                        16,
                        LR_LOADFROMFILE,
                    )
                    .ok();
                    let icon = if let Some(handle) = custom {
                        HICON(handle.0)
                    } else {
                        LoadIconW(None, IDI_APPLICATION)?
                    };
                    let mut state = Box::new(State {
                        sender,
                        icon: NOTIFYICONDATAW {
                            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                            uID: 1,
                            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                            uCallbackMessage: WM_APP + 1,
                            hIcon: icon,
                            ..Default::default()
                        },
                        taskbar: RegisterWindowMessageW(w!("TaskbarCreated")),
                    });
                    let tip: Vec<u16> = format!("Butterpollo Rust — web port {port}")
                        .encode_utf16()
                        .collect();
                    state.icon.szTip[..tip.len().min(127)]
                        .copy_from_slice(&tip[..tip.len().min(127)]);
                    let hwnd = CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("ButterpolloRustTray"),
                        w!("Butterpollo Rust"),
                        WINDOW_STYLE(0),
                        0,
                        0,
                        0,
                        0,
                        None,
                        None,
                        Some(HINSTANCE(module.0)),
                        Some((&mut *state as *mut State).cast()),
                    )?;
                    state.icon.hWnd = hwnd;
                    let _ = Shell_NotifyIconW(NIM_ADD, &state.icon);
                    let _ = ready.send(Ok(GetCurrentThreadId()));
                    let mut message = MSG::default();
                    loop {
                        let status = GetMessageW(&mut message, None, 0, 0).0;
                        if status <= 0 {
                            break;
                        }
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    let _ = Shell_NotifyIconW(NIM_DELETE, &state.icon);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = DestroyWindow(hwnd);
                    if custom.is_some() {
                        let _ = DestroyIcon(icon);
                    }
                    Ok(())
                }
            })();
            if let Err(e) = result {
                let _ = ready.send(Err(e.to_string()));
            }
        })?;
        let id = started
            .recv_timeout(std::time::Duration::from_secs(10))
            .context("tray startup timed out")?
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok((
            Self {
                id,
                worker: Some(worker),
            },
            receiver,
        ))
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub fn open_web(port: u16) -> Result<()> {
    if crate::process::is_system() {
        return crate::process::Process::spawn_detached(
            &std::env::current_exe()?,
            &["--open-web".into(), port.to_string().into()],
            crate::process::Target::User { elevated: false },
        );
    }
    let url: Vec<u16> = format!("https://localhost:{port}/\0")
        .encode_utf16()
        .collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(url.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize <= 32 {
        bail!("Windows could not open the web interface");
    }
    Ok(())
}
