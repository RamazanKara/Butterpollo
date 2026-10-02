//! User-session process creation with ownership of the complete Windows job.
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    time::Duration,
};
use windows::{
    Win32::{
        Foundation::*,
        Security::*,
        System::{Environment::*, JobObjects::*, RemoteDesktop::*, Threading::*},
    },
    core::{BOOL, PCWSTR, PWSTR},
};
fn owned(handle: HANDLE) -> OwnedHandle {
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}
fn wide(s: &std::ffi::OsStr) -> Result<Vec<u16>> {
    let mut value: Vec<_> = s.encode_wide().collect();
    if value.contains(&0) {
        bail!("Windows process string contains NUL");
    }
    value.push(0);
    Ok(value)
}
fn quote(s: &std::ffi::OsStr) -> Result<String> {
    let s = s.to_string_lossy();
    if s.contains('\0') {
        bail!("process argument contains NUL");
    }
    let mut result = String::from("\"");
    let mut slashes = 0;
    for ch in s.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '"' {
            result.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            result.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        result.push(ch);
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    Ok(result)
}
fn token() -> Result<OwnedHandle> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_SESSIONID,
            &mut token,
        )?;
        Ok(owned(token))
    }
}
pub fn is_system() -> bool {
    let Ok(token) = token() else { return false };
    unsafe {
        let mut buffer = [0usize; 64];
        let mut needed = 0;
        if GetTokenInformation(
            raw(&token),
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size_of::<[usize; 64]>() as u32,
            &mut needed,
        )
        .is_err()
        {
            return false;
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        IsWellKnownSid(user.User.Sid, WinLocalSystemSid).as_bool()
    }
}
pub enum Target {
    User { elevated: bool },
    SystemSession(u32),
}
fn target_token(target: &Target) -> Result<Option<OwnedHandle>> {
    if !is_system() {
        if matches!(target, Target::SystemSession(_)) {
            bail!("SYSTEM session creation requires the Windows service");
        }
        if matches!(target, Target::User { elevated: true }) {
            let token = token()?;
            let mut info = TOKEN_ELEVATION::default();
            let mut bytes = 0;
            unsafe {
                GetTokenInformation(
                    raw(&token),
                    TokenElevation,
                    Some((&mut info as *mut TOKEN_ELEVATION).cast()),
                    size_of::<TOKEN_ELEVATION>() as u32,
                    &mut bytes,
                )?;
            }
            if info.TokenIsElevated == 0 {
                bail!("this elevated command requires the installed Windows service");
            }
        }
        return Ok(None);
    }
    unsafe {
        let source = match target {
            Target::SystemSession(_) => token()?,
            Target::User { .. } => {
                let mut session = 0;
                ProcessIdToSessionId(GetCurrentProcessId(), &mut session)?;
                let mut user = HANDLE::default();
                WTSQueryUserToken(session, &mut user)
                    .context("no signed-in user in the host session")?;
                let mut user = owned(user);
                if matches!(target, Target::User { elevated: true }) {
                    let mut linked = TOKEN_LINKED_TOKEN::default();
                    let mut bytes = 0;
                    if GetTokenInformation(
                        raw(&user),
                        TokenLinkedToken,
                        Some((&mut linked as *mut TOKEN_LINKED_TOKEN).cast()),
                        size_of::<TOKEN_LINKED_TOKEN>() as u32,
                        &mut bytes,
                    )
                    .is_ok()
                        && !linked.LinkedToken.is_invalid()
                    {
                        user = owned(linked.LinkedToken);
                    }
                }
                user
            }
        };
        let mut primary = HANDLE::default();
        DuplicateTokenEx(
            raw(&source),
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )?;
        let primary = owned(primary);
        if let Target::SystemSession(session) = target {
            SetTokenInformation(
                raw(&primary),
                TokenSessionId,
                (session as *const u32).cast(),
                4,
            )?;
        }
        Ok(Some(primary))
    }
}
struct Environment(*mut std::ffi::c_void);
impl Drop for Environment {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = DestroyEnvironmentBlock(self.0);
            }
        }
    }
}
pub struct Process {
    handle: OwnedHandle,
    job: OwnedHandle,
    host_stop: Option<HostStop>,
    pub pid: u32,
}
pub fn user_environment() -> Result<BTreeMap<String, String>> {
    let token = target_token(&Target::User { elevated: false })?;
    unsafe {
        let mut native = Environment(std::ptr::null_mut());
        CreateEnvironmentBlock(&mut native.0, token.as_ref().map(raw), true)?;
        let mut environment = BTreeMap::new();
        let mut offset = 0;
        while !native.0.is_null() && offset < 1024 * 1024 {
            let p = native.0.cast::<u16>().add(offset);
            if *p == 0 {
                break;
            }
            let mut len = 0;
            while offset + len < 1024 * 1024 && *p.add(len) != 0 {
                len += 1;
            }
            if offset + len >= 1024 * 1024 {
                bail!("process environment exceeds the limit");
            }
            let entry = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let split = if let Some(rest) = entry.strip_prefix('=') {
                rest.find('=').map(|i| i + 1)
            } else {
                entry.find('=')
            };
            if let Some(i) = split {
                environment.insert(entry[..i].to_uppercase(), entry[i + 1..].to_owned());
            }
            offset += len + 1;
        }
        Ok(environment)
    }
}
impl Process {
    /// Keep the supervisor's unnamed stop event alive for the entire child.
    /// The child duplicates a wait-only handle; handle inheritance cannot cross
    /// Windows sessions, and session-local object names cannot reach the service.
    pub fn spawn_host(
        program: &Path,
        args: &[OsString],
        directory: Option<&Path>,
        target: Target,
    ) -> Result<Self> {
        let stop = HostStop::new()?;
        let mut args = args.to_vec();
        args.extend(["--service-stop-source".into(), stop.source().into()]);
        let mut process = Self::spawn(program, &args, directory, target, &BTreeMap::new(), true)?;
        process.host_stop = Some(stop);
        Ok(process)
    }
    pub fn spawn(
        program: &Path,
        args: &[OsString],
        directory: Option<&Path>,
        target: Target,
        extras: &BTreeMap<String, String>,
        hidden: bool,
    ) -> Result<Self> {
        let mut command = quote(program.as_os_str())?;
        for arg in args {
            command.push(' ');
            command.push_str(&quote(arg)?);
        }
        Self::spawn_command(program, &command, directory, target, extras, hidden, false)
    }
    pub fn spawn_detached(program: &Path, args: &[OsString], target: Target) -> Result<()> {
        let mut command = quote(program.as_os_str())?;
        for arg in args {
            command.push(' ');
            command.push_str(&quote(arg)?);
        }
        Self::spawn_command(
            program,
            &command,
            None,
            target,
            &BTreeMap::new(),
            true,
            true,
        )?;
        Ok(())
    }
    pub fn shell(
        command: &str,
        directory: Option<&Path>,
        elevated: bool,
        extras: &BTreeMap<String, String>,
    ) -> Result<Self> {
        Self::shell_inner(command, directory, elevated, extras, false)
    }
    pub fn shell_detached(
        command: &str,
        directory: Option<&Path>,
        elevated: bool,
        extras: &BTreeMap<String, String>,
    ) -> Result<()> {
        Self::shell_inner(command, directory, elevated, extras, true)?;
        Ok(())
    }
    fn shell_inner(
        command: &str,
        directory: Option<&Path>,
        elevated: bool,
        extras: &BTreeMap<String, String>,
        detached: bool,
    ) -> Result<Self> {
        if command.contains('\0') {
            bail!("shell command contains NUL");
        }
        let executable = std::path::PathBuf::from(
            std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()),
        )
        .join("System32")
        .join("cmd.exe");
        let line = format!("{} /D /S /C \"{command}\"", quote(executable.as_os_str())?);
        Self::spawn_command(
            &executable,
            &line,
            directory,
            Target::User { elevated },
            extras,
            true,
            detached,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn spawn_command(
        program: &Path,
        line: &str,
        directory: Option<&Path>,
        target: Target,
        extras: &BTreeMap<String, String>,
        hidden: bool,
        detached: bool,
    ) -> Result<Self> {
        let token = target_token(&target)?;
        let program = wide(program.as_os_str())?;
        let directory = directory.map(|p| wide(p.as_os_str())).transpose()?;
        let mut line = wide(std::ffi::OsStr::new(line))?;
        unsafe {
            let mut native = Environment(std::ptr::null_mut());
            CreateEnvironmentBlock(&mut native.0, token.as_ref().map(raw), true)?;
            let mut environment: BTreeMap<String, String> = BTreeMap::new();
            let mut offset = 0usize;
            while !native.0.is_null() && offset < 1024 * 1024 {
                let p = native.0.cast::<u16>().add(offset);
                if *p == 0 {
                    break;
                }
                let mut len = 0;
                while offset + len < 1024 * 1024 && *p.add(len) != 0 {
                    len += 1;
                }
                if offset + len >= 1024 * 1024 {
                    bail!("process environment exceeds the limit");
                }
                let entry = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
                let split = if let Some(rest) = entry.strip_prefix('=') {
                    rest.find('=').map(|i| i + 1)
                } else {
                    entry.find('=')
                };
                if let Some(i) = split {
                    let (name, value) = (&entry[..i], &entry[i + 1..]);
                    environment.insert(name.to_uppercase(), value.to_owned());
                }
                offset += len + 1;
            }
            for (key, value) in extras {
                if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                    bail!("invalid process environment variable");
                }
                environment.insert(key.to_uppercase(), value.clone());
            }
            let mut env: Vec<u16> = environment
                .iter()
                .flat_map(|(k, v)| format!("{k}={v}\0").encode_utf16().collect::<Vec<_>>())
                .collect();
            env.extend([0, 0]);
            let job = owned(CreateJobObjectW(None, PCWSTR::null())?);
            let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                    LimitFlags: JOB_OBJECT_LIMIT_BREAKAWAY_OK
                        | if detached {
                            JOB_OBJECT_LIMIT(0)
                        } else {
                            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                        },
                    ..Default::default()
                },
                ..Default::default()
            };
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            let mut desktop: Vec<u16> = "winsta0\\default\0".encode_utf16().collect();
            let startup = STARTUPINFOW {
                cb: size_of::<STARTUPINFOW>() as u32,
                lpDesktop: PWSTR(desktop.as_mut_ptr()),
                dwFlags: if hidden {
                    STARTF_USESHOWWINDOW
                } else {
                    STARTUPINFOW_FLAGS(0)
                },
                wShowWindow: if hidden { 0 } else { 1 },
                ..Default::default()
            };
            let mut info = PROCESS_INFORMATION::default();
            let flags = CREATE_SUSPENDED
                | CREATE_UNICODE_ENVIRONMENT
                | if detached {
                    CREATE_BREAKAWAY_FROM_JOB
                } else {
                    PROCESS_CREATION_FLAGS(0)
                }
                | if hidden {
                    CREATE_NO_WINDOW
                } else {
                    PROCESS_CREATION_FLAGS(0)
                };
            let current = directory
                .as_ref()
                .map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr()));
            let mut create = |flags| -> windows::core::Result<()> {
                if let Some(token) = &token {
                    CreateProcessAsUserW(
                        Some(raw(token)),
                        PCWSTR(program.as_ptr()),
                        Some(PWSTR(line.as_mut_ptr())),
                        None,
                        None,
                        false,
                        flags,
                        Some(env.as_ptr().cast()),
                        current,
                        &startup,
                        &mut info,
                    )?;
                } else {
                    CreateProcessW(
                        PCWSTR(program.as_ptr()),
                        Some(PWSTR(line.as_mut_ptr())),
                        None,
                        None,
                        false,
                        flags,
                        Some(env.as_ptr().cast()),
                        current,
                        &startup,
                        &mut info,
                    )?;
                }
                Ok(())
            };
            match create(flags) {
                Err(e)
                    if detached
                        && e.code()
                            == windows::core::HRESULT::from_win32(ERROR_ACCESS_DENIED.0) =>
                {
                    create(PROCESS_CREATION_FLAGS(
                        flags.0 & !CREATE_BREAKAWAY_FROM_JOB.0,
                    ))?;
                }
                result => result?,
            }
            let process = Self {
                handle: owned(info.hProcess),
                job,
                host_stop: None,
                pid: info.dwProcessId,
            };
            let thread = owned(info.hThread);
            if let Err(e) = AssignProcessToJobObject(raw(&process.job), raw(&process.handle)) {
                let _ = TerminateProcess(raw(&process.handle), 1);
                return Err(e.into());
            }
            if ResumeThread(raw(&thread)) == u32::MAX {
                let _ = TerminateProcess(raw(&process.handle), 1);
                bail!("cannot resume the application process");
            }
            Ok(process)
        }
    }
    pub fn exit_code(&self) -> Result<Option<u32>> {
        unsafe {
            if WaitForSingleObject(raw(&self.handle), 0) == WAIT_TIMEOUT {
                return Ok(None);
            }
            let mut code = 0;
            GetExitCodeProcess(raw(&self.handle), &mut code)?;
            Ok(Some(code))
        }
    }
    pub fn active_processes(&self) -> Result<u32> {
        unsafe {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            QueryInformationJobObject(
                Some(raw(&self.job)),
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                None,
            )?;
            Ok(info.ActiveProcesses)
        }
    }
    pub fn process_ids(&self) -> Result<Vec<u32>> {
        let mut words = vec![0usize; 4098];
        let list = words.as_mut_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
        unsafe {
            QueryInformationJobObject(
                Some(raw(&self.job)),
                JobObjectBasicProcessIdList,
                list.cast(),
                (words.len() * size_of::<usize>()) as u32,
                None,
            )?;
            let count = (*list).NumberOfProcessIdsInList as usize;
            if count > 4096 {
                bail!("application process group exceeds its limit");
            }
            Ok(
                std::slice::from_raw_parts((*list).ProcessIdList.as_ptr(), count)
                    .iter()
                    .filter_map(|id| u32::try_from(*id).ok())
                    .collect(),
            )
        }
    }
    pub fn wait(&self, timeout: Duration) -> Result<u32> {
        unsafe {
            match WaitForSingleObject(
                raw(&self.handle),
                timeout.as_millis().min(u32::MAX as u128) as u32,
            ) {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => bail!("process timed out"),
                _ => return Err(windows::core::Error::from_thread().into()),
            }
        }
        self.exit_code()?.context("process still running")
    }
    pub fn stop(&self) -> Result<()> {
        unsafe {
            TerminateJobObject(raw(&self.job), 0)?;
        }
        self.wait(Duration::from_secs(10))?;
        Ok(())
    }
    pub fn stop_graceful(&self, timeout: Duration) -> Result<()> {
        use windows::Win32::UI::WindowsAndMessaging::*;
        struct Closing {
            ids: std::collections::BTreeSet<u32>,
            sent: bool,
        }
        unsafe extern "system" fn close_window(window: HWND, data: LPARAM) -> BOOL {
            unsafe {
                let closing = &mut *(data.0 as *mut Closing);
                let mut pid = 0;
                GetWindowThreadProcessId(window, Some(&mut pid));
                if closing.ids.contains(&pid)
                    && PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok()
                {
                    closing.sent = true;
                }
                BOOL(1)
            }
        }
        if timeout.is_zero() || self.active_processes()? == 0 {
            return self.stop();
        }
        let mut words = vec![0usize; 4098];
        let list = words.as_mut_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
        unsafe {
            QueryInformationJobObject(
                Some(raw(&self.job)),
                JobObjectBasicProcessIdList,
                list.cast(),
                (words.len() * size_of::<usize>()) as u32,
                None,
            )?;
            let count = (*list).NumberOfProcessIdsInList as usize;
            if count > 4096 {
                bail!("application process group exceeds its limit");
            }
            let ids = std::slice::from_raw_parts((*list).ProcessIdList.as_ptr(), count)
                .iter()
                .filter_map(|id| u32::try_from(*id).ok())
                .collect();
            let mut closing = Closing { ids, sent: false };
            EnumWindows(
                Some(close_window),
                LPARAM((&mut closing as *mut Closing) as isize),
            )?;
            if closing.sent {
                let deadline = std::time::Instant::now() + timeout;
                while self.active_processes()? != 0 && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        self.stop()
    }
    pub fn shutdown_host(&self) -> Result<HostShutdown> {
        if let Some(code) = self.exit_code()? {
            return Ok(HostShutdown::Graceful(code));
        }
        self.host_stop
            .as_ref()
            .context("host has no supervisor stop channel")?
            .request()?;
        match self.wait(Duration::from_secs(20)) {
            Ok(code) => Ok(HostShutdown::Graceful(code)),
            Err(error) => {
                tracing::warn!(pid=self.pid, %error, "host shutdown timed out; terminating its owned job");
                self.stop()?;
                Ok(HostShutdown::Forced)
            }
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum HostShutdown {
    Graceful(u32),
    Forced,
}
struct HostStop(OwnedHandle);
impl HostStop {
    fn new() -> Result<Self> {
        Ok(Self(owned(unsafe {
            CreateEventW(None, true, false, PCWSTR::null())?
        })))
    }
    fn source(&self) -> String {
        format!(
            "{}:{:x}",
            unsafe { GetCurrentProcessId() },
            self.0.as_raw_handle() as usize
        )
    }
    fn request(&self) -> Result<()> {
        unsafe {
            SetEvent(raw(&self.0))?;
        }
        Ok(())
    }
}
fn parse_stop_source(source: &str) -> Result<(u32, usize)> {
    let (pid, handle) = source
        .split_once(':')
        .context("invalid supervisor stop source")?;
    if pid.is_empty()
        || !pid.bytes().all(|b| b.is_ascii_digit())
        || handle.is_empty()
        || !handle.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("invalid supervisor stop source");
    }
    let pid: u32 = pid.parse()?;
    let handle = usize::from_str_radix(handle, 16)?;
    if pid == 0 || handle == 0 || handle == usize::MAX {
        bail!("invalid supervisor stop source");
    }
    Ok((pid, handle))
}
fn parent_pid() -> Result<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let snapshot = owned(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(raw(&snapshot), &mut entry)?;
        loop {
            if entry.th32ProcessID == GetCurrentProcessId() {
                return Ok(entry.th32ParentProcessID);
            }
            Process32NextW(raw(&snapshot), &mut entry)
                .context("host parent process unavailable")?;
        }
    }
}
fn duplicate_stop_event(pid: u32, handle: usize) -> Result<OwnedHandle> {
    unsafe {
        let parent = owned(OpenProcess(PROCESS_DUP_HANDLE, false, pid)?);
        let mut duplicate = HANDLE::default();
        DuplicateHandle(
            raw(&parent),
            HANDLE(handle as *mut _),
            GetCurrentProcess(),
            &mut duplicate,
            SYNCHRONIZATION_SYNCHRONIZE.0,
            false,
            DUPLICATE_HANDLE_OPTIONS(0),
        )?;
        Ok(owned(duplicate))
    }
}
pub struct StopSignal(Option<OwnedHandle>);
impl StopSignal {
    pub fn new(source: Option<&str>) -> Result<Self> {
        let Some(source) = source else {
            return Ok(Self(None));
        };
        let (pid, handle) = parse_stop_source(source)?;
        if pid != parent_pid()? {
            bail!("stop source is not this host's supervisor");
        }
        Ok(Self(Some(
            duplicate_stop_event(pid, handle).context("connecting supervisor stop event")?,
        )))
    }
    pub fn requested(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|event| unsafe { WaitForSingleObject(raw(event), 0) == WAIT_OBJECT_0 })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn supervisor_event_is_wait_only_and_outlives_the_source_handle() -> Result<()> {
        let source = HostStop::new()?;
        let (pid, handle) = parse_stop_source(&source.source())?;
        let signal = StopSignal(Some(duplicate_stop_event(pid, handle)?));
        assert!(!signal.requested());
        let event = raw(signal.0.as_ref().unwrap());
        assert_eq!(
            unsafe { SetEvent(event) }.unwrap_err().code(),
            windows::core::HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
        );
        source.request()?;
        drop(source);
        assert!(signal.requested());
        assert!(unsafe { ResetEvent(event) }.is_err());
        Ok(())
    }
    #[test]
    fn shutdown_source_rejects_invalid_handles_and_foreign_parents() -> Result<()> {
        for source in [
            "",
            "1",
            "0:1",
            "1:0",
            "1:-1",
            "1:1:2",
            "+1:2",
            "1:xyz",
            "4294967296:1",
            "1:ffffffffffffffffffffffff",
        ] {
            assert!(parse_stop_source(source).is_err(), "{source}");
        }
        assert!(StopSignal::new(Some(&HostStop::new()?.source())).is_err());
        assert!(!StopSignal::new(None)?.requested());
        Ok(())
    }
    #[test]
    fn shell_runs_grouped_commands_with_quoted_output_paths() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("quoted output.log");
        let command = format!("(echo NATIVE_SHELL) 1>\"{}\" 2>&1", output.display());
        let child =
            super::Process::shell(&command, Some(directory.path()), false, &Default::default())
                .unwrap();
        assert_eq!(child.wait(std::time::Duration::from_secs(5)).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(output).unwrap().trim(),
            "NATIVE_SHELL"
        );
    }
    use super::*;
    #[test]
    fn dropping_the_job_terminates_the_shell_and_its_child() {
        let process =
            Process::shell("ping -n 30 127.0.0.1 >nul", None, false, &BTreeMap::new()).unwrap();
        let mut ids = [0usize; 34];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let count = loop {
            unsafe {
                QueryInformationJobObject(
                    Some(raw(&process.job)),
                    JobObjectBasicProcessIdList,
                    ids.as_mut_ptr().cast(),
                    std::mem::size_of_val(&ids) as u32,
                    None,
                )
                .unwrap();
            }
            let count = unsafe { *ids.as_ptr().cast::<u32>().add(1) } as usize;
            if count >= 2 {
                break count;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child never joined the owned Windows job"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(count <= 32);
        let children: Vec<_> = ids[1..=count]
            .iter()
            .map(|id| {
                owned(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, *id as u32).unwrap() })
            })
            .collect();
        drop(process);
        for child in children {
            assert_eq!(
                unsafe { WaitForSingleObject(raw(&child), 5000) },
                WAIT_OBJECT_0
            );
        }
    }
}
