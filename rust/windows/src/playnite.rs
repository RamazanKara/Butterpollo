//! Playnite on this PC: where it is installed, where its extensions go, and
//! the named pipe its Rubylight (Vibepollo) plugin serves.

use anyhow::{Context, Result, bail};
use std::{
    fs::File,
    io::{Read, Write},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::SECURITY_IDENTIFICATION,
    System::{
        Pipes::{GetNamedPipeServerSessionId, PeekNamedPipe, WaitNamedPipeW},
        Registry::{HKEY_LOCAL_MACHINE, HKEY_USERS},
        RemoteDesktop::ProcessIdToSessionId,
        Threading::GetCurrentProcessId,
    },
};

use crate::steam::registry_string;

const PIPE: &str = r"\\.\pipe\Sunshine.PlayniteExtension";
pub const PROCESSES: [&str; 2] = ["Playnite.DesktopApp.exe", "Playnite.FullscreenApp.exe"];

fn session(pid: u32) -> Option<u32> {
    let mut session = 0;
    // SAFETY: `session` is a local u32 that outlives the call.
    unsafe { ProcessIdToSessionId(pid, &mut session).ok()? };
    Some(session)
}
fn in_session(
    processes: Vec<butterpollo_core::steam::Process>,
    current: u32,
    mut session: impl FnMut(u32) -> Option<u32>,
) -> Vec<butterpollo_core::steam::Process> {
    processes
        .into_iter()
        .filter(|p| session(p.pid) == Some(current))
        .collect()
}
/// Processes in the streaming user's session, including games handed off
/// to a store client rather than started as Playnite children.
pub fn session_processes() -> Result<Vec<butterpollo_core::steam::Process>> {
    // SAFETY: GetCurrentProcessId takes no arguments and cannot fail.
    let current =
        session(unsafe { GetCurrentProcessId() }).context("reading the host's Windows session")?;
    Ok(in_session(crate::process::processes()?, current, session))
}
/// The running Playnite process (id and program), if any.
pub fn running() -> Option<(u32, PathBuf)> {
    // SAFETY: GetCurrentProcessId takes no arguments and cannot fail.
    let current = session(unsafe { GetCurrentProcessId() })?;
    find_running(
        crate::process::processes().ok()?,
        current,
        session,
        crate::process::image_path,
    )
}
fn find_running(
    processes: Vec<butterpollo_core::steam::Process>,
    current: u32,
    session: impl FnMut(u32) -> Option<u32>,
    mut image: impl FnMut(u32) -> Option<String>,
) -> Option<(u32, PathBuf)> {
    in_session(processes, current, session)
        .into_iter()
        .filter(|p| {
            PROCESSES
                .iter()
                .any(|name| p.name.eq_ignore_ascii_case(name))
        })
        .find_map(|p| Some((p.pid, PathBuf::from(image(p.pid)?))))
}
/// The running Playnite's folder, then the user's association, installer
/// record and default installation folder.
pub fn install_dir() -> Option<PathBuf> {
    if let Some((_, exe)) = running() {
        return exe.parent().map(PathBuf::from);
    }
    let mut candidates = Vec::new();
    if let Some(sid) = crate::process::user_sid() {
        if let Some(command) = registry_string(
            HKEY_USERS,
            &format!("{sid}\\Software\\Classes\\playnite\\shell\\open\\command"),
            "",
        ) {
            candidates.extend(
                Path::new(crate::process::command_target(&command))
                    .parent()
                    .map(PathBuf::from),
            );
        }
        if let Some(folder) = registry_string(
            HKEY_USERS,
            &format!(
                "{sid}\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Playnite_is1"
            ),
            "InstallLocation",
        ) {
            candidates.push(PathBuf::from(folder));
        }
    }
    if let Some(command) = registry_string(
        HKEY_LOCAL_MACHINE,
        "Software\\Classes\\playnite\\shell\\open\\command",
        "",
    ) {
        candidates.extend(
            Path::new(crate::process::command_target(&command))
                .parent()
                .map(PathBuf::from),
        );
    }
    if let Ok(environment) = crate::process::user_environment()
        && let Some(local) = environment.get("LOCALAPPDATA")
    {
        candidates.push(PathBuf::from(local).join("Playnite"));
    }
    find_install(candidates)
}
fn find_install(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|dir| executable(dir).is_some())
}
pub fn executable(dir: &Path) -> Option<PathBuf> {
    PROCESSES
        .iter()
        .map(|name| dir.join(name))
        .find(|exe| exe.is_file())
}
/// Where Playnite loads extensions from: the user's roaming folder for an
/// installed Playnite, the program folder for a portable one.
pub fn extensions_dir() -> Option<PathBuf> {
    let dir = install_dir()?;
    let environment = crate::process::user_environment().ok();
    extensions_at(
        &dir,
        environment
            .as_ref()
            .and_then(|env| env.get("APPDATA"))
            .map(Path::new),
    )
}
fn extensions_at(dir: &Path, roaming: Option<&Path>) -> Option<PathBuf> {
    // Playnite itself uses the uninstaller's presence to choose its data folder.
    if dir.join("unins000.exe").is_file() {
        Some(roaming?.join("Playnite").join("Extensions"))
    } else {
        Some(dir.join("Extensions"))
    }
}
/// Start the resolved executable in the signed-in user's session, without
/// depending on a URI association or cmd.exe reporting a launch failure.
pub fn launch(
    program: &Path,
    args: &[&str],
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let mut command = crate::process::quote(program.as_os_str())?;
    for arg in args {
        command.push(' ');
        command.push_str(&crate::process::quote(std::ffi::OsStr::new(arg))?);
    }
    crate::process::Process::spawn_command(
        program,
        &command,
        program.parent(),
        crate::process::Target::User { elevated: false },
        environment,
        false,
        true,
    )
    .with_context(|| {
        format!(
            "starting Playnite executable {} in the signed-in user's session",
            program.display()
        )
    })?;
    Ok(())
}

fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}
fn check_session(file: &File, current: u32) -> Result<()> {
    let mut server_session = 0;
    // SAFETY: `file` keeps the pipe handle open, and `server_session` outlives the call.
    unsafe { GetNamedPipeServerSessionId(handle(file), &mut server_session)? };
    if server_session != current {
        bail!(
            "Playnite plugin pipe belongs to Windows session {server_session}, but the host is in session {current}"
        );
    }
    Ok(())
}
/// Bytes waiting in the pipe; an error once it is closed.
fn available(file: &File) -> Result<u32> {
    let mut count = 0;
    // SAFETY: `file` keeps the pipe handle open; only `count` is written, and it outlives the call.
    unsafe { PeekNamedPipe(handle(file), None, 0, None, Some(&mut count), None)? };
    Ok(count)
}
fn open(name: &str, wait: Duration) -> Result<File> {
    let deadline = Instant::now() + wait;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            // A connector in our session must not borrow the host's token.
            .security_qos_flags(SECURITY_IDENTIFICATION.0)
            .open(name)
        {
            Ok(file) => return Ok(file),
            // Busy: another client is mid-handshake.
            Err(error) if error.raw_os_error() == Some(231) && Instant::now() < deadline => {
                let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
                // SAFETY: `wide` is a NUL-terminated pipe name that outlives the call.
                unsafe {
                    let _ = WaitNamedPipeW(windows::core::PCWSTR(wide.as_ptr()), 500);
                }
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error).with_context(|| format!("opening {name}")),
        }
    }
}
/// A connection to the plugin: newline-separated JSON both ways.
pub struct Pipe {
    file: Arc<Mutex<File>>,
    pub lines: mpsc::Receiver<String>,
    stop: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}
