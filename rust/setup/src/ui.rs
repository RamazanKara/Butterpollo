//! Native Windows task dialogs. TaskDialogIndirect lives in Common Controls
//! 6, which a process only gets through a manifest: setup activates one at
//! run time and loads the function then, so it starts on any system.
use std::sync::{Arc, Mutex};
use windows::{
    Win32::{
        Foundation::{FreeLibrary, HMODULE, HWND, LPARAM, S_FALSE, S_OK, WPARAM},
        System::{
            ApplicationInstallationAndServicing::{ACTCTXW, ActivateActCtx, CreateActCtxW},
            LibraryLoader::{GetProcAddress, LoadLibraryW},
        },
        UI::{
            Controls::*,
            WindowsAndMessaging::{IDCANCEL, IDOK, MB_ICONERROR, MB_OK, MessageBoxW, SendMessageW},
        },
    },
    core::{BOOL, HRESULT, HSTRING, PCWSTR, w},
};

type TaskDialogIndirect =
    unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut BOOL) -> HRESULT;

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
      processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
</assembly>"#;

/// TaskDialogIndirect from Common Controls 6, loaded once.
fn task_dialog() -> Option<TaskDialogIndirect> {
    static FUNCTION: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    // SAFETY: the manifest path outlives CreateActCtxW and the module stays loaded while the address is in use.
    let address = FUNCTION.get_or_init(|| unsafe {
        let manifest = std::env::temp_dir().join("butterpollo-setup-controls.manifest");
        std::fs::write(&manifest, MANIFEST).ok()?;
        let source = HSTRING::from(manifest.as_os_str());
        let context = CreateActCtxW(&ACTCTXW {
            cbSize: size_of::<ACTCTXW>() as u32,
            lpSource: PCWSTR(source.as_ptr()),
            ..Default::default()
        })
        .ok()?;
        let mut cookie = 0;
        ActivateActCtx(Some(context), &mut cookie).ok()?;
        let library: HMODULE = LoadLibraryW(w!("comctl32.dll")).ok()?;
        match GetProcAddress(library, windows::core::s!("TaskDialogIndirect")) {
            Some(function) => Some(function as usize),
            None => {
                let _ = FreeLibrary(library);
                None
            }
        }
    });
    // SAFETY: the address is TaskDialogIndirect from comctl32 6, whose signature `TaskDialogIndirect` matches.
    address.map(|a| unsafe { std::mem::transmute::<usize, TaskDialogIndirect>(a) })
}

pub fn message_box(title: &str, text: &str) {
    let title = HSTRING::from(title);
    let text = HSTRING::from(text);
    // SAFETY: both strings are HSTRINGs that outlive the call.
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | MB_ICONERROR);
    }
}

pub struct Choice {
    pub accepted: bool,
    pub checked: bool,
}
/// A question with a confirming button, Cancel and an optional checkbox.
pub fn ask(
    title: &str,
    heading: &str,
    text: &str,
    confirm: &str,
    checkbox: Option<(&str, bool)>,
) -> Choice {
    let Some(dialog) = task_dialog() else {
        // Without Common Controls 6, a plain question.
        use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_YESNO};
        let body = HSTRING::from(format!("{heading}\n\n{text}"));
        let caption = HSTRING::from(title);
        // SAFETY: both strings are HSTRINGs that outlive the call.
        let answer = unsafe { MessageBoxW(None, &body, &caption, MB_YESNO | MB_ICONQUESTION) };
        return Choice {
            accepted: answer == IDYES,
            checked: checkbox.is_some_and(|(_, checked)| checked),
        };
    };
    let title = HSTRING::from(title);
    let heading = HSTRING::from(heading);
    let text = HSTRING::from(text);
    let confirm = HSTRING::from(confirm);
    let label = checkbox.map(|(label, _)| HSTRING::from(label));
    let buttons = [TASKDIALOG_BUTTON {
        nButtonID: IDOK.0,
        pszButtonText: PCWSTR(confirm.as_ptr()),
    }];
    let mut flags = TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW;
    if checkbox.is_some_and(|(_, checked)| checked) {
        flags |= TDF_VERIFICATION_FLAG_CHECKED;
    }
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: flags,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_SHIELD_ICON,
        },
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        pszContent: PCWSTR(text.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: IDOK.0,
        pszVerificationText: label
            .as_ref()
            .map_or(PCWSTR::null(), |l| PCWSTR(l.as_ptr())),
        ..Default::default()
    };
    let mut button = 0;
    let mut checked = BOOL(0);
    // SAFETY: `config` and every string and button it points at outlive the modal call; the out-pointers are valid or null.
    let result = unsafe { dialog(&config, &mut button, std::ptr::null_mut(), &mut checked) };
    Choice {
        accepted: result.is_ok() && button == IDOK.0,
        checked: checked.as_bool(),
    }
}

