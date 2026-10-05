//! RTSS SDK calls run in a short-lived Rust worker, keeping a stalled third-party
//! message loop outside the streaming process. The profile's unknown fields survive.
use crate::{
    ipc::Pipe,
    process::{Process, Target},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub const KEYS: [&str; 3] = ["Limit", "LimitDenominator", "SyncLimiter"];
const PIPE_PREFIX: &str = r"\\.\pipe\Butterpollo.Rtss.";
const TIMEOUT: Duration = Duration::from_secs(2);
pub fn root(config: &butterpollo_core::config::Config) -> PathBuf {
    let configured = config.get("rtss_install_path", config.get("rtss_path", ""));
    let path = PathBuf::from(if configured.is_empty() {
        "RivaTuner Statistics Server"
    } else {
        configured
    });
    if path.is_absolute() {
        return path;
    }
    for name in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(name) {
            let candidate = PathBuf::from(base).join(&path);
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    PathBuf::from("C:/Program Files (x86)").join(path)
}
pub fn executable(root: &Path) -> Option<PathBuf> {
    ["RTSS.exe", "RTSS64.exe"]
        .into_iter()
        .map(|s| root.join(s))
        .find(|p| p.is_file())
}
fn hooks(root: &Path) -> Option<PathBuf> {
    ["RTSSHooks64.dll", "RTSSHooks.dll"]
        .into_iter()
        .map(|s| root.join(s))
        .find(|p| p.is_file())
}
pub fn available(root: &Path) -> bool {
    executable(root).is_some() && hooks(root).is_some()
}
pub fn running(root: &Path) -> bool {
    use windows::Win32::{
        Foundation::*,
        System::{Diagnostics::ToolHelp::*, Threading::*},
    };
    use windows::core::PWSTR;
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut next = Process32FirstW(snapshot, &mut entry);
        let mut found = false;
        while next.is_ok() {
            let name = String::from_utf16_lossy(
                &entry.szExeFile[..entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len())],
            );
            if ["RTSS.exe", "RTSS64.exe"]
                .iter()
                .any(|n| name.eq_ignore_ascii_case(n))
                && let Ok(process) = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    entry.th32ProcessID,
                )
            {
                let mut path = vec![0u16; 32768];
                let mut size = path.len() as u32;
                if QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(path.as_mut_ptr()),
                    &mut size,
                )
                .is_ok()
                {
                    let path = PathBuf::from(String::from_utf16_lossy(&path[..size as usize]));
                    found = path.parent().is_some_and(|p| {
                        p.to_string_lossy()
                            .eq_ignore_ascii_case(&root.to_string_lossy())
                    });
                }
                let _ = CloseHandle(process);
                if found {
                    break;
                }
            }
            next = Process32NextW(snapshot, &mut entry);
        }
        let _ = CloseHandle(snapshot);
        found
    }
}
pub fn start(root: &Path) -> Result<Option<Process>> {
    if running(root) {
        return Ok(None);
    }
    let executable = executable(root).context("RTSS executable is missing")?;
    let process = start_with_elevation(crate::process::is_system(), |elevated| {
        Process::spawn(
            &executable,
            &[],
            Some(root),
            Target::User { elevated },
            &BTreeMap::new(),
            true,
        )
    })
    .with_context(|| format!("start RTSS at {}", executable.display()))?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(Some(process))
}
fn start_with_elevation<T>(service: bool, mut spawn: impl FnMut(bool) -> Result<T>) -> Result<T> {
    use windows::{Win32::Foundation::ERROR_ELEVATION_REQUIRED, core::HRESULT};
    match spawn(false) {
        Err(error)
            if error
                .downcast_ref::<windows::core::Error>()
                .is_some_and(|e| e.code() == HRESULT::from_win32(ERROR_ELEVATION_REQUIRED.0)) =>
        {
            if !service {
                return Err(error).context(
                    "RTSS requires administrator privileges. Start RTSS manually as administrator before streaming, or run Butterpollo through its installed Windows service",
                );
            }
            // RTSS can require elevation in its manifest or compatibility
            // settings. Use only the signed-in user's linked admin token;
            // RTSS must keep that user's session, profile and desktop.
            tracing::info!("RTSS requires elevation; retrying as the signed-in administrator");
            spawn(true).context(
                "RTSS could not start as the signed-in administrator. Start RTSS manually as administrator before streaming",
            )
        }
        result => result,
    }
}
pub fn read(root: &Path) -> Result<String> {
    let path = root.join("Profiles/Global");
    match std::fs::metadata(&path) {
        Ok(m) if m.len() <= 4 * 1024 * 1024 => Ok(std::fs::read_to_string(path)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        _ => bail!("RTSS Global profile is unreadable or exceeds its size limit"),
    }
}
pub fn properties(text: &str) -> Result<BTreeMap<String, Option<u32>>> {
    let mut in_section = false;
    let mut result: BTreeMap<_, _> = KEYS.iter().map(|k| (k.to_string(), None)).collect();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case("[Framerate]");
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Some(key) = KEYS.iter().find(|k| k.eq_ignore_ascii_case(key.trim())) {
            if result[*key].is_some() {
                bail!("RTSS profile has a duplicate {key}");
            }
            let value = value
                .split([';', '#'])
                .next()
                .unwrap_or("")
                .trim()
                .parse()?;
            result.insert(key.to_string(), Some(value));
        }
    }
    Ok(result)
}
pub fn replace(text: &str, values: &BTreeMap<String, Option<u32>>) -> Result<String> {
    properties(text)?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut result = Vec::new();
    let mut in_section = false;
    let mut seen_section = false;
    let mut pending = values.clone();
    let append = |result: &mut Vec<String>, pending: &mut BTreeMap<String, Option<u32>>| {
        for (key, value) in std::mem::take(pending) {
            if let Some(value) = value {
                result.push(format!("{key}={value}"));
            }
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_section {
                append(&mut result, &mut pending);
            }
            in_section = trimmed.eq_ignore_ascii_case("[Framerate]");
            seen_section |= in_section;
        }
        if in_section
            && let Some((key, _)) = trimmed.split_once('=')
            && let Some(key) = values.keys().find(|k| k.eq_ignore_ascii_case(key.trim()))
        {
            if let Some(value) = pending.remove(key).flatten() {
                result.push(format!("{key}={value}"));
            }
            continue;
        }
        result.push(line.into());
    }
    if !seen_section {
        result.push("[Framerate]".into());
    }
    append(&mut result, &mut pending);
    Ok(result.join(newline) + newline)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    root: PathBuf,
    /// Only the limiter-disable bit is touched; other RTSS flags are retained.
    disabled: Option<bool>,
    reload: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub flags: u32,
    pub values: BTreeMap<String, Option<u32>>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Response {
    Done { reply: Reply },
    Error { message: String },
}
fn call(request: &Request) -> Result<Reply> {
    // The user helper cannot write the service's config directory. Keep that
    // directory private and exchange bounded messages over an owned pipe.
    let (pipe, name) = Pipe::server(PIPE_PREFIX)?;
    let program = std::env::current_exe()?;
    let worker = Process::spawn(
        &program,
        &[
            "--rtss-worker".into(),
            name.into(),
            "--rtss-parent".into(),
            std::process::id().to_string().into(),
        ],
        program.parent(),
        Target::User { elevated: false },
        &BTreeMap::new(),
        true,
    )
    .context("start RTSS helper in the signed-in user's session")?;
    let deadline = Instant::now() + TIMEOUT;
    let check = || -> Result<()> {
        ensure!(
            worker.exit_code()?.is_none(),
            "RTSS helper exited before replying"
        );
        ensure!(
            Instant::now() < deadline,
            "RTSS helper timed out; RTSS may be unresponsive"
        );
        std::thread::sleep(Duration::from_millis(2));
        Ok(())
    };
    while !pipe.connected(worker.pid)? {
        check()?;
    }
    pipe.send(request)?;
    let response = loop {
        if let Some(response) = pipe.receive::<Response>()? {
            break response;
        }
        check()?;
    };
    // Let the helper close only after the reply has been read. Closing a named
    // pipe with unread data can discard the result, even on successful exit.
    pipe.send(&())?;
    ensure!(
        worker.wait(Duration::from_millis(500))? == 0,
        "RTSS helper failed during shutdown"
    );
    match response {
        Response::Done { reply } => Ok(reply),
        Response::Error { message } => bail!("RTSS helper: {message}"),
    }
}
pub fn worker(name: &str, parent: u32) -> Result<()> {
    let pipe = Pipe::client(name, parent, PIPE_PREFIX)?;
    let deadline = Instant::now() + TIMEOUT;
    let request = loop {
        if let Some(request) = pipe.receive::<Request>()? {
            break request;
        }
        ensure!(Instant::now() < deadline, "RTSS request timed out");
        std::thread::sleep(Duration::from_millis(2));
    };
    let response = match execute(&request) {
        Ok(reply) => Response::Done { reply },
        Err(error) => Response::Error {
            message: format!("{error:#}").chars().take(700).collect(),
        },
    };
    pipe.send(&response)?;
    let deadline = Instant::now() + TIMEOUT;
    while pipe.receive::<()>()?.is_none() {
        ensure!(
            Instant::now() < deadline,
            "RTSS reply acknowledgement timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}
fn execute(request: &Request) -> Result<Reply> {
    let library =
        unsafe { libloading::Library::new(hooks(&request.root).context("RTSS hooks missing")?)? };
    unsafe {
        let load = library.get::<unsafe extern "C" fn(*const i8)>(b"LoadProfile\0")?;
        let update = library.get::<unsafe extern "C" fn()>(b"UpdateProfiles\0")?;
        let flags = library.get::<unsafe extern "C" fn() -> u32>(b"GetFlags\0")?;
        let set_flags = library.get::<unsafe extern "C" fn(u32, u32) -> u32>(b"SetFlags\0")?;
        let get = library.get::<unsafe extern "C" fn(*const i8, *mut u32, u32) -> i32>(
            b"GetProfileProperty\0",
        )?;
        // RTSS's SDK identifies the global profile with an empty string.
        load(c"".as_ptr());
        if request.reload {
            update();
        }
        if let Some(disabled) = request.disabled {
            set_flags(!4, if disabled { 4 } else { 0 });
        }
        let mut values = BTreeMap::new();
        for (key, property) in KEYS.into_iter().zip(PROPERTIES) {
            let mut value = 0;
            let exists = get(property.as_ptr(), &mut value, 4) != 0;
            values.insert(key.into(), exists.then_some(value));
        }
        Ok(Reply {
            flags: flags(),
            values,
        })
    }
}
const PROPERTIES: [&std::ffi::CStr; 3] = [
    c"FramerateLimit",
    c"FramerateLimitDenominator",
    c"SyncLimiter",
];
pub fn query(root: &Path) -> Result<Reply> {
    call(&Request {
        root: root.into(),
        disabled: None,
        reload: false,
    })
}
pub fn reload(root: &Path, disabled: Option<bool>) -> Result<Reply> {
    call(&Request {
        root: root.into(),
        disabled,
        reload: true,
    })
}
pub fn write_profile(root: &Path, values: &BTreeMap<String, Option<u32>>) -> Result<()> {
    ensure!(
        values.keys().all(|key| KEYS.contains(&key.as_str())),
        "unknown RTSS property"
    );
    let path = root.join("Profiles/Global");
    let original = read(root)?;
    let current = properties(&original)?;
    // A failed application may leave only already-original properties to
    // restore. Do not require write/delete access or claim pending recovery
    // for a profile that never changed.
    if values
        .iter()
        .all(|(key, value)| current.get(key) == Some(value))
    {
        return Ok(());
    }
    let content = replace(&original, values)?;
    if let Err(error) = butterpollo_core::state::atomic_write(&path, content.as_bytes()) {
        // RTSS's UI opens the selected profile without delete sharing. It
        // still permits writing the file; keep unknown fields and truncate
        // only after the complete replacement has been written successfully.
        let locked = error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| matches!(e.raw_os_error(), Some(5 | 32)));
        if !locked {
            return Err(error);
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .context("open RTSS Global profile for update")?;
        file.write_all(content.as_bytes())?;
        file.set_len(content.len() as u64)?;
        file.sync_all()?;
    }
    Ok(())
}
/// Called only after the limiter has durably saved its originals. The service
/// writes the protected profile; the user helper reloads and verifies it. SDK
/// setters reject some rational numerators, so retain the complete file values.
pub fn apply(
    root: &Path,
    values: &BTreeMap<String, Option<u32>>,
    disabled: Option<bool>,
) -> Result<Reply> {
    ensure!(
        values.keys().all(|key| KEYS.contains(&key.as_str())),
        "unknown RTSS property"
    );
    if !values.is_empty() {
        write_profile(root, values).context("write RTSS Global profile")?;
    }
    reload(root, disabled)
}
pub fn wait_ready(root: &Path) -> Result<Reply> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match query(root) {
            Ok(reply) => return Ok(reply),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use windows::{Win32::Foundation::*, core::HRESULT};

    fn launch_error(code: WIN32_ERROR) -> anyhow::Error {
        windows::core::Error::from_hresult(HRESULT::from_win32(code.0)).into()
    }

    #[test]
    fn service_retries_elevation_required_with_the_user_admin_token() {
        let mut attempts = Vec::new();
        let result = start_with_elevation(true, |elevated| {
            attempts.push(elevated);
            if elevated {
                Ok(42)
            } else {
                Err(launch_error(ERROR_ELEVATION_REQUIRED).context("CreateProcessAsUserW"))
            }
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts, [false, true]);
    }

    #[test]
    fn ordinary_launches_and_unrelated_errors_never_request_elevation() {
        for service in [false, true] {
            let mut attempts = Vec::new();
            start_with_elevation(service, |elevated| {
                attempts.push(elevated);
                Ok(())
            })
            .unwrap();
            assert_eq!(attempts, [false]);
            for code in [
                ERROR_ACCESS_DENIED,
                ERROR_FILE_NOT_FOUND,
                ERROR_PRIVILEGE_NOT_HELD,
            ] {
                attempts.clear();
                let error = start_with_elevation::<()>(service, |elevated| {
                    attempts.push(elevated);
                    Err(launch_error(code))
                })
                .unwrap_err();
                assert_eq!(attempts, [false]);
                assert_eq!(
                    error.downcast_ref::<windows::core::Error>().unwrap().code(),
                    HRESULT::from_win32(code.0)
                );
            }
        }
    }

    #[test]
    fn portable_elevation_required_has_actionable_guidance_without_retrying() {
        let mut attempts = Vec::new();
        let error = start_with_elevation::<()>(false, |elevated| {
            attempts.push(elevated);
            Err(launch_error(ERROR_ELEVATION_REQUIRED))
        })
        .unwrap_err();
        assert_eq!(attempts, [false]);
        assert!(
            error
                .to_string()
                .contains("Start RTSS manually as administrator")
        );
        assert_eq!(
            error.downcast_ref::<windows::core::Error>().unwrap().code(),
            HRESULT::from_win32(ERROR_ELEVATION_REQUIRED.0)
        );
    }

    #[test]
    fn failed_elevated_retry_preserves_the_error_and_does_not_retry_again() {
        let mut attempts = Vec::new();
        let error = start_with_elevation::<()>(true, |elevated| {
            attempts.push(elevated);
            Err(launch_error(if elevated {
                ERROR_ACCESS_DENIED
            } else {
                ERROR_ELEVATION_REQUIRED
            }))
        })
        .unwrap_err();
        assert_eq!(attempts, [false, true]);
        assert!(error.to_string().contains("signed-in administrator"));
        assert_eq!(
            error.downcast_ref::<windows::core::Error>().unwrap().code(),
            HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
        );
    }

    #[test]
    fn profile_updates_preserve_other_sections_and_remove_absent_originals() {
        let original = "; user\r\n[Hooking]\r\nEnable=1\r\n[Framerate]\r\nLimit=60\r\nCustom=keep\r\n[OSD]\r\nColor=red\r\n";
        let before = properties(original).unwrap();
        let desired = BTreeMap::from([
            ("Limit".into(), Some(2997)),
            ("LimitDenominator".into(), Some(50)),
            ("SyncLimiter".into(), Some(3)),
        ]);
        let applied = replace(original, &desired).unwrap();
        assert_eq!(properties(&applied).unwrap(), desired);
        assert_eq!(replace(&applied, &before).unwrap(), original);
        assert!(properties("[Framerate]\nLimit=1\nlimit=2").is_err());
        assert!(applied.contains("Custom=keep\r\n"));
        assert!(applied.contains("[OSD]\r\nColor=red\r\n"));
    }
    #[test]
    fn profile_restore_survives_rtss_ui_file_lock_and_preserves_user_fields() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let directory = tempfile::tempdir()?;
        std::fs::create_dir(directory.path().join("Profiles"))?;
        let path = directory.path().join("Profiles/Global");
        let original = "[Framerate]\r\nLimit=60\r\nCustom=keep\r\n[OSD]\r\nColor=red\r\n";
        std::fs::write(&path, original)?;
        let _lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
            .open(&path)?;
        let values = BTreeMap::from([
            ("Limit".into(), Some(60000)),
            ("LimitDenominator".into(), Some(1001)),
            ("SyncLimiter".into(), Some(2)),
        ]);
        write_profile(directory.path(), &values)?;
        assert_eq!(properties(&read(directory.path())?)?, values);
        write_profile(directory.path(), &properties(original)?)?;
        assert_eq!(read(directory.path())?, original);
        Ok(())
    }
    #[test]
    fn unchanged_profile_restores_without_write_or_delete_access() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let directory = tempfile::tempdir()?;
        std::fs::create_dir(directory.path().join("Profiles"))?;
        let path = directory.path().join("Profiles/Global");
        let original = "[Framerate]\nLimit=120\nLimitDenominator=1\nSyncLimiter=1\nCustom=keep\n";
        std::fs::write(&path, original)?;
        let _lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&path)?;
        write_profile(directory.path(), &properties(original)?)?;
        assert!(
            write_profile(
                directory.path(),
                &BTreeMap::from([("Limit".into(), Some(60))])
            )
            .is_err()
        );
        assert_eq!(read(directory.path())?, original);
        Ok(())
    }
}
