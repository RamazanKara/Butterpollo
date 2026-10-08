//! Input desktop attachment and scoped display work.
#![warn(clippy::undocumented_unsafe_blocks)]

use super::*;

struct InputDesktop {
    original: HDESK,
    current: HDESK,
}
impl Drop for InputDesktop {
    fn drop(&mut self) {
        // SAFETY: The original desktop handle is borrowed from Windows; the owned current handle is closed after detaching.
        unsafe {
            // Windows refuses to close a handle while a thread is attached to it.
            let _ = SetThreadDesktop(self.original);
            let _ = CloseDesktop(self.current);
        }
    }
}
thread_local! {
    static INPUT_DESKTOP: std::cell::RefCell<Option<InputDesktop>> = const { std::cell::RefCell::new(None) };
}

/// Move the calling thread to the desktop that receives input, which is the
/// secure desktop while a UAC prompt or the lock screen shows. Only a host
/// running as SYSTEM may attach to it; elsewhere this fails harmlessly.
/// Returns whether the thread is now on the input desktop.
pub fn follow_input_desktop() -> bool {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    // SAFETY: The thread-local attachment owns every opened desktop handle, retains the attached one, and closes only detached handles.
    INPUT_DESKTOP.with_borrow_mut(|attachment| unsafe {
        let Ok(desktop) = OpenInputDesktop(
            DF_ALLOWOTHERACCOUNTHOOK,
            false,
            DESKTOP_ACCESS_FLAGS(windows::Win32::Foundation::GENERIC_ALL.0),
        ) else {
            return false;
        };
        let original = GetThreadDesktop(GetCurrentThreadId());
        let Ok(original) = original else {
            let _ = CloseDesktop(desktop);
            return false;
        };
        if SetThreadDesktop(desktop).is_err() {
            let _ = CloseDesktop(desktop);
            return false;
        }
        if let Some(attached) = attachment {
            let previous = std::mem::replace(&mut attached.current, desktop);
            let _ = CloseDesktop(previous);
        } else {
            *attachment = Some(InputDesktop {
                original,
                current: desktop,
            });
        }
        true
    })
}
fn desktop_name(desktop: HDESK) -> Option<String> {
    let mut name = [0u16; 256];
    // SAFETY: The desktop handle is live and the writable UTF-16 buffer is passed with its exact byte capacity.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            size_of_val(&name) as u32,
            None,
        )
    }
    .ok()?;
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    Some(String::from_utf16_lossy(&name[..end]))
}
/// The desktop that receives input: "Default", or "Winlogon" while the lock
/// screen, the sign-in screen or a UAC prompt shows. None when this process
/// may not see it, as a host outside the service may not while Windows is locked.
pub fn input_desktop_name() -> Option<String> {
    // SAFETY: The opened desktop handle is used only while live and closed exactly once after querying its name.
    unsafe {
        let desktop =
            OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS).ok()?;
        let name = desktop_name(desktop);
        let _ = CloseDesktop(desktop);
        name
    }
}
fn thread_desktop_name() -> Option<String> {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    // Windows owns the thread's desktop handle; it is not closed here.
    // SAFETY: The current thread ID is valid; Windows owns the returned desktop handle.
    unsafe { GetThreadDesktop(GetCurrentThreadId()) }
        .ok()
        .and_then(desktop_name)
}
/// Whether Windows shows the lock screen, the sign-in screen or a UAC prompt.
pub fn secure_desktop_shown() -> bool {
    input_desktop_name().is_some_and(|name| !name.eq_ignore_ascii_case("default"))
}
/// Whether a thread on desktop `thread` must move to `input` before
/// configuring displays: Windows refuses QueryDisplayConfig and
/// SetDisplayConfig ("access denied") to a thread on another desktop. A
/// desktop that cannot be named keeps the work where it is.
fn must_follow(thread: Option<&str>, input: Option<&str>) -> bool {
    matches!((thread, input), (Some(thread), Some(input)) if !thread.eq_ignore_ascii_case(input))
}
/// For threads that keep a display's layout: follow the input desktop to the
/// lock screen and back, moving only when it changed.
pub fn keep_on_input_desktop() {
    if must_follow(
        thread_desktop_name().as_deref(),
        input_desktop_name().as_deref(),
    ) {
        follow_input_desktop();
    }
}
/// Run display configuration where Windows allows it. While the lock screen
/// or sign-in screen shows, that is only on its desktop, so the work runs on a
/// fresh thread attached there: a thread that ever owned a window, COM's
/// included, cannot change desktops. Otherwise it runs on the calling thread.
pub fn on_input_desktop<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    if !must_follow(
        thread_desktop_name().as_deref(),
        input_desktop_name().as_deref(),
    ) {
        return work();
    }
    let mut work = Some(work);
    let slot = &mut work;
    let done = std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("input-desktop".into())
            .spawn_scoped(scope, move || {
                if !follow_input_desktop() {
                    tracing::warn!("display work could not move to the input desktop");
                }
                slot.take().map(|work| work())
            });
        match worker {
            Ok(worker) => worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(error) => {
                tracing::warn!(%error, "display work could not start on the input desktop");
                None
            }
        }
    });
    match done {
        Some(done) => done,
        // The thread never started, so the work is still here.
        None => work.take().expect("unstarted display work")(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an unlocked Windows desktop"]
    fn input_desktop_handles_close_on_reattachment_and_thread_exit() {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::*;
        if std::env::var_os("BUTTERPOLLO_DESKTOP_HANDLE_TEST").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "input::desktop::tests::input_desktop_handles_close_on_reattachment_and_thread_exit",
                    "--ignored",
                ])
                .env("BUTTERPOLLO_DESKTOP_HANDLE_TEST", "1")
                .creation_flags(CREATE_NO_WINDOW.0)
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let run = || {
            std::thread::spawn(|| {
                for _ in 0..8 {
                    assert!(follow_input_desktop());
                }
            })
            .join()
            .unwrap();
        };
        run();
        let handles = || {
            let mut count = 0;
            // SAFETY: GetCurrentProcess returns a valid pseudo-handle and count remains writable throughout the query.
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count).unwrap() };
            count
        };
        // A subprocess keeps unrelated parallel tests out of this measurement.
        let before = handles();
        for _ in 0..32 {
            run();
        }
        assert_eq!(handles(), before);
    }
    #[test]
    fn display_work_moves_only_to_a_different_named_input_desktop() {
        assert!(!must_follow(Some("Default"), Some("Default")));
        assert!(!must_follow(Some("Default"), Some("default")));
        assert!(must_follow(Some("Default"), Some("Winlogon")));
        assert!(must_follow(Some("Winlogon"), Some("Default")));
        // A host that may not open the lock screen's desktop stays put.
        assert!(!must_follow(Some("Default"), None));
        assert!(!must_follow(None, Some("Winlogon")));
    }
    #[test]
    fn display_work_returns_its_result_and_stays_on_the_caller_when_no_move_is_needed() {
        assert_eq!(on_input_desktop(|| 7), 7);
        let caller = std::thread::current().id();
        let ran_on = on_input_desktop(|| std::thread::current().id());
        if !must_follow(
            thread_desktop_name().as_deref(),
            input_desktop_name().as_deref(),
        ) {
            assert_eq!(ran_on, caller);
        }
        let borrowed = String::from("display");
        assert_eq!(on_input_desktop(|| borrowed.len()), 7);
    }
}
