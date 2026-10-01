//! RTSS SDK calls run in a short-lived Rust worker, keeping a stalled third-party
//! message loop outside the streaming process. The profile's unknown fields survive.
use crate::process::{Process, Target};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub const KEYS: [&str; 3] = ["Limit", "LimitDenominator", "SyncLimiter"];
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
    let process = Process::spawn(
        &executable,
        &[],
        Some(root),
        Target::User { elevated: false },
        &BTreeMap::new(),
        true,
    )?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(Some(process))
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
pub struct Request {
    pub root: PathBuf,
    /// Only the limiter-disable bit is touched; other RTSS flags are retained.
    pub disabled: Option<bool>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub flags: u32,
    pub values: BTreeMap<String, Option<u32>>,
}
pub fn call(root: &Path, directory: &Path, disabled: Option<bool>) -> Result<Reply> {
    let name = format!(
        "rtss-worker-{:x}.json",
        u64::from_le_bytes(butterpollo_core::crypto::random())
    );
    let request = directory.join(name);
    let response = request.with_extension("reply.json");
    butterpollo_core::state::write_json(
        &request,
        &Request {
            root: root.into(),
            disabled,
        },
    )?;
    let result = (|| -> Result<Reply> {
        let worker = Process::spawn(
            &std::env::current_exe()?,
            &["--rtss-worker".into(), request.as_os_str().into()],
            None,
            Target::User { elevated: false },
            &BTreeMap::new(),
            true,
        )?;
        let code = worker.wait(Duration::from_secs(2))?;
        if code != 0 {
            bail!("RTSS worker exited with {code}");
        }
        serde_json::from_slice(&std::fs::read(&response)?).context("invalid RTSS response")
    })();
    let _ = std::fs::remove_file(request);
    let _ = std::fs::remove_file(response);
    result
}
pub fn worker(path: &Path) -> Result<()> {
    if std::fs::metadata(path)?.len() > 32768 {
        bail!("invalid RTSS request size");
    }
    let request: Request = serde_json::from_slice(&std::fs::read(path)?)?;
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
        if let Some(disabled) = request.disabled {
            set_flags(!4, if disabled { 4 } else { 0 });
        }
        load(c"Global".as_ptr());
        update();
        let mut values = BTreeMap::new();
        for (key, property) in KEYS.into_iter().zip([
            c"FramerateLimit",
            c"FramerateLimitDenominator",
            c"SyncLimiter",
        ]) {
            let mut value = 0;
            let exists = get(property.as_ptr(), &mut value, 4) != 0;
            values.insert(key.into(), exists.then_some(value));
        }
        let reply = Reply {
            flags: flags(),
            values,
        };
        butterpollo_core::state::write_json(&path.with_extension("reply.json"), &reply)
    }
}
pub fn wait_ready(root: &Path, directory: &Path) -> Result<Reply> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match call(root, directory, None) {
            Ok(reply) => return Ok(reply),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
}
