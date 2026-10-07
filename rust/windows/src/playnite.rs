//! Playnite on this PC: where it is installed, where its extensions go, and
//! the named pipe its Butterpollo (Vibepollo) plugin serves.
use anyhow::{Context, Result, bail};
use std::{
    fs::File,
    io::{Read, Write},
    os::windows::io::AsRawHandle,
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
    System::{
        Pipes::{PeekNamedPipe, WaitNamedPipeW},
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
    unsafe { ProcessIdToSessionId(pid, &mut session).ok()? };
    Some(session)
}
/// The running Playnite process (id and program), if any.
pub fn running() -> Option<(u32, PathBuf)> {
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
    mut session: impl FnMut(u32) -> Option<u32>,
    mut image: impl FnMut(u32) -> Option<String>,
) -> Option<(u32, PathBuf)> {
    processes
        .into_iter()
        .filter(|p| {
            PROCESSES
                .iter()
                .any(|name| p.name.eq_ignore_ascii_case(name))
                && session(p.pid) == Some(current)
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
/// Bytes waiting in the pipe; an error once it is closed.
fn available(file: &File) -> Result<u32> {
    let mut count = 0;
    unsafe { PeekNamedPipe(handle(file), None, 0, None, Some(&mut count), None)? };
    Ok(count)
}
fn open(name: &str, wait: Duration) -> Result<File> {
    let deadline = Instant::now() + wait;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
        {
            Ok(file) => return Ok(file),
            // Busy: another client is mid-handshake.
            Err(error) if error.raw_os_error() == Some(231) && Instant::now() < deadline => {
                let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
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
        let mut control =
            open(PIPE, Duration::from_secs(2)).context("the Playnite plugin is not running")?;
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
