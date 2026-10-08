//! Native notification-area UI, owned by its Windows message thread.
use anyhow::{Context, Result, bail};
use butterpollo_core::tray::Icon;
use std::{
    mem::size_of,
    path::PathBuf,
    sync::{Mutex, mpsc},
    thread,
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, DIB_RGB_COLORS,
            DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};
/// Retries adding the icon while the taskbar does not accept it yet.
const RETRY_TIMER: usize = 1;
/// The running app changed; see [`show_app`].
const APP_CHANGED: u32 = WM_APP + 2;
#[derive(Clone, Copy)]
pub enum Action {
    Open,
    StopSessions,
    /// Quit the running app, as the console's close button does.
    QuitApp,
    /// Look for a newer release and say what was found.
    CheckUpdates,
    Restart,
    Quit,
}
struct State {
    sender: mpsc::Sender<Action>,
    icon: NOTIFYICONDATAW,
    /// Idle, streaming and paused.
    icons: [HICON; 3],
    port: u16,
    taskbar: u32,
    hide_controls: bool,
}
impl State {
    /// Show the running app's state in the icon and its tooltip.
    fn show(&mut self) {
        let app = APP.lock().unwrap();
        self.icon.hIcon = self.icons[match app.icon {
            Icon::Idle => 0,
            Icon::Streaming => 1,
            Icon::Paused => 2,
        }];
        let tip = match &app.summary {
            Some(summary) => format!("Butterpollo — {summary}"),
            None => format!("Butterpollo — web port {}", self.port),
        };
        let tip: Vec<u16> = tip.encode_utf16().take(127).collect();
        self.icon.szTip = [0; 128];
        self.icon.szTip[..tip.len()].copy_from_slice(&tip);
    }
}
/// The running app as the tray shows it.
struct Shown {
    icon: Icon,
    app: Option<String>,
    summary: Option<String>,
}
static APP: Mutex<Shown> = Mutex::new(Shown {
    icon: Icon::Idle,
    app: None,
    summary: None,
});
/// Show the running app (None when none runs) in the tray: the icon's
/// state, the tooltip and the menu's item that quits it.
pub fn show_app(icon: Icon, app: Option<&str>, summary: Option<&str>) {
    *APP.lock().unwrap() = Shown {
        icon,
        app: app.map(str::to_owned),
        summary: summary.map(str::to_owned),
    };
    if let Some(window) = *SHOWN.lock().unwrap() {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(window as *mut _)),
                APP_CHANGED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}
