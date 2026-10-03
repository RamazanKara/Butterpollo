//! Playnite on this PC: where it is installed, where its extensions go, and
//! the named pipe its Butterpollo (Vibepollo) plugin serves.
use anyhow::{Context, Result, bail};
use std::{
    fs::File,
    io::{Read, Write},
    os::windows::io::AsRawHandle,
    path::PathBuf,
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
    },
};

use crate::steam::registry_string;

const PIPE: &str = r"\\.\pipe\Sunshine.PlayniteExtension";
pub const PROCESSES: [&str; 2] = ["Playnite.DesktopApp.exe", "Playnite.FullscreenApp.exe"];

/// The running Playnite process (id and program), if any.
pub fn running() -> Option<(u32, PathBuf)> {
    crate::process::processes()
        .ok()?
        .into_iter()
        .find(|p| {
            PROCESSES
                .iter()
                .any(|name| p.name.eq_ignore_ascii_case(name))
        })
        .and_then(|p| Some((p.pid, PathBuf::from(crate::process::image_path(p.pid)?))))
}
/// Playnite's program folder: the running Playnite, else the program the
/// signed-in user's `playnite:` links open.
pub fn install_dir() -> Option<PathBuf> {
    if let Some((_, exe)) = running() {
        return exe.parent().map(PathBuf::from);
    }
    let command = crate::process::user_sid()
        .and_then(|sid| {
            registry_string(
                HKEY_USERS,
                &format!("{sid}\\Software\\Classes\\playnite\\shell\\open\\command"),
                "",
            )
        })
        .or_else(|| {
            registry_string(
                HKEY_LOCAL_MACHINE,
                "Software\\Classes\\playnite\\shell\\open\\command",
                "",
            )
        })?;
    let exe = PathBuf::from(crate::process::command_target(&command));
    exe.is_file()
        .then(|| exe.parent().map(PathBuf::from))
        .flatten()
}
/// Where Playnite loads extensions from: the user's roaming folder for an
/// installed Playnite, the program folder for a portable one.
pub fn extensions_dir() -> Option<PathBuf> {
    let installed = crate::process::user_sid().is_some_and(|sid| {
        registry_string(
            HKEY_USERS,
            &format!(
                "{sid}\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Playnite_is1"
            ),
            "InstallLocation",
        )
        .is_some()
    });
    if installed {
        let environment = crate::process::user_environment().ok()?;
        return Some(
            PathBuf::from(environment.get("APPDATA")?)
                .join("Playnite")
                .join("Extensions"),
        );
    }
    install_dir().map(|dir| dir.join("Extensions"))
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
        while available(&control)? < 80 {
            if Instant::now() > deadline {
                bail!("the Playnite plugin did not answer");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut message = [0u8; 80];
        control.read_exact(&mut message)?;
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
        control.write_all(&[2])?;
        let file = open(&format!(r"\\.\pipe\{name}"), Duration::from_secs(5))?;
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
                                        .map_err(|_| ())
                                }
                                Err(_) => Err(()),
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
                            Err(()) => return,
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
        pipe.send(hello)?;
        Ok(pipe)
    }
    pub fn send(&self, message: &serde_json::Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        let mut file = self.file.lock().unwrap();
        file.write_all(&line)?;
        file.flush()?;
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
