//! Windows facilities setup needs: elevation, commands, services, registry,
//! processes, firewall rules, shortcuts and folder permissions.
use crate::log::line;
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_SUCCESS},
        System::{Registry::*, Services::*, Threading::*},
        UI::Shell::*,
    },
    core::{HSTRING, PCWSTR, PWSTR},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Run a program without a window, logging its command line and output.
/// Returns the exit code; a program still running at `timeout` is killed.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Result<i32> {
    line(format!("> {program} {}", args.join(" ")));
    let mut child = Command::new(program)
        .args(args)
        // A PowerShell 7 parent (a script running setup) leaves its module
        // path behind, and Windows PowerShell 5.1 then fails to load its own
        // modules: the driver scripts could not check signatures.
        .env_remove("PSModulePath")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut stream| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stream.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        })
    })
    .collect();
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    for reader in readers {
        if let Ok(output) = reader.join()
            && !output.trim().is_empty()
        {
            line(output.trim_end());
        }
    }
    let Some(status) = status else {
        bail!("{program} did not finish within {} s", timeout.as_secs());
    };
    let code = status.code().unwrap_or(-1);
    line(format!("  exit code {code}"));
    Ok(code)
}
/// Run a program as LocalSystem through a one-time scheduled task and return
/// its exit code and output. Vibepollo's driver scripts run as SYSTEM under
/// Windows Installer: as an administrator, the display driver's health
/// check cannot open the driver and needlessly reinstalls it.
pub fn run_as_system(program: &str, args: &[&str], timeout: Duration) -> Result<(i32, String)> {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let work = program_data().join("Butterpollo").join("setup-tasks");
    std::fs::create_dir_all(&work)?;
    let id = format!(
        "ButterpolloSetup-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let log = work.join(format!("{id}.log"));
    let script = work.join(format!("{id}.cmd"));
    let command = std::iter::once(program)
        .chain(args.iter().copied())
        .map(|a| {
            if a.is_empty() || a.contains([' ', '\t']) {
                format!("\"{a}\"")
            } else {
                a.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let _ = std::fs::remove_file(&log);
    std::fs::write(
        &script,
        format!(
            // The redirection comes first: "...=0>> file" would redirect handle 0.
            "@echo off\r\n{command} > \"{log}\" 2>&1\r\n>> \"{log}\" echo BUTTERPOLLO-EXIT=%ERRORLEVEL%\r\n",
            log = log.display()
        ),
    )?;
    line(format!("as SYSTEM> {command}"));
    let schtasks = system32("schtasks.exe");
    let task = format!("\"{}\"", script.display());
    let created = run(
        &schtasks,
        &[
            "/Create", "/TN", &id, "/TR", &task, "/SC", "ONCE", "/ST", "23:59", "/RU", "SYSTEM",
            "/RL", "HIGHEST", "/F",
        ],
        Duration::from_secs(60),
    )?;
    if created != 0 {
        bail!("creating the setup task failed ({created})");
    }
    let result = (|| -> Result<(i32, String)> {
        if run(&schtasks, &["/Run", "/TN", &id], Duration::from_secs(60))? != 0 {
            bail!("starting the setup task failed");
        }
        let deadline = Instant::now() + timeout;
        loop {
            let text = std::fs::read(&log)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            if let Some(index) = text.rfind("BUTTERPOLLO-EXIT=") {
                let code = text[index + 17..].trim().parse().unwrap_or(-1);
                let output = text[..index].to_owned();
                if !output.trim().is_empty() {
                    line(output.trim_end());
                }
                line(format!("  exit code {code}"));
                return Ok((code, output));
            }
            if Instant::now() >= deadline {
                bail!("{program} did not finish within {} s", timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    })();
    let _ = run(
        &schtasks,
        &["/Delete", "/TN", &id, "/F"],
        Duration::from_secs(60),
    );
    let _ = std::fs::remove_file(&script);
    let _ = std::fs::remove_file(&log);
    result
}
pub fn elevated() -> bool {
    unsafe { IsUserAnAdmin().as_bool() }
}
/// Start this program again elevated with the same arguments and return its
/// exit code (1223 when the user declines the UAC prompt).
pub fn relaunch_elevated() -> Result<i32> {
    let exe = std::env::current_exe()?;
    let arguments: Vec<String> = std::env::args()
        .skip(1)
        .map(|a| {
            if a.contains([' ', '\t', '"']) || a.is_empty() {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a
            }
        })
        .collect();
    let file = HSTRING::from(exe.as_os_str());
    let parameters = HSTRING::from(arguments.join(" "));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: windows::core::w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: 1,
        ..Default::default()
    };
    unsafe {
        if let Err(error) = ShellExecuteExW(&mut info) {
            if error.code() == windows::Win32::Foundation::ERROR_CANCELLED.to_hresult() {
                return Ok(1223);
            }
            return Err(error.into());
        }
        let process = info.hProcess;
        WaitForSingleObject(process, INFINITE);
        let mut code = 0u32;
        GetExitCodeProcess(process, &mut code)?;
        let _ = CloseHandle(process);
        Ok(code as i32)
    }
}

// --- Services -------------------------------------------------------------

struct ServiceHandle(SC_HANDLE);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}
fn manager() -> Result<ServiceHandle> {
    unsafe {
        Ok(ServiceHandle(OpenSCManagerW(
            PCWSTR::null(),
            PCWSTR::null(),
            SC_MANAGER_ALL_ACCESS,
        )?))
    }
}
fn open_service(name: &str) -> Option<ServiceHandle> {
    let manager = manager().ok()?;
    let name = wide(name);
    unsafe {
        OpenServiceW(manager.0, PCWSTR(name.as_ptr()), SERVICE_ALL_ACCESS)
            .ok()
            .map(ServiceHandle)
    }
}
/// The service's program path (without quotes or arguments), if installed.
pub fn service_program(name: &str) -> Option<PathBuf> {
    let image = registry_string(
        HKEY_LOCAL_MACHINE,
        &format!("SYSTEM\\CurrentControlSet\\Services\\{name}"),
        "ImagePath",
    )?;
    let image = image.trim();
    let program = match image.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => image.split(" -").next().unwrap_or(image).trim(),
    };
    Some(PathBuf::from(program))
}
fn service_state(service: &ServiceHandle) -> Option<SERVICE_STATUS_CURRENT_STATE> {
    let mut status = SERVICE_STATUS::default();
    unsafe { QueryServiceStatus(service.0, &mut status) }
        .ok()
        .map(|_| status.dwCurrentState)
}
/// Stop the service if it is running and wait for it (35 s, then fail).
pub fn stop_service(name: &str) -> Result<()> {
    let Some(service) = open_service(name) else {
        return Ok(());
    };
    if service_state(&service) == Some(SERVICE_STOPPED) {
        return Ok(());
    }
    line(format!("stopping service {name}"));
    let mut status = SERVICE_STATUS::default();
    unsafe {
        let _ = ControlService(service.0, SERVICE_CONTROL_STOP, &mut status);
    }
    let deadline = Instant::now() + Duration::from_secs(35);
    while Instant::now() < deadline {
        if service_state(&service) == Some(SERVICE_STOPPED) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("the {name} service did not stop")
}
pub fn start_service(name: &str) -> Result<()> {
    let service = open_service(name).with_context(|| format!("the {name} service is missing"))?;
    line(format!("starting service {name}"));
    unsafe { StartServiceW(service.0, None) }.with_context(|| format!("starting {name}"))
}
pub fn delete_service(name: &str) -> Result<()> {
    let Some(service) = open_service(name) else {
        return Ok(());
    };
    line(format!("deleting service {name}"));
    unsafe { DeleteService(service.0) }.with_context(|| format!("deleting {name}"))
}
/// Create the service, or point an existing one with this name at `program`.
/// Runs as LocalSystem, starts automatically and restarts after failures.
pub fn install_service(name: &str, display: &str, description: &str, program: &Path) -> Result<()> {
    let manager = manager()?;
    let binary = wide(&format!("\"{}\"", program.display()));
    let wide_name = wide(name);
    let wide_display = wide(display);
    let service = unsafe {
        match OpenServiceW(manager.0, PCWSTR(wide_name.as_ptr()), SERVICE_ALL_ACCESS) {
            Ok(existing) => {
                line(format!("reconfiguring service {name}"));
                let existing = ServiceHandle(existing);
                ChangeServiceConfigW(
                    existing.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    PCWSTR(binary.as_ptr()),
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    windows::core::w!("LocalSystem"),
                    PCWSTR::null(),
                    PCWSTR(wide_display.as_ptr()),
                )?;
                existing
            }
            Err(_) => {
                line(format!("creating service {name}"));
                ServiceHandle(CreateServiceW(
                    manager.0,
                    PCWSTR(wide_name.as_ptr()),
                    PCWSTR(wide_display.as_ptr()),
                    SERVICE_ALL_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    PCWSTR(binary.as_ptr()),
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR::null(),
                )?)
            }
        }
    };
    let mut text = wide(description);
    let description = SERVICE_DESCRIPTIONW {
        lpDescription: PWSTR(text.as_mut_ptr()),
    };
    let mut actions = [
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 5_000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 5_000,
        },
        SC_ACTION {
            Type: SC_ACTION_RESTART,
            Delay: 30_000,
        },
    ];
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 86_400,
        lpRebootMsg: PWSTR::null(),
        lpCommand: PWSTR::null(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DESCRIPTION,
            Some((&description as *const SERVICE_DESCRIPTIONW).cast()),
        )?;
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            Some((&failure as *const SERVICE_FAILURE_ACTIONSW).cast()),
        )?;
    }
    Ok(())
}

// --- Registry -------------------------------------------------------------

/// A string value (REG_SZ or REG_EXPAND_SZ, unexpanded) in the 64-bit view.
pub fn registry_string(root: HKEY, path: &str, value: &str) -> Option<String> {
    registry_string_view(root, path, value, KEY_WOW64_64KEY)
}
pub fn registry_string_view(
    root: HKEY,
    path: &str,
    value: &str,
    view: REG_SAM_FLAGS,
) -> Option<String> {
    let key = open_key(root, path, KEY_READ | view)?;
    let name = wide(value);
    let mut size = 0u32;
    let mut kind = REG_VALUE_TYPE::default();
    unsafe {
        if RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            Some(&mut size),
        ) != ERROR_SUCCESS
            || !matches!(kind, REG_SZ | REG_EXPAND_SZ)
            || size > 1 << 20
        {
            return None;
        }
        let mut buffer = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut bytes = (buffer.len() * 2) as u32;
        if RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut bytes),
        ) != ERROR_SUCCESS
        {
            return None;
        }
        buffer.truncate((bytes as usize) / 2);
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..end]))
    }
}
pub fn registry_dword(root: HKEY, path: &str, value: &str, view: REG_SAM_FLAGS) -> Option<u32> {
    let key = open_key(root, path, KEY_READ | view)?;
    let name = wide(value);
    let mut data = 0u32;
    let mut size = 4u32;
    let mut kind = REG_VALUE_TYPE::default();
    unsafe {
        (RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        ) == ERROR_SUCCESS
            && kind == REG_DWORD)
            .then_some(data)
    }
}
pub struct Key(pub HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
pub fn open_key(root: HKEY, path: &str, access: REG_SAM_FLAGS) -> Option<Key> {
    let path = wide(path);
    let mut key = HKEY::default();
    unsafe {
        (RegOpenKeyExW(root, PCWSTR(path.as_ptr()), None, access, &mut key) == ERROR_SUCCESS)
            .then_some(Key(key))
    }
}
pub fn subkeys(root: HKEY, path: &str, view: REG_SAM_FLAGS) -> Vec<String> {
    let Some(key) = open_key(root, path, KEY_READ | view) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for index in 0.. {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..length as usize]));
    }
    names
}
/// Value names and string data of a key's values.
pub fn values(root: HKEY, path: &str, view: REG_SAM_FLAGS) -> Vec<String> {
    let Some(key) = open_key(root, path, KEY_READ | view) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for index in 0.. {
        let mut name = [0u16; 2048];
        let mut length = name.len() as u32;
        let status = unsafe {
            RegEnumValueW(
                key.0,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..length as usize]));
    }
    names
}
pub fn delete_value(root: HKEY, path: &str, value: &str, view: REG_SAM_FLAGS) {
    if let Some(key) = open_key(root, path, KEY_SET_VALUE | view) {
        let value = wide(value);
        unsafe {
            let _ = RegDeleteValueW(key.0, PCWSTR(value.as_ptr()));
        }
    }
}
pub fn delete_key(root: HKEY, path: &str) {
    let path = wide(path);
    unsafe {
        let _ = RegDeleteTreeW(root, PCWSTR(path.as_ptr()));
        let _ = RegDeleteKeyExW(root, PCWSTR(path.as_ptr()), KEY_WOW64_64KEY.0, None);
    }
}
pub enum Value<'a> {
    Text(&'a str),
    Number(u32),
}
pub fn write_key(root: HKEY, path: &str, values: &[(&str, Value)]) -> Result<()> {
    let path = wide(path);
    let mut key = HKEY::default();
    unsafe {
        let status = RegCreateKeyExW(
            root,
            PCWSTR(path.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE | KEY_WOW64_64KEY,
            None,
            &mut key,
            None,
        );
        if status != ERROR_SUCCESS {
            bail!("cannot create registry key ({})", status.0);
        }
        let key = Key(key);
        for (name, value) in values {
            let name = wide(name);
            let status = match value {
                Value::Text(text) => {
                    let data = wide(text);
                    RegSetValueExW(
                        key.0,
                        PCWSTR(name.as_ptr()),
                        None,
                        REG_SZ,
                        Some(std::slice::from_raw_parts(
                            data.as_ptr().cast(),
                            data.len() * 2,
                        )),
                    )
                }
                Value::Number(number) => RegSetValueExW(
                    key.0,
                    PCWSTR(name.as_ptr()),
                    None,
                    REG_DWORD,
                    Some(&number.to_le_bytes()),
                ),
            };
            if status != ERROR_SUCCESS {
                bail!("cannot write registry value ({})", status.0);
            }
        }
    }
    Ok(())
}

// --- Processes, firewall, permissions, shortcuts --------------------------

/// End every process with this executable name (all sessions).
pub fn kill(names: &[&str]) {
    for name in names {
        let _ = run(
            "taskkill.exe",
            &["/F", "/T", "/IM", name],
            Duration::from_secs(20),
        );
    }
}
pub fn system32(program: &str) -> String {
    let windows = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    format!("{windows}\\System32\\{program}")
}
/// Filesystem canonicalization returns verbatim paths. Shell tools and the
/// firewall require conventional drive/UNC paths instead. Keep file I/O's
/// canonical paths internal, and reject names whose meaning would change.
pub fn win32_path(path: &Path) -> Result<PathBuf> {
    use std::path::{Component, Prefix};
    let text = path
        .to_str()
        .context("the installation path is not valid Unicode")?;
    if let Some(verbatim) = text.strip_prefix(r"\\?\") {
        ensure!(
            !verbatim.contains('/') && !verbatim.split('\\').any(|part| matches!(part, "." | "..")),
            "the installation path cannot be represented without changing its meaning"
        );
    }
    let path = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(disk) = text.strip_prefix(r"\\?\") {
        PathBuf::from(disk)
    } else {
        path.to_owned()
    };
    ensure!(
        path.is_absolute()
            && matches!(path.components().next(), Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(_, _))),
        "the installation path must be an absolute drive or network-share path"
    );
    for component in path.components() {
        if let Component::Normal(name) = component {
            let name = name
                .to_str()
                .context("invalid installation path component")?;
            ensure!(
                !name.ends_with(['.', ' ']),
                "the installation path contains a name unsupported by the Windows shell"
            );
        }
    }
    Ok(path)
}
/// Update the inbound application allowance, or create it on a fresh install.
/// A rejected replacement must leave any existing allowance intact.
pub fn firewall_allow(rule: &str, program: &Path) -> Result<()> {
    firewall_allow_with(rule, program, |args| {
        run(&system32("netsh.exe"), args, Duration::from_secs(60))
    })
}
fn firewall_allow_with(
    rule: &str,
    program: &Path,
    mut execute: impl FnMut(&[&str]) -> Result<i32>,
) -> Result<()> {
    let program = win32_path(program)?;
    let name = format!("name={rule}");
    let application = format!("program={}", program.display());
    let updated = execute(&[
        "advfirewall",
        "firewall",
        "set",
        "rule",
        &name,
        "dir=in",
        "new",
        "action=allow",
        &application,
        "enable=yes",
        "profile=any",
    ])?;
    if updated == 0 {
        return Ok(());
    }
    // A missing rule and other netsh errors share a nonzero exit code. Adding
    // the desired rule handles the former without destroying existing rules
    // when either operation is denied or its parameters are rejected.
    let code = execute(&[
        "advfirewall",
        "firewall",
        "add",
        "rule",
        &name,
        "dir=in",
        "action=allow",
        &application,
        "enable=yes",
        "profile=any",
    ])?;
    if code != 0 {
        bail!(
            "adding the firewall rule for {} failed ({code}); existing rules were kept; see the setup log",
            program.display()
        );
    }
    Ok(())
}
pub fn firewall_remove(rule: &str) {
    let _ = run(
        &system32("netsh.exe"),
        &[
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            &format!("name={rule}"),
        ],
        Duration::from_secs(60),
    );
}
/// Limit a folder to SYSTEM and Administrators (and, if `users_read`, read
/// access for users), replacing inherited permissions, as Vibepollo does
/// for its credentials.
pub fn restrict(folder: &Path, users_read: bool) -> Result<()> {
    let path = folder.display().to_string();
    let mut args = vec![
        path.as_str(),
        "/inheritance:r",
        "/grant:r",
        "*S-1-5-18:(OI)(CI)(F)",
        "/grant:r",
        "*S-1-5-32-544:(OI)(CI)(F)",
    ];
    if users_read {
        args.extend(["/grant:r", "*S-1-5-32-545:(OI)(CI)(RX)"]);
    }
    args.push("/Q");
    let code = run(&system32("icacls.exe"), &args, Duration::from_secs(120))?;
    if code != 0 {
        bail!("setting permissions on {path} failed ({code})");
    }
    Ok(())
}
/// Reset a folder's contents to inherit the folder's permissions.
pub fn inherit_contents(folder: &Path) -> Result<()> {
    let pattern = folder.join("*").display().to_string();
    run(
        &system32("icacls.exe"),
        &[&pattern, "/reset", "/T", "/C", "/Q"],
        Duration::from_secs(300),
    )?;
    Ok(())
}
pub fn shortcut(link: &Path, target: &Path, description: &str) -> Result<()> {
    use windows::Win32::System::Com::*;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| -> Result<()> {
            let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell.SetPath(&HSTRING::from(target.as_os_str()))?;
            if let Some(directory) = target.parent() {
                shell.SetWorkingDirectory(&HSTRING::from(directory.as_os_str()))?;
            }
            shell.SetDescription(&HSTRING::from(description))?;
            shell.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
            let file: IPersistFile = windows::core::Interface::cast(&shell)?;
            file.Save(&HSTRING::from(link.as_os_str()), true)?;
            Ok(())
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}
/// Delete a file now, or at the next restart if it is in use.
pub fn remove_file_later(path: &Path) {
    if std::fs::remove_file(path).is_ok() || !path.exists() {
        return;
    }
    let source = HSTRING::from(path.as_os_str());
    unsafe {
        let _ = windows::Win32::Storage::FileSystem::MoveFileExW(
            &source,
            PCWSTR::null(),
            windows::Win32::Storage::FileSystem::MOVEFILE_DELAY_UNTIL_REBOOT,
        );
    }
}
pub fn program_data() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into()))
}
pub fn program_files() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_consumers_receive_drive_and_unc_paths_without_verbatim_prefixes() -> Result<()> {
        for (input, expected) in [
            (
                r"\\?\C:\Program Files\Butterpollo\butterpollo.exe",
                r"C:\Program Files\Butterpollo\butterpollo.exe",
            ),
            (
                r"\\?\UNC\server\share\Butterpollo ü\butterpollo.exe",
                r"\\server\share\Butterpollo ü\butterpollo.exe",
            ),
            (
                r"C:\Program Files\Butterpollo",
                r"C:\Program Files\Butterpollo",
            ),
            (r"\\server\share\Butterpollo", r"\\server\share\Butterpollo"),
        ] {
            assert_eq!(win32_path(Path::new(input))?, Path::new(expected));
        }
        for invalid in [
            r"relative\host.exe",
            r"C:host.exe",
            r"\\.\PhysicalDrive0",
            r"\\?\GLOBALROOT\Device\host.exe",
            r"\\?\C:\Folder\..\host.exe",
            r"\\?\C:\Folder\.\host.exe",
            r"\\?\C:\Folder.\host.exe",
            r"\\?\C:\Folder \host.exe",
        ] {
            assert!(win32_path(Path::new(invalid)).is_err(), "{invalid}");
        }
        let root = tempfile::tempdir()?;
        let file = root.path().join("Butterpollo ü.exe");
        std::fs::write(&file, b"fixture")?;
        let canonical = std::fs::canonicalize(&file)?;
        assert_eq!(std::fs::canonicalize(win32_path(&canonical)?)?, canonical);
        Ok(())
    }

    #[test]
    fn firewall_updates_or_creates_without_deleting_existing_rules_on_failure() -> Result<()> {
        for (results, expected_operations, succeeds) in [
            (vec![0], vec!["set"], true),
            (vec![1, 0], vec!["set", "add"], true),
            (vec![1, 1], vec!["set", "add"], false),
        ] {
            let mut results = results.into_iter();
            let mut operations = Vec::new();
            let result = firewall_allow_with(
                "Butterpollo",
                Path::new(r"\\?\C:\Program Files\Butterpollo\butterpollo.exe"),
                |args| {
                    assert!(
                        args.contains(&r"program=C:\Program Files\Butterpollo\butterpollo.exe")
                    );
                    assert!(!args.contains(&"delete"));
                    operations.push(args[2].to_owned());
                    Ok(results.next().expect("unexpected firewall operation"))
                },
            );
            assert_eq!(result.is_ok(), succeeds);
            assert_eq!(operations, expected_operations);
        }
        firewall_allow_with("Butterpollo", Path::new(r"\\.\PhysicalDrive0"), |_| {
            panic!("invalid paths must fail before touching firewall rules")
        })
        .unwrap_err();
        Ok(())
    }
}
