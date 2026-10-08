//! Lossless Scaling on this PC: where it is, its settings file, and the
//! window and keyboard steps that start scaling a game.
#![warn(clippy::undocumented_unsafe_blocks)]

use std::path::{Path, PathBuf};
use windows::Win32::{
    Foundation::{HWND, LPARAM},
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
            SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            BringWindowToTop, EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow,
            GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE, SW_SHOWMINNOACTIVE,
            SetForegroundWindow, ShowWindow,
        },
    },
};

pub const PROCESSES: [&str; 2] = ["LosslessScaling.exe", "Lossless Scaling.exe"];

/// Inputs to Vibepollo's CPU normalization and Windows-process penalty.
pub fn scoring_environment() -> (u32, Option<String>) {
    use windows::Win32::System::SystemInformation::{
        GetSystemInfo, GetWindowsDirectoryW, SYSTEM_INFO,
    };
    let mut system = SYSTEM_INFO::default();
    let mut directory = [0u16; 260];
    // SAFETY: both output buffers are valid for the synchronous calls, and the directory slice
    // carries its capacity.
    let length = unsafe {
        GetSystemInfo(&mut system);
        GetWindowsDirectoryW(Some(&mut directory)) as usize
    };
    (
        system.dwNumberOfProcessors.max(1),
        (length > 0 && length < directory.len())
            .then(|| String::from_utf16_lossy(&directory[..length])),
    )
}

fn program_in(folder: &Path) -> Option<PathBuf> {
    let direct = PROCESSES
        .iter()
        .map(|name| folder.join(name))
        .find(|p| p.is_file());
    direct.or_else(|| {
        std::fs::read_dir(folder)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .find_map(|sub| {
                PROCESSES
                    .iter()
                    .map(|name| sub.join(name))
                    .find(|p| p.is_file())
            })
    })
}
/// The Lossless Scaling program: `configured` (the program or its folder)
/// when set, else the running copy, a Steam library or the usual folders.
/// A configured path that does not exist is not replaced by a guess.
pub fn program(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        return if path.is_file() {
            Some(path)
        } else {
            program_in(&path)
        };
    }
    if let Some(running) = crate::process::processes().ok().and_then(|list| {
        list.into_iter()
            .find(|p| PROCESSES.iter().any(|n| p.name.eq_ignore_ascii_case(n)))
            .and_then(|p| crate::process::image_path(p.pid))
    }) {
        return Some(PathBuf::from(running));
    }
    let mut folders: Vec<PathBuf> = vec![];
    for root in crate::steam::roots() {
        let Some(apps) = butterpollo_core::steam::steamapps(&root) else {
            continue;
        };
        folders.push(apps.join("common"));
        if let Ok(text) = std::fs::read_to_string(apps.join("libraryfolders.vdf")) {
            let folders_vdf = butterpollo_core::steam::Vdf::parse(&text);
            let list = folders_vdf.get("libraryfolders").unwrap_or(&folders_vdf);
            for (_, folder) in list.entries() {
                if let Some(path) = folder
                    .get("path")
                    .or(Some(folder))
                    .and_then(butterpollo_core::steam::Vdf::text)
                {
                    folders.push(Path::new(path).join("steamapps").join("common"));
                }
            }
        }
    }
    let user = crate::process::user_environment().unwrap_or_default();
    for variable in ["PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(folder) = std::env::var_os(variable) {
            folders.push(PathBuf::from(folder));
        }
    }
    if let Some(local) = user.get("LOCALAPPDATA") {
        folders.push(PathBuf::from(local).join("Programs"));
    }
    folders
        .iter()
        .map(|folder| folder.join("Lossless Scaling"))
        .find_map(|folder| program_in(&folder))
}
/// The signed-in user's Lossless Scaling settings file.
pub fn settings_path() -> Option<PathBuf> {
    let user = crate::process::user_environment().ok()?;
    Some(
        PathBuf::from(user.get("LOCALAPPDATA")?)
            .join("Lossless Scaling")
            .join("settings.xml"),
    )
}
/// Visible top-level windows of `pid`.
pub fn windows_of(pid: u32) -> Vec<HWND> {
    struct Search {
        pid: u32,
        found: Vec<HWND>,
    }
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> windows::core::BOOL {
        // SAFETY: EnumWindows passes the `&mut Search` from windows_of as `data`, which outlives
        // the enumeration and is not otherwise used during it.
        unsafe {
            let search = &mut *(data.0 as *mut Search);
            let mut pid = 0;
            GetWindowThreadProcessId(window, Some(&mut pid));
            if pid == search.pid
                && IsWindowVisible(window).as_bool()
                && GetWindow(window, GW_OWNER).map_or(true, |owner| owner.is_invalid())
            {
                search.found.push(window);
            }
            windows::core::BOOL(1)
        }
    }
    let mut search = Search { pid, found: vec![] };
    // SAFETY: `search` outlives the synchronous EnumWindows call that reads it through `visit`.
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM((&mut search as *mut Search) as isize));
    }
    search.found
}
/// Minimize `pid`'s windows without activating them.
pub fn minimize(pid: u32) {
    for window in windows_of(pid) {
        // SAFETY: ShowWindow only takes a window handle by value; a stale handle just fails.
        unsafe {
            let _ = ShowWindow(window, SW_SHOWMINNOACTIVE);
        }
    }
}
/// Bring `pid`'s main window to the front; true when it is in front.
pub fn focus(pid: u32) -> bool {
    let Some(window) = windows_of(pid).into_iter().next() else {
        return false;
    };
    crate::input::follow_input_desktop();
    // SAFETY: these calls take window handles by value, and a stale handle just fails.
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        // Windows lets the process that sent the last input take the
        // foreground; an Alt tap makes that this one.
        send(&[key(0x12, false), key(0x12, true)]);
        let _ = SetForegroundWindow(window);
        let _ = BringWindowToTop(window);
        GetForegroundWindow() == window
    }
}
fn key(code: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(code),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}
fn send(inputs: &[INPUT]) -> bool {
    // SAFETY: `inputs` outlives the call, its length is passed by the slice, and the size is
    // INPUT's.
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) as usize == inputs.len() }
}
/// Press `modifiers` and `code`, then release them in reverse.
pub fn press(modifiers: &[u16], code: u16) -> bool {
    crate::input::follow_input_desktop();
    let mut inputs: Vec<INPUT> = modifiers.iter().map(|m| key(*m, false)).collect();
    inputs.push(key(code, false));
    inputs.push(key(code, true));
    inputs.extend(modifiers.iter().rev().map(|m| key(*m, true)));
    send(&inputs)
}