/// Shared state between the work and the progress dialog.
pub struct Progress {
    text: Mutex<String>,
    done: std::sync::atomic::AtomicBool,
}
impl Progress {
    pub fn set(&self, text: &str) {
        crate::log::line(format!("== {text}"));
        *self.text.lock().unwrap() = text.to_owned();
    }
}
unsafe extern "system" fn progress_callback(
    hwnd: HWND,
    message: TASKDIALOG_NOTIFICATIONS,
    _: WPARAM,
    _: LPARAM,
    data: isize,
) -> HRESULT {
    // SAFETY: `data` is the `lpCallbackData` set in `progress`, which keeps that state alive until the dialog closes.
    let state = unsafe { &*(data as *const (Arc<Progress>, Mutex<String>)) };
    // SAFETY: `hwnd` is the dialog that called back, valid for the duration of the notification.
    unsafe {
        match message {
            TDN_CREATED => {
                SendMessageW(
                    hwnd,
                    TDM_SET_PROGRESS_BAR_MARQUEE.0 as u32,
                    Some(WPARAM(1)),
                    Some(LPARAM(30)),
                );
            }
            TDN_TIMER => {
                let text = state.0.text.lock().unwrap().clone();
                let mut shown = state.1.lock().unwrap();
                if *shown != text {
                    let wide = HSTRING::from(text.as_str());
                    SendMessageW(
                        hwnd,
                        TDM_SET_ELEMENT_TEXT.0 as u32,
                        Some(WPARAM(TDE_CONTENT.0 as usize)),
                        Some(LPARAM(wide.as_ptr() as isize)),
                    );
                    *shown = text;
                }
                if state.0.done.load(std::sync::atomic::Ordering::Acquire) {
                    SendMessageW(
                        hwnd,
                        TDM_CLICK_BUTTON.0 as u32,
                        Some(WPARAM(IDCANCEL.0 as usize)),
                        Some(LPARAM(0)),
                    );
                }
            }
            // Setup cannot be interrupted halfway; the dialog closes itself.
            TDN_BUTTON_CLICKED if !state.0.done.load(std::sync::atomic::Ordering::Acquire) => {
                return S_FALSE;
            }
            _ => {}
        }
    }
    S_OK
}
/// Run `work` on another thread while a progress dialog shows its steps.
pub fn progress<T: Send + 'static>(
    title: &str,
    heading: &str,
    quiet: bool,
    work: impl FnOnce(Arc<Progress>) -> T + Send + 'static,
) -> T {
    let state = Arc::new(Progress {
        text: Mutex::new("Preparing…".into()),
        done: std::sync::atomic::AtomicBool::new(false),
    });
    let worker = {
        let state = state.clone();
        std::thread::spawn(move || {
            let result = work(state.clone());
            state.done.store(true, std::sync::atomic::Ordering::Release);
            result
        })
    };
    if !quiet && let Some(dialog) = task_dialog() {
        let title = HSTRING::from(title);
        let heading = HSTRING::from(heading);
        let initial = HSTRING::from("Preparing…");
        let callback_state: Box<(Arc<Progress>, Mutex<String>)> =
            Box::new((state.clone(), Mutex::new(String::new())));
        let config = TASKDIALOGCONFIG {
            cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
            dwFlags: TDF_SHOW_MARQUEE_PROGRESS_BAR | TDF_CALLBACK_TIMER,
            dwCommonButtons: TDCBF_CANCEL_BUTTON,
            pszWindowTitle: PCWSTR(title.as_ptr()),
            pszMainInstruction: PCWSTR(heading.as_ptr()),
            pszContent: PCWSTR(initial.as_ptr()),
            pfCallback: Some(progress_callback),
            lpCallbackData: &*callback_state as *const _ as isize,
            ..Default::default()
        };
        // SAFETY: `config` and the state it points at outlive the modal call; the out-pointers are null.
        unsafe {
            let _ = dialog(
                &config,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        }
        drop(callback_state);
    }
    worker.join().expect("setup work panicked")
}
/// The final message. With `action`, offers that button and reports
/// whether it was chosen.
pub fn finished(
    title: &str,
    heading: &str,
    text: &str,
    success: bool,
    action: Option<&str>,
) -> bool {
    let Some(dialog) = task_dialog() else {
        message_box(title, &format!("{heading}\n\n{text}"));
        return false;
    };
    let title = HSTRING::from(title);
    let heading = HSTRING::from(heading);
    let text = HSTRING::from(text);
    let action = action.map(HSTRING::from);
    let buttons: Vec<TASKDIALOG_BUTTON> = action
        .iter()
        .map(|label| TASKDIALOG_BUTTON {
            nButtonID: 100,
            pszButtonText: PCWSTR(label.as_ptr()),
        })
        .collect();
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: if success {
                TD_INFORMATION_ICON
            } else {
                TD_ERROR_ICON
            },
        },
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        pszContent: PCWSTR(text.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: if buttons.is_empty() {
            std::ptr::null()
        } else {
            buttons.as_ptr()
        },
        ..Default::default()
    };
    let mut button = 0;
    // SAFETY: `config` and every string and button it points at outlive the modal call.
    unsafe {
        let _ = dialog(
            &config,
            &mut button,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
    button == 100
}