impl Pipe {
    /// Connect and introduce this side with `hello`. The plugin answers on
    /// its well-known pipe with the name of a private one.
    pub fn connect(hello: &serde_json::Value) -> Result<Self> {
        Self::connect_to(PIPE, hello)
    }
    fn connect_to(control_name: &str, hello: &serde_json::Value) -> Result<Self> {
        // SAFETY: GetCurrentProcessId takes no arguments and cannot fail.
        let current = session(unsafe { GetCurrentProcessId() })
            .context("reading the host's Windows session")?;
        let mut control = open(control_name, Duration::from_secs(2))
            .context("the Playnite plugin is not running")?;
        check_session(&control, current).context("checking the Playnite control pipe session")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while available(&control).context("reading the Playnite control pipe handshake")? < 80 {
            if Instant::now() > deadline {
                bail!(
                    "the Playnite plugin did not send its 80-byte control pipe handshake within 2 seconds"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut message = [0u8; 80];
        control
            .read_exact(&mut message)
            .context("reading the Playnite private pipe name")?;
        let units: Vec<u16> = message
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|unit| *unit != 0)
            .collect();
        let name = String::from_utf16(&units)?;
        if name.is_empty() || name.contains(['\\', '/']) {
            bail!("the Playnite plugin sent an invalid pipe name");
        }
        control
            .write_all(&[2])
            .context("acknowledging the Playnite pipe handshake")?;
        let file = open(&format!(r"\\.\pipe\{name}"), Duration::from_secs(5))
            .context("connecting to the Playnite private pipe")?;
        check_session(&file, current).context("checking the Playnite private pipe session")?;
        drop(control);
        let file = Arc::new(Mutex::new(file));
        let (sender, lines) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let (file, stop) = (file.clone(), stop.clone());
            std::thread::Builder::new()
                .name("playnite-pipe".into())
                .spawn(move || {
                    let mut pending = Vec::new();
                    while !stop.load(Ordering::Acquire) {
                        let read = {
                            let mut file = file.lock().unwrap();
                            match available(&file) {
                                Ok(0) => Ok(0),
                                Ok(count) => {
                                    let mut chunk = vec![0; count.min(1 << 20) as usize];
                                    file.read(&mut chunk)
                                        .inspect(|&n| pending.extend_from_slice(&chunk[..n]))
                                        .map_err(anyhow::Error::from)
                                }
                                Err(error) => Err(error),
                            }
                        };
                        match read {
                            Ok(0) => std::thread::sleep(Duration::from_millis(30)),
                            Ok(_) => {
                                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                                    let line: Vec<u8> = pending.drain(..=end).collect();
                                    let line = String::from_utf8_lossy(&line)
                                        .trim_end_matches(['\r', '\n'])
                                        .to_owned();
                                    if !line.is_empty() && sender.send(line).is_err() {
                                        return;
                                    }
                                }
                            }
                            // Closed: the receiver sees the channel end.
                            Err(error) => {
                                tracing::warn!(error = %format!("{error:#}"), "Playnite data pipe closed or failed");
                                return;
                            }
                        }
                    }
                })?
        };
        let pipe = Self {
            file,
            lines,
            stop,
            reader: Some(reader),
        };
        pipe.send(hello)
            .context("sending the Playnite plugin hello")?;
        Ok(pipe)
    }
    pub fn send(&self, message: &serde_json::Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        let mut file = self.file.lock().unwrap();
        file.write_all(&line)
            .context("writing to the Playnite data pipe")?;
        file.flush().context("flushing the Playnite data pipe")?;
        Ok(())
    }
}
impl Drop for Pipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::FromRawHandle;
    use windows::{
        Win32::{
            Storage::FileSystem::PIPE_ACCESS_DUPLEX,
            System::Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_NOWAIT,
                PIPE_TYPE_BYTE,
            },
        },
        core::PCWSTR,
    };

    fn server(name: &str) -> File {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `name` is NUL-terminated and outlives the call; null attributes mean default
        // security.
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_NOWAIT,
                1,
                4096,
                4096,
                0,
                None,
            )
        };
        assert!(!pipe.is_invalid());
        // SAFETY: `pipe` was checked valid and nothing else owns it, so the File takes sole
        // ownership.
        let file = unsafe { File::from_raw_handle(pipe.0) };
        // SAFETY: `file` owns the pipe handle, and a null OVERLAPPED is allowed for this
        // synchronous call on a PIPE_NOWAIT pipe.
        unsafe {
            let _ = ConnectNamedPipe(handle(&file), None);
        }
        file
    }
    fn accept(file: &File) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut pid = 0;
            // SAFETY: `file` keeps the pipe open and `pid` outlives the call.
            if unsafe { GetNamedPipeClientProcessId(handle(file), &mut pid) }.is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "fake pipe client did not connect"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn read_line(file: &mut File) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut line = Vec::new();
        loop {
            if available(file).unwrap() > 0 {
                let mut byte = [0];
                file.read_exact(&mut byte).unwrap();
                if byte[0] == b'\n' {
                    return String::from_utf8(line).unwrap();
                }
                line.push(byte[0]);
            } else {
                assert!(Instant::now() < deadline, "fake pipe received no line");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn pipe_handshake_and_fragmented_unicode_status_match_the_packaged_plugin() {
        let temp = tempfile::tempdir().unwrap();
        let name = format!(
            "BP-{}-{}",
            std::process::id(),
            temp.path().file_name().unwrap().to_string_lossy()
        );
        let control_name = format!(r"\\.\pipe\{name}-control");
        let mut control = server(&control_name);
        let mut data = server(&format!(r"\\.\pipe\{name}"));
        let thread = std::thread::spawn(move || {
            accept(&control);
            let mut handshake = [0u8; 80];
            for (i, unit) in name.encode_utf16().enumerate() {
                handshake[i * 2..i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
            }
            control.write_all(&handshake).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while available(&control).unwrap() == 0 {
                assert!(Instant::now() < deadline, "fake pipe received no ACK");
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut ack = [0];
            control.read_exact(&mut ack).unwrap();
            assert_eq!(ack, [2]);
            accept(&data);
            let hello: serde_json::Value = serde_json::from_str(&read_line(&mut data)).unwrap();
            assert_eq!(hello["role"], "launcher");
            let line = "\u{feff}{\"type\":\"status\",\"status\":{\"name\":\"gameStarted\",\"id\":\"game\",\"installDir\":\"C:/Çağrı/Oyun\"}}\r\n";
            data.write_all(&line.as_bytes()[..2]).unwrap();
            std::thread::sleep(Duration::from_millis(60));
            data.write_all(&line.as_bytes()[2..]).unwrap();
            assert_eq!(read_line(&mut data), "{\"done\":true}");
        });
        let pipe = Pipe::connect_to(
            &control_name,
            &serde_json::json!({"type":"hello","role":"launcher","pid":std::process::id()}),
        )
        .unwrap();
        let message = pipe.lines.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(
            matches!(butterpollo_core::playnite::parse(&message), butterpollo_core::playnite::Message::Status { install_dir, .. } if install_dir == "C:/Çağrı/Oyun")
        );
        pipe.send(&serde_json::json!({"done":true})).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn pipe_server_cannot_impersonate_the_host() {
        use std::os::windows::io::OwnedHandle;
        use windows::Win32::{
            Security::{
                GetTokenInformation, RevertToSelf, SECURITY_IMPERSONATION_LEVEL,
                SecurityIdentification, TOKEN_QUERY, TokenImpersonationLevel,
            },
            System::{
                Pipes::ImpersonateNamedPipeClient,
                Threading::{GetCurrentThread, OpenThreadToken},
            },
        };

        let temp = tempfile::tempdir().unwrap();
        let name = format!(
            r"\\.\pipe\BP-{}-{}",
            std::process::id(),
            temp.path().file_name().unwrap().to_string_lossy()
        );
        let mut server = server(&name);
        let mut client = open(&name, Duration::from_secs(1)).unwrap();
        check_session(&client, session(std::process::id()).unwrap()).unwrap();
        client.write_all(&[1]).unwrap();
        server.read_exact(&mut [0]).unwrap();
        let mut level = SECURITY_IMPERSONATION_LEVEL::default();
        // SAFETY: `server` is a connected pipe that has read client data, as impersonation
        // requires; `token` is owned once opened and `level` outlives GetTokenInformation.
        unsafe {
            ImpersonateNamedPipeClient(handle(&server)).unwrap();
            let mut token = HANDLE::default();
            let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token);
            RevertToSelf().unwrap();
            opened.unwrap();
            let token = OwnedHandle::from_raw_handle(token.0);
            GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenImpersonationLevel,
                Some((&mut level as *mut SECURITY_IMPERSONATION_LEVEL).cast()),
                size_of::<SECURITY_IMPERSONATION_LEVEL>() as u32,
                &mut 0,
            )
            .unwrap();
        }
        assert_eq!(level, SecurityIdentification);
    }

    #[test]
    fn pipe_rejects_a_server_outside_the_expected_windows_session() {
        let temp = tempfile::tempdir().unwrap();
        let name = format!(
            r"\\.\pipe\BP-{}-{}",
            std::process::id(),
            temp.path().file_name().unwrap().to_string_lossy()
        );
        let _server = server(&name);
        let client = open(&name, Duration::from_secs(1)).unwrap();
        let current = session(std::process::id()).unwrap();
        check_session(&client, current).unwrap();
        let error = check_session(&client, current + 1).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Playnite plugin pipe belongs to Windows session")
        );
    }

    #[test]
    fn running_ignores_other_sessions_and_unreadable_processes() {
        let processes = (1..=3)
            .map(|pid| butterpollo_core::steam::Process {
                pid,
                parent: 0,
                started: 1,
                name: PROCESSES[1].into(),
            })
            .collect();
        let found = find_running(
            processes,
            2,
            |pid| Some(if pid == 1 { 1 } else { 2 }),
            |pid| (pid != 2).then(|| "C:/Çağrı/Playnite.FullscreenApp.exe".into()),
        );
        assert_eq!(
            found,
            Some((3, PathBuf::from("C:/Çağrı/Playnite.FullscreenApp.exe")))
        );
    }

    #[test]
    fn discovery_skips_stale_paths_and_finds_either_mode_with_unicode() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("Çağrı & Oyunlar");
        std::fs::create_dir(&dir).unwrap();
        let fullscreen = dir.join(PROCESSES[1]);
        std::fs::write(&fullscreen, []).unwrap();
        assert_eq!(
            find_install([temp.path().join("missing"), dir.clone()]),
            Some(dir.clone())
        );
        assert_eq!(executable(&dir), Some(fullscreen));
        std::fs::write(dir.join(PROCESSES[0]), []).unwrap();
        assert_eq!(executable(&dir), Some(dir.join(PROCESSES[0])));
    }

    #[test]
    fn extensions_follow_playnites_portable_detection() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let roaming = dir.join("Çağrı");
        assert_eq!(
            extensions_at(dir, Some(&roaming)),
            Some(dir.join("Extensions"))
        );
        std::fs::write(dir.join("unins000.exe"), []).unwrap();
        assert_eq!(
            extensions_at(dir, Some(&roaming)),
            Some(roaming.join("Playnite/Extensions"))
        );
        assert_eq!(extensions_at(dir, None), None);
    }

    #[test]
    fn missing_executable_returns_the_actual_start_failure() {
        let temp = tempfile::tempdir().unwrap();
        let exe = temp.path().join("Çağrı Playnite.exe");
        let error = launch(&exe, &[], &Default::default()).unwrap_err();
        assert!(format!("{error:#}").contains("Çağrı Playnite.exe"));
    }
}
