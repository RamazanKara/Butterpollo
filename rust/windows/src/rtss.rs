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
    ffi::{CStr, c_char},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub const KEYS: [&str; 3] = ["Limit", "LimitDenominator", "SyncLimiter"];
const PIPE_PREFIX: &str = r"\\.\pipe\Butterpollo.Rtss.";
/// For each RTSS call; a stalled RTSS must not hold up a stream.
const TIMEOUT: Duration = Duration::from_secs(2);
/// Starting the helper as SYSTEM in the user's session can take longer.
const STARTUP: Duration = Duration::from_secs(5);
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
    // Configured paths may contain a trailing separator, dot components or a
    // directory junction. Compare the actual directory, not its spelling.
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // SAFETY: `entry` has dwSize set to its size, `size` is the capacity of `path` in UTF-16 units,
    // and the snapshot and each process handle are closed once.
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
                        std::fs::canonicalize(p)
                            .unwrap_or_else(|_| p.to_path_buf())
                            .to_string_lossy()
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
    // RTSSHooks hands profile calls to RTSS's message loop. Until that loop
    // runs they block, as Vibepollo found: a host check's restore to an RTSS
    // started a moment earlier timed out in the helper.
    if !process.wait_input_idle(Duration::from_secs(3)) {
        tracing::warn!("RTSS did not finish starting within 3 s");
        std::thread::sleep(Duration::from_millis(300));
    }
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
                    "RTSS requires administrator privileges. Start RTSS manually as administrator before streaming, or run Rubylight through its installed Windows service",
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
    /// Written through the profile SDK and saved before the reload.
    set: BTreeMap<String, u32>,
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
    /// The RTSS call the helper makes next, so a timeout can name it.
    Step {
        step: String,
    },
    Done {
        reply: Reply,
    },
    Error {
        message: String,
    },
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
        helper_target(crate::process::is_system(), crate::process::current_session)?,
        &BTreeMap::new(),
        true,
    )
    .context("start RTSS helper in the signed-in user's session")?;
    let started = Instant::now();
    let check = |deadline: Instant, step: &str| -> Result<()> {
        ensure!(
            worker.exit_code()?.is_none(),
            "RTSS helper exited during {step}"
        );
        ensure!(
            Instant::now() < deadline,
            "RTSS helper timed out after {} ms in {step}; RTSS may be unresponsive",
            started.elapsed().as_millis()
        );
        std::thread::sleep(Duration::from_millis(2));
        Ok(())
    };
    let mut step = String::from("starting");
    let mut deadline = started + STARTUP;
    while !pipe.connected(worker.pid)? {
        check(deadline, &step)?;
    }
    pipe.send(request)?;
    step = "reading the request".into();
    deadline = Instant::now() + TIMEOUT;
    let response = loop {
        match pipe.receive::<Response>()? {
            Some(Response::Step { step: next }) => {
                step = next;
                deadline = Instant::now() + TIMEOUT;
            }
            Some(response) => break response,
            None => check(deadline, &step)?,
        }
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
        Response::Step { .. } => unreachable!("steps are read above"),
    }
}
/// RTSS runs elevated, and Windows drops an unelevated caller's messages to
/// it, so the SDK's UpdateProfiles never reached RTSS: it kept showing and
/// enforcing the old limit. Like Vibepollo, whose SYSTEM host calls the SDK
/// itself, the service runs the helper as SYSTEM in its own (the user's)
/// session. A portable host's helper keeps the host's token.
fn helper_target(service: bool, session: impl FnOnce() -> Result<u32>) -> Result<Target> {
    Ok(if service {
        Target::SystemSession(session()?)
    } else {
        Target::User { elevated: false }
    })
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
    let report = |step: &str| {
        // Progress only; the reply below reports any failure.
        let _ = pipe.send(&Response::Step { step: step.into() });
    };
    let response = match execute(&request, &report) {
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
/// RTSSHooks' profile and flag calls; tests substitute a simulated RTSS.
trait Sdk {
    /// Reads the Global profile into the SDK's buffer.
    fn load(&self);
    fn set(&self, property: &CStr, value: u32) -> Result<bool>;
    fn save(&self) -> Result<()>;
    /// Asks RTSS to apply its profiles to running applications.
    fn update(&self);
    fn get(&self, property: &CStr) -> Option<u32>;
    fn flags(&self) -> u32;
    fn set_flags(&self, and: u32, xor: u32);
}
struct Hooks {
    load: unsafe extern "C" fn(*const c_char),
    save: Option<unsafe extern "C" fn(*const c_char)>,
    update: unsafe extern "C" fn(),
    flags: unsafe extern "C" fn() -> u32,
    set_flags: unsafe extern "C" fn(u32, u32) -> u32,
    get: unsafe extern "C" fn(*const c_char, *mut u32, u32) -> i32,
    set: Option<unsafe extern "C" fn(*const c_char, *const u32, u32) -> i32>,
    // The function pointers above are valid only while this stays loaded.
    _library: libloading::Library,
}
impl Hooks {
    fn open(root: &Path) -> Result<Self> {
        // SAFETY: the hooks DLL is RTSS's own, from the configured RTSS folder, and loading it
        // runs only that trusted code.
        let library =
            unsafe { libloading::Library::new(hooks(root).context("RTSS hooks missing")?)? };
        // SAFETY: the symbol types match the RTSSHooks exports, and `_library` keeps every copied
        // function pointer loaded for the lifetime of `Hooks`.
        unsafe {
            Ok(Self {
                load: *library.get(b"LoadProfile\0")?,
                save: library.get(b"SaveProfile\0").ok().map(|f| *f),
                update: *library.get(b"UpdateProfiles\0")?,
                flags: *library.get(b"GetFlags\0")?,
                set_flags: *library.get(b"SetFlags\0")?,
                get: *library.get(b"GetProfileProperty\0")?,
                set: library.get(b"SetProfileProperty\0").ok().map(|f| *f),
                _library: library,
            })
        }
    }
}
// The pointers come from the loaded RTSSHooks exports (see `Hooks::open`).
impl Sdk for Hooks {
    fn load(&self) {
        // SAFETY: the profile name is a NUL-terminated C literal.
        unsafe { (self.load)(c"".as_ptr()) }
    }
    fn set(&self, property: &CStr, value: u32) -> Result<bool> {
        let set = self.set.context("this RTSS has no SetProfileProperty")?;
        // SAFETY: the name is NUL-terminated and `value` is readable for the 4 bytes passed.
        Ok(unsafe { set(property.as_ptr(), &value, 4) } != 0)
    }
    fn save(&self) -> Result<()> {
        let save = self.save.context("this RTSS has no SaveProfile")?;
        // SAFETY: the profile name is a NUL-terminated C literal.
        unsafe { save(c"".as_ptr()) };
        Ok(())
    }
    fn update(&self) {
        // SAFETY: UpdateProfiles takes no arguments.
        unsafe { (self.update)() }
    }
    fn get(&self, property: &CStr) -> Option<u32> {
        let mut value = 0;
        // SAFETY: the name is NUL-terminated and `value` is writable for the 4 bytes passed.
        (unsafe { (self.get)(property.as_ptr(), &mut value, 4) } != 0).then_some(value)
    }
    fn flags(&self) -> u32 {
        // SAFETY: GetFlags takes no arguments.
        unsafe { (self.flags)() }
    }
    fn set_flags(&self, and: u32, xor: u32) {
        // SAFETY: SetFlags takes two masks by value.
        unsafe { (self.set_flags)(and, xor) };
    }
}
fn execute(request: &Request, report: &dyn Fn(&str)) -> Result<Reply> {
    report("loading RTSSHooks");
    let sdk = Hooks::open(&request.root)?;
    run(request, &Reporting { sdk, report })
}
/// Names each RTSS call to the service before making it.
struct Reporting<'a, S> {
    sdk: S,
    report: &'a dyn Fn(&str),
}
impl<S: Sdk> Sdk for Reporting<'_, S> {
    fn load(&self) {
        (self.report)("LoadProfile");
        self.sdk.load()
    }
    fn set(&self, property: &CStr, value: u32) -> Result<bool> {
        (self.report)(&format!(
            "SetProfileProperty {}",
            property.to_string_lossy()
        ));
        self.sdk.set(property, value)
    }
    fn save(&self) -> Result<()> {
        (self.report)("SaveProfile");
        self.sdk.save()
    }
    fn update(&self) {
        (self.report)("UpdateProfiles");
        self.sdk.update()
    }
    fn get(&self, property: &CStr) -> Option<u32> {
        (self.report)(&format!(
            "GetProfileProperty {}",
            property.to_string_lossy()
        ));
        self.sdk.get(property)
    }
    fn flags(&self) -> u32 {
        (self.report)("GetFlags");
        self.sdk.flags()
    }
    fn set_flags(&self, and: u32, xor: u32) {
        (self.report)("SetFlags");
        self.sdk.set_flags(and, xor)
    }
}
/// The order matches Vibepollo, which reaches RTSS 7.3.7: properties, save,
/// UpdateProfiles, then the limiter flag and one more load and update.
fn run(request: &Request, sdk: &impl Sdk) -> Result<Reply> {
    sdk.load();
    if !request.set.is_empty() {
        for (key, value) in &request.set {
            let property = property(key).context("unknown RTSS property")?;
            ensure!(sdk.set(property, *value)?, "RTSS rejected {key}={value}");
        }
        sdk.save()?;
        sdk.update();
    }
    if let Some(disabled) = request.disabled {
        sdk.set_flags(!4, if disabled { 4 } else { 0 });
    }
    if request.reload {
        sdk.load();
        sdk.update();
    }
    Ok(Reply {
        flags: sdk.flags(),
        values: KEYS
            .into_iter()
            .zip(PROPERTIES)
            .map(|(key, property)| (key.into(), sdk.get(property)))
            .collect(),
    })
}
const PROPERTIES: [&CStr; 3] = [
    c"FramerateLimit",
    c"FramerateLimitDenominator",
    c"SyncLimiter",
];
fn property(key: &str) -> Option<&'static CStr> {
    KEYS.iter().position(|k| *k == key).map(|i| PROPERTIES[i])
}
/// RTSS's value for a key its profile leaves out.
fn default_value(key: &str) -> u32 {
    u32::from(key == "LimitDenominator")
}
/// RTSS.exe's file version: major, minor, build, revision.
pub fn version(root: &Path) -> Option<[u16; 4]> {
    use windows::{
        Win32::Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
        },
        core::{HSTRING, w},
    };
    let path = HSTRING::from(executable(root)?.as_os_str());
    // SAFETY: `data` has the size Windows reported, VerQueryValueW returns a pointer into it
    // with the length checked here, and the structure is read unaligned before `data` drops.
    unsafe {
        let size = GetFileVersionInfoSizeW(&path, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&path, None, size, data.as_mut_ptr().cast()).ok()?;
        let mut info = std::ptr::null_mut();
        let mut length = 0;
        if !VerQueryValueW(data.as_ptr().cast(), w!("\\"), &mut info, &mut length).as_bool()
            || info.is_null()
            || (length as usize) < size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = std::ptr::read_unaligned(info.cast::<VS_FIXEDFILEINFO>());
        Some([
            (info.dwFileVersionMS >> 16) as u16,
            info.dwFileVersionMS as u16,
            (info.dwFileVersionLS >> 16) as u16,
            info.dwFileVersionLS as u16,
        ])
    }
}
/// RTSS 7.3.7 keeps running the Global profile it has open: UpdateProfiles
/// after a direct file edit leaves the live limit unchanged (Vibepollo #378,
/// #479), so the values must go through its profile SDK. That release's
/// RTSS.exe still says 7.3.5.28314. Older RTSS reloads the file, and its SDK
/// mishandles fractional limits.
pub fn profile_sdk(version: Option<[u16; 4]>) -> bool {
    version.is_some_and(|v| v == [7, 3, 5, 28314] || (v[0], v[1], v[2]) >= (7, 3, 7))
}
pub fn query(root: &Path) -> Result<Reply> {
    call(&Request {
        root: root.into(),
        set: BTreeMap::new(),
        disabled: None,
        reload: false,
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
/// writes the protected profile, so it holds the values even if RTSS stalls;
/// the helper then gives RTSS 7.3.7 the same values through its SDK, reloads
/// and verifies. Older SDK setters reject some rational numerators.
pub fn apply(
    root: &Path,
    values: &BTreeMap<String, Option<u32>>,
    disabled: Option<bool>,
) -> Result<Reply> {
    apply_with(root, values, disabled, profile_sdk(version(root)), call)
}
fn apply_with(
    root: &Path,
    values: &BTreeMap<String, Option<u32>>,
    disabled: Option<bool>,
    sdk: bool,
    call: impl FnOnce(&Request) -> Result<Reply>,
) -> Result<Reply> {
    ensure!(
        values.keys().all(|key| KEYS.contains(&key.as_str())),
        "unknown RTSS property"
    );
    if !values.is_empty() {
        write_profile(root, values).context("write RTSS Global profile")?;
    }
    // The SDK cannot remove a key; an originally absent one gets RTSS's default.
    let set: BTreeMap<_, _> = if sdk {
        values
            .iter()
            .map(|(key, value)| (key.clone(), value.unwrap_or_else(|| default_value(key))))
            .collect()
    } else {
        BTreeMap::new()
    };
    let reply = call(&Request {
        root: root.into(),
        set: set.clone(),
        disabled,
        reload: true,
    })?;
    // SaveProfile rewrote the file that restoration and crash recovery read.
    let saved = properties(&read(root)?)?;
    for (key, value) in &set {
        ensure!(
            saved[key].unwrap_or_else(|| default_value(key)) == *value,
            "RTSS saved a different {key} than the requested {value}"
        );
    }
    Ok(reply)
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
    fn service_helper_runs_as_system_in_the_host_session() {
        assert!(matches!(
            helper_target(true, || Ok(3)).unwrap(),
            Target::SystemSession(3)
        ));
        assert!(matches!(
            helper_target(false, || unreachable!()).unwrap(),
            Target::User { elevated: false }
        ));
        assert!(helper_target(true, || bail!("no session")).is_err());
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

    #[test]
    fn profile_sdk_starts_at_rtss_737_including_its_mislabelled_build() {
        for (version, sdk) in [
            ([7, 3, 5, 28314], true),
            ([7, 3, 7, 0], true),
            ([7, 3, 8, 1], true),
            ([7, 4, 0, 0], true),
            ([8, 0, 0, 0], true),
            ([7, 3, 5, 26975], false),
            ([7, 3, 6, 27707], false),
            ([7, 3, 4, 0], false),
            ([7, 2, 9, 0], false),
        ] {
            assert_eq!(profile_sdk(Some(version)), sdk, "{version:?}");
        }
        assert!(!profile_sdk(None));
    }

    /// RTSS 7.3.7 as Vibepollo found it: SaveProfile reaches the profile RTSS
    /// has open, UpdateProfiles applies that one, and a file edit does not.
    #[derive(Default)]
    struct Rtss737 {
        disk: std::cell::RefCell<BTreeMap<&'static CStr, u32>>,
        buffer: std::cell::RefCell<BTreeMap<&'static CStr, u32>>,
        open: std::cell::RefCell<BTreeMap<&'static CStr, u32>>,
        live: std::cell::RefCell<BTreeMap<&'static CStr, u32>>,
        flags: std::cell::Cell<u32>,
        calls: std::cell::RefCell<Vec<&'static str>>,
    }
    impl Rtss737 {
        fn new(limit: u32, flags: u32) -> Self {
            let profile = BTreeMap::from([
                (PROPERTIES[0], limit),
                (PROPERTIES[1], 1),
                (PROPERTIES[2], 0),
            ]);
            let rtss = Self::default();
            *rtss.disk.borrow_mut() = profile.clone();
            *rtss.open.borrow_mut() = profile.clone();
            *rtss.live.borrow_mut() = profile;
            rtss.flags.set(flags);
            rtss
        }
        fn live_limit(&self) -> u32 {
            self.live.borrow()[PROPERTIES[0]]
        }
    }
    impl Sdk for Rtss737 {
        fn load(&self) {
            self.calls.borrow_mut().push("load");
            *self.buffer.borrow_mut() = self.disk.borrow().clone();
        }
        fn set(&self, property: &CStr, value: u32) -> Result<bool> {
            self.calls.borrow_mut().push("set");
            let property = PROPERTIES.into_iter().find(|p| *p == property).unwrap();
            self.buffer.borrow_mut().insert(property, value);
            Ok(true)
        }
        fn save(&self) -> Result<()> {
            self.calls.borrow_mut().push("save");
            *self.disk.borrow_mut() = self.buffer.borrow().clone();
            *self.open.borrow_mut() = self.buffer.borrow().clone();
            Ok(())
        }
        fn update(&self) {
            self.calls.borrow_mut().push("update");
            *self.live.borrow_mut() = self.open.borrow().clone();
        }
        fn get(&self, property: &CStr) -> Option<u32> {
            self.buffer.borrow().get(property).copied()
        }
        fn flags(&self) -> u32 {
            self.flags.get()
        }
        fn set_flags(&self, and: u32, xor: u32) {
            self.calls.borrow_mut().push("flags");
            self.flags.set((self.flags.get() & and) ^ xor);
        }
    }
    fn request(set: &[(&str, u32)], disabled: Option<bool>) -> Request {
        Request {
            root: PathBuf::new(),
            set: set.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            disabled,
            reload: true,
        }
    }

    #[test]
    fn rtss_737_takes_the_stream_limit_only_through_its_profile_sdk() -> Result<()> {
        // The user's own 158 FPS limit, with the limiter switched off.
        let rtss = Rtss737::new(158, 4);
        // The file edit and reload the host made before this fix.
        rtss.disk.borrow_mut().insert(PROPERTIES[0], 100);
        let reply = run(&request(&[], Some(false)), &rtss)?;
        assert_eq!(reply.values["Limit"], Some(100));
        assert_eq!(rtss.live_limit(), 158, "the file alone never reaches RTSS");

        let rtss = Rtss737::new(158, 4);
        let applied = [("Limit", 100), ("LimitDenominator", 1), ("SyncLimiter", 1)];
        let reply = run(&request(&applied, Some(false)), &rtss)?;
        assert_eq!(rtss.live_limit(), 100);
        assert_eq!(rtss.live.borrow()[PROPERTIES[2]], 1);
        assert_eq!(reply.flags & 4, 0);
        assert_eq!(
            reply.values,
            applied
                .iter()
                .map(|(k, v)| (k.to_string(), Some(*v)))
                .collect::<BTreeMap<_, _>>()
        );
        // Vibepollo's order: values, save, UpdateProfiles, the flag, reload.
        assert_eq!(
            *rtss.calls.borrow(),
            [
                "load", "set", "set", "set", "save", "update", "flags", "load", "update"
            ]
        );

        // Restoring the user's limit at stream end goes the same way.
        let reply = run(&request(&[("Limit", 158)], Some(true)), &rtss)?;
        assert_eq!(rtss.live_limit(), 158);
        assert_eq!(reply.flags & 4, 4);

        // A query changes nothing.
        let rtss = Rtss737::new(158, 0);
        let query = Request {
            reload: false,
            ..request(&[], None)
        };
        run(&query, &rtss)?;
        assert_eq!(*rtss.calls.borrow(), ["load"]);
        Ok(())
    }

    #[test]
    fn the_helper_names_each_rtss_call_before_making_it() -> Result<()> {
        let steps = std::cell::RefCell::new(Vec::new());
        let report = |step: &str| steps.borrow_mut().push(step.to_string());
        let sdk = Reporting {
            sdk: Rtss737::new(158, 0),
            report: &report,
        };
        run(&request(&[("Limit", 100)], Some(false)), &sdk)?;
        assert_eq!(
            steps.borrow()[..6],
            [
                "LoadProfile",
                "SetProfileProperty FramerateLimit",
                "SaveProfile",
                "UpdateProfiles",
                "SetFlags",
                "LoadProfile",
            ]
        );
        // A timeout after this names the call that never returned.
        assert_eq!(
            steps.borrow().last().unwrap(),
            "GetProfileProperty SyncLimiter"
        );
        Ok(())
    }

    #[test]
    fn rtss_rejecting_a_property_fails_before_saving() {
        struct Rejecting(Rtss737);
        impl Sdk for Rejecting {
            fn load(&self) {
                self.0.load()
            }
            fn set(&self, _: &CStr, _: u32) -> Result<bool> {
                Ok(false)
            }
            fn save(&self) -> Result<()> {
                self.0.save()
            }
            fn update(&self) {
                self.0.update()
            }
            fn get(&self, property: &CStr) -> Option<u32> {
                self.0.get(property)
            }
            fn flags(&self) -> u32 {
                self.0.flags()
            }
            fn set_flags(&self, and: u32, xor: u32) {
                self.0.set_flags(and, xor)
            }
        }
        let rtss = Rejecting(Rtss737::new(158, 0));
        let error = run(&request(&[("Limit", 100)], Some(false)), &rtss).unwrap_err();
        assert!(error.to_string().contains("rejected Limit=100"), "{error}");
        assert_eq!(*rtss.0.calls.borrow(), ["load"]);
        assert_eq!(rtss.0.live_limit(), 158);
    }

    fn profile_fixture(text: &str) -> Result<tempfile::TempDir> {
        let directory = tempfile::tempdir()?;
        std::fs::create_dir(directory.path().join("Profiles"))?;
        std::fs::write(directory.path().join("Profiles/Global"), text)?;
        Ok(directory)
    }
    fn reply(values: &BTreeMap<String, Option<u32>>) -> Reply {
        Reply {
            flags: 0,
            values: values.clone(),
        }
    }

    #[test]
    fn apply_sends_values_through_the_sdk_only_for_rtss_737() -> Result<()> {
        let original = "[Framerate]\r\nLimit=158\r\nLimitDenominator=1\r\nCustom=keep\r\n";
        let applied = BTreeMap::from([
            ("Limit".into(), Some(100)),
            ("LimitDenominator".into(), Some(1)),
            ("SyncLimiter".into(), Some(1)),
        ]);
        for sdk in [false, true] {
            let directory = profile_fixture(original)?;
            let mut sent = None;
            apply_with(directory.path(), &applied, Some(false), sdk, |request| {
                sent = Some((request.set.clone(), request.disabled, request.reload));
                Ok(reply(&applied))
            })?;
            let expected = if sdk {
                BTreeMap::from([
                    ("Limit".into(), 100),
                    ("LimitDenominator".into(), 1),
                    ("SyncLimiter".into(), 1),
                ])
            } else {
                BTreeMap::new()
            };
            assert_eq!(sent, Some((expected, Some(false), true)), "sdk {sdk}");
            // The service's own write keeps the file right even if RTSS stalls.
            assert_eq!(properties(&read(directory.path())?)?, applied);
            assert!(read(directory.path())?.contains("Custom=keep\r\n"));
        }

        // Restoring a key the user's profile never had: the SDK cannot delete
        // it, so RTSS gets its default and may save that.
        let directory =
            profile_fixture("[Framerate]\nLimit=100\nLimitDenominator=1\nSyncLimiter=1\n")?;
        let restore = BTreeMap::from([("Limit".into(), Some(158)), ("SyncLimiter".into(), None)]);
        let mut sent = None;
        apply_with(directory.path(), &restore, Some(true), true, |request| {
            sent = Some(request.set.clone());
            write_profile(
                &request.root,
                &request
                    .set
                    .iter()
                    .map(|(k, v)| (k.clone(), Some(*v)))
                    .collect(),
            )?;
            Ok(reply(&restore))
        })?;
        assert_eq!(
            sent,
            Some(BTreeMap::from([
                ("Limit".into(), 158),
                ("SyncLimiter".into(), 0)
            ]))
        );
        Ok(())
    }

    #[test]
    fn apply_fails_when_rtss_saves_a_different_limit() -> Result<()> {
        let directory = profile_fixture("[Framerate]\nLimit=158\nLimitDenominator=1\n")?;
        let applied = BTreeMap::from([
            ("Limit".into(), Some(100)),
            ("LimitDenominator".into(), Some(1)),
        ]);
        let error = apply_with(directory.path(), &applied, Some(false), true, |request| {
            // SaveProfile from a stale buffer.
            write_profile(
                &request.root,
                &BTreeMap::from([("Limit".into(), Some(158))]),
            )?;
            Ok(reply(&applied))
        })
        .unwrap_err();
        assert!(error.to_string().contains("different Limit"), "{error}");
        Ok(())
    }
}