/// `base` with the state's dot in its corner; None for Idle or an icon
/// without alpha, which then shows as is.
unsafe fn badged(base: HICON, state: Icon) -> Option<HICON> {
    unsafe {
        let mut info = ICONINFO::default();
        GetIconInfo(base, &mut info).ok()?;
        let made = (|| {
            if info.hbmColor.is_invalid() {
                return None;
            }
            let mut bitmap = BITMAP::default();
            if GetObjectW(
                info.hbmColor.into(),
                size_of::<BITMAP>() as i32,
                Some((&mut bitmap as *mut BITMAP).cast()),
            ) == 0
            {
                return None;
            }
            let (width, height) = (bitmap.bmWidth, bitmap.bmHeight);
            if width <= 0 || height <= 0 || width > 256 || height > 256 {
                return None;
            }
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Top-down rows, as CreateBitmap takes them.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = vec![0u8; width as usize * height as usize * 4];
            let screen = GetDC(None);
            let lines = GetDIBits(
                screen,
                info.hbmColor,
                0,
                height as u32,
                Some(pixels.as_mut_ptr().cast()),
                &mut header,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, screen);
            if lines != height
                || !butterpollo_core::tray::badge(
                    &mut pixels,
                    width as usize,
                    height as usize,
                    state,
                )
            {
                return None;
            }
            let color = CreateBitmap(width, height, 1, 32, Some(pixels.as_ptr().cast()));
            // An all-zero mask: the colour image's alpha decides.
            let mask_bytes = vec![0u8; (width as usize).div_ceil(16) * 2 * height as usize];
            let mask = CreateBitmap(width, height, 1, 1, Some(mask_bytes.as_ptr().cast()));
            let icon = (!color.is_invalid() && !mask.is_invalid())
                .then(|| {
                    CreateIconIndirect(&ICONINFO {
                        fIcon: TRUE,
                        xHotspot: 0,
                        yHotspot: 0,
                        hbmMask: mask,
                        hbmColor: color,
                    })
                    .ok()
                })
                .flatten();
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
            icon
        })();
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        made
    }
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
            // Explorer announces a new taskbar (at sign-in, or after it
            // restarts); until the icon is in, retry every few seconds.
            if message == s.taskbar || (message == WM_TIMER && wparam.0 == RETRY_TIMER) {
                s.show();
                if Shell_NotifyIconW(NIM_ADD, &s.icon).as_bool() {
                    let _ = KillTimer(Some(hwnd), RETRY_TIMER);
                } else if message == s.taskbar {
                    SetTimer(Some(hwnd), RETRY_TIMER, 3000, None);
                }
                return LRESULT(0);
            }
            if message == APP_CHANGED {
                s.show();
                let _ = Shell_NotifyIconW(NIM_MODIFY, &s.icon);
                return LRESULT(0);
            }
            if message == WM_APP + 1 {
                match (lparam.0 & 0xffff) as u32 {
                    WM_LBUTTONDBLCLK => {
                        let _ = s.sender.send(Action::Open);
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => {
                        if let Ok(menu) = CreatePopupMenu() {
                            let _ = AppendMenuW(menu, MF_STRING, 1, w!("Open Butterpollo"));
                            let _ = AppendMenuW(menu, MF_STRING, 6, w!("Check for updates"));
                            if !s.hide_controls {
                                let _ = AppendMenuW(menu, MF_STRING, 2, w!("Disconnect clients"));
                                let app = APP.lock().unwrap().app.clone();
                                let label: Vec<u16> = match &app {
                                    // An ampersand would underline the next letter.
                                    Some(app) => format!("Quit {}", app.replace('&', "&&")),
                                    None => "No app running".into(),
                                }
                                .encode_utf16()
                                .chain(Some(0))
                                .collect();
                                let flags = if app.is_some() {
                                    MF_STRING
                                } else {
                                    MF_STRING | MF_GRAYED
                                };
                                let _ = AppendMenuW(menu, flags, 5, PCWSTR(label.as_ptr()));
                                let _ = AppendMenuW(menu, MF_STRING, 3, w!("Restart"));
                                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                                let _ = AppendMenuW(menu, MF_STRING, 4, w!("Quit"));
                            }
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
                                5 => Some(Action::QuitApp),
                                6 => Some(Action::CheckUpdates),
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
        Self::new_options(icon, port, false)
    }
    pub fn new_options(
        icon: PathBuf,
        port: u16,
        hide_controls: bool,
    ) -> Result<(Self, mpsc::Receiver<Action>)> {
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
                    // The executable's own icon (resource 1, see host/build.rs),
                    // else the icon file, else the generic application icon.
                    let (width, height) =
                        (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
                    let path: Vec<u16> = icon
                        .to_string_lossy()
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                    let custom = LoadImageW(
                        Some(HINSTANCE(module.0)),
                        PCWSTR(1 as _),
                        IMAGE_ICON,
                        width,
                        height,
                        LR_DEFAULTCOLOR,
                    )
                    .or_else(|_| {
                        LoadImageW(
                            None,
                            PCWSTR(path.as_ptr()),
                            IMAGE_ICON,
                            width,
                            height,
                            LR_LOADFROMFILE,
                        )
                    })
                    .ok();
                    let icon = if let Some(handle) = custom {
                        HICON(handle.0)
                    } else {
                        LoadIconW(None, IDI_APPLICATION)?
                    };
                    let icons = [
                        icon,
                        badged(icon, Icon::Streaming).unwrap_or(icon),
                        badged(icon, Icon::Paused).unwrap_or(icon),
                    ];
                    let mut state = Box::new(State {
                        sender,
                        icons,
                        port,
                        icon: NOTIFYICONDATAW {
                            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                            uID: 1,
                            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                            uCallbackMessage: WM_APP + 1,
                            hIcon: icon,
                            ..Default::default()
                        },
                        taskbar: RegisterWindowMessageW(w!("TaskbarCreated")),
                        hide_controls,
                    });
                    state.show();
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
                    // The service starts the host elevated, and Windows keeps
                    // Explorer's messages from an elevated window unless they
                    // are allowed: the TaskbarCreated broadcast after sign-in
                    // never arrived, so an icon that the not-yet-ready taskbar
                    // refused at boot never appeared.
                    for message in [state.taskbar, WM_APP + 1] {
                        let _ = ChangeWindowMessageFilterEx(hwnd, message, MSGFLT_ALLOW, None);
                    }
                    if !Shell_NotifyIconW(NIM_ADD, &state.icon).as_bool() {
                        SetTimer(Some(hwnd), RETRY_TIMER, 3000, None);
                    }
                    *SHOWN.lock().unwrap() = Some(hwnd.0 as isize);
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
                    *SHOWN.lock().unwrap() = None;
                    let _ = Shell_NotifyIconW(NIM_DELETE, &state.icon);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = DestroyWindow(hwnd);
                    for badge in &icons[1..] {
                        if *badge != icon {
                            let _ = DestroyIcon(*badge);
                        }
                    }
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
/// The tray icon's window while it is shown.
static SHOWN: std::sync::Mutex<Option<isize>> = std::sync::Mutex::new(None);
/// Show a notification from the tray icon, if there is one.
pub fn notify(title: &str, text: &str) {
    let Some(window) = *SHOWN.lock().unwrap() else {
        return;
    };
    let mut icon = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(window as *mut _),
        uID: 1,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    let copy = |target: &mut [u16], value: &str| {
        let units: Vec<u16> = value.encode_utf16().take(target.len() - 1).collect();
        target[..units.len()].copy_from_slice(&units);
    };
    copy(&mut icon.szInfoTitle, title);
    copy(&mut icon.szInfo, text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &icon);
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
