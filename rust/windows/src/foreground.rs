//! Read-only visible window stack, including passive shell/compositor overlays.
use crate::capture::Display;
use std::sync::{Arc, Mutex, mpsc};
use windows::{
    Win32::{Foundation::*, Graphics::Dwm::*, System::Threading::*, UI::WindowsAndMessaging::*},
    core::{BOOL, PWSTR},
};
#[derive(Clone, PartialEq, Eq)]
struct Request {
    owned: Vec<u32>,
    rect: [i32; 4],
}
struct Snapshot {
    request: Request,
    /// The selected window's program and process.
    selected: Option<(String, u32)>,
}
/// Window enumeration and process image queries never run on the encode thread.
pub struct Tracker {
    sender: Option<mpsc::SyncSender<Request>>,
    result: Arc<Mutex<Option<Snapshot>>>,
}
impl Default for Tracker {
    fn default() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Request>(1);
        let result = Arc::new(Mutex::new(None));
        let worker = result.clone();
        let started = std::thread::Builder::new()
            .name("visible-game".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    let selected = visible_executable(&request);
                    *worker.lock().unwrap() = Some(Snapshot { request, selected });
                }
            })
            .is_ok();
        Self {
            sender: started.then_some(sender),
            result,
        }
    }
}
impl Tracker {
    pub fn poll(&self, owned: &[u32], display: &Display) -> Option<String> {
        self.poll_process(owned, display).map(|(exe, _)| exe)
    }
    /// The program and process of the selected window, as of the last scan.
    pub fn poll_process(&self, owned: &[u32], display: &Display) -> Option<(String, u32)> {
        let request = Request {
            owned: owned.to_vec(),
            rect: [
                display.x,
                display.y,
                display.x.saturating_add(display.width as i32),
                display.y.saturating_add(display.height as i32),
            ],
        };
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(request.clone());
        }
        self.result
            .lock()
            .unwrap()
            .as_ref()
            .filter(|snapshot| snapshot.request == request)
            .and_then(|snapshot| snapshot.selected.clone())
    }
}
fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut name = [0; 32768];
        let mut count = name.len() as u32;
        let queried = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(name.as_mut_ptr()),
            &mut count,
        );
        let _ = CloseHandle(process);
        queried.ok()?;
        Some(String::from_utf16_lossy(&name[..count as usize]))
    }
}
#[derive(Default)]
struct Evidence {
    belongs: bool,
    desktop: bool,
    passive: bool,
    fullscreen: bool,
    opaque: bool,
    transient: bool,
}
#[derive(Debug, PartialEq)]
enum Decision {
    Continue,
    Select,
    Block,
}
fn decide(e: &Evidence, attributed: bool) -> Decision {
    if e.transient || e.passive {
        return Decision::Continue;
    }
    if e.desktop {
        return Decision::Block;
    }
    if attributed && e.belongs {
        return if e.fullscreen && e.opaque {
            Decision::Select
        } else {
            Decision::Continue
        };
    }
    if !e.opaque {
        return Decision::Continue;
    }
    if attributed || !e.fullscreen {
        Decision::Block
    } else {
        Decision::Select
    }
}
fn fullscreen(rect: RECT, capture: [i32; 4], framed: bool) -> bool {
    !framed
        && rect.left <= capture[0].saturating_add(2)
        && rect.top <= capture[1].saturating_add(2)
        && rect.right >= capture[2].saturating_sub(2)
        && rect.bottom >= capture[3].saturating_sub(2)
}
fn visible_executable(request: &Request) -> Option<(String, u32)> {
    struct Scan<'a> {
        request: &'a Request,
        selected: Option<(String, u32)>,
        count: usize,
    }
    unsafe extern "system" fn visit(window: HWND, context: LPARAM) -> BOOL {
        unsafe {
            let scan = &mut *(context.0 as *mut Scan<'_>);
            scan.count += 1;
            if scan.count > 4096 {
                return false.into();
            }
            if !IsWindowVisible(window).as_bool() || IsIconic(window).as_bool() {
                return true.into();
            }
            let mut cloaked = 0u32;
            let _ =
                DwmGetWindowAttribute(window, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4);
            if cloaked != 0 {
                return true.into();
            }
            let style = GetWindowLongPtrW(window, GWL_STYLE) as u32;
            let ex = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
            let framed = style & (WS_CAPTION.0 | WS_THICKFRAME.0) != 0;
            let mut color = COLORREF::default();
            let mut alpha = 255;
            let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS::default();
            let layered = ex & WS_EX_LAYERED.0 != 0;
            let alpha_known = layered
                && GetLayeredWindowAttributes(
                    window,
                    Some(&mut color),
                    Some(&mut alpha),
                    Some(&mut flags),
                )
                .is_ok();
            if alpha_known && flags.0 & LWA_ALPHA.0 != 0 && alpha == 0 {
                return true.into();
            }
            let opaque = !layered
                || (alpha_known
                    && flags.0 & LWA_COLORKEY.0 == 0
                    && flags.0 & LWA_ALPHA.0 != 0
                    && alpha == 255);
            let mut rect = RECT::default();
            if DwmGetWindowAttribute(
                window,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&mut rect as *mut RECT).cast(),
                std::mem::size_of::<RECT>() as u32,
            )
            .is_err()
                && GetWindowRect(window, &mut rect).is_err()
            {
                return true.into();
            }
            let c = scan.request.rect;
            if rect.right.min(c[2]) - rect.left.max(c[0]) < 3
                || rect.bottom.min(c[3]) - rect.top.max(c[1]) < 3
            {
                return true.into();
            }
            let mut pid = 0;
            GetWindowThreadProcessId(window, Some(&mut pid));
            let exe = process_path(pid);
            let basename = exe
                .as_ref()
                .and_then(|p| p.rsplit(['/', '\\']).next())
                .unwrap_or("")
                .to_ascii_lowercase();
            let mut class = [0; 256];
            let count = GetClassNameW(window, &mut class).max(0) as usize;
            let class = String::from_utf16_lossy(&class[..count]);
            let attributed = !scan.request.owned.is_empty();
            let belongs = attributed && scan.request.owned.contains(&pid);
            let desktop = window == GetDesktopWindow()
                || window == GetShellWindow()
                || matches!(
                    class.as_str(),
                    "Progman"
                        | "WorkerW"
                        | "SHELLDLL_DefView"
                        | "Shell_TrayWnd"
                        | "Shell_SecondaryTrayWnd"
                )
                || matches!(
                    basename.as_str(),
                    "explorer.exe"
                        | "startmenuexperiencehost.exe"
                        | "searchhost.exe"
                        | "searchapp.exe"
                        | "shellexperiencehost.exe"
                        | "textinputhost.exe"
                        | "lockapp.exe"
                        | "systemsettings.exe"
                        | "applicationframehost.exe"
                )
                || (!attributed && exe.is_none());
            let fullscreen = fullscreen(rect, c, framed);
            let transient = desktop
                && (matches!(
                    class.as_str(),
                    "XamlExplorerHostIslandWindow"
                        | "MultitaskingViewFrame"
                        | "TaskSwitcherWnd"
                        | "TaskSwitcherOverlayWnd"
                        | "ForegroundStaging"
                ) || (!fullscreen
                    && matches!(class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")));
            let passive = !desktop
                && !belongs
                && ((!framed
                    && (ex & (WS_EX_NOACTIVATE.0 | WS_EX_TRANSPARENT.0) != 0
                        || ex & (WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0)
                            == (WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0)))
                    || (alpha_known
                        && (flags.0 & LWA_COLORKEY.0 != 0
                            || (flags.0 & LWA_ALPHA.0 != 0 && alpha < 255))));
            match decide(
                &Evidence {
                    belongs,
                    desktop,
                    passive,
                    fullscreen,
                    opaque,
                    transient,
                },
                attributed,
            ) {
                Decision::Select => {
                    scan.selected = exe.map(|exe| (exe, pid));
                    false.into()
                }
                Decision::Block if !attributed => false.into(),
                _ => true.into(),
            }
        }
    }
    let mut scan = Scan {
        request,
        selected: None,
        count: 0,
    };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM((&mut scan as *mut Scan<'_>) as isize));
    }
    scan.selected
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_stack_preserves_previous_shell_overlay_and_untracked_game_semantics() {
        let mut game = Evidence {
            fullscreen: true,
            opaque: true,
            ..Default::default()
        };
        assert_eq!(decide(&game, false), Decision::Select);
        assert_eq!(decide(&game, true), Decision::Block);
        game.belongs = true;
        assert_eq!(decide(&game, true), Decision::Select);
        game.fullscreen = false;
        assert_eq!(decide(&game, true), Decision::Continue);
        assert_eq!(
            decide(
                &Evidence {
                    desktop: true,
                    transient: true,
                    opaque: true,
                    ..Default::default()
                },
                false
            ),
            Decision::Continue
        );
        assert_eq!(
            decide(
                &Evidence {
                    desktop: true,
                    opaque: true,
                    ..Default::default()
                },
                false
            ),
            Decision::Block
        );
        assert_eq!(
            decide(
                &Evidence {
                    passive: true,
                    opaque: true,
                    ..Default::default()
                },
                true
            ),
            Decision::Continue
        );
        let capture = [0, 0, 1920, 1080];
        assert!(!fullscreen(
            RECT {
                left: 0,
                top: 0,
                right: 1800,
                bottom: 1080
            },
            capture,
            false
        ));
        assert!(fullscreen(
            RECT {
                left: 2,
                top: 1,
                right: 1918,
                bottom: 1080
            },
            capture,
            false
        ));
        assert!(!fullscreen(
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080
            },
            capture,
            true
        ));
    }
}
