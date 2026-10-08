//! A real Steam Deck controller for a Steam Deck client. The Deck's own USB
//! controller (28de:1205, [`butterpollo_core::steam_deck`]) is served over
//! USB/IP on loopback and attached by usbip-win2's signed driver, so Steam on
//! the host recognises a Steam Deck, with its trackpads, gyro and back grips,
//! and applies the Deck's Steam Input configuration to it.
use anyhow::{Context, Result, bail};
use butterpollo_core::{
    input::Input as Event,
    steam_deck::{REPORT_PERIOD, SteamDeck},
    usbip::{BUS_ID, Export},
};
use std::{
    ffi::OsStr,
    io::Read,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// usbip.exe returns once the device is plugged in or the attach failed.
const COMMAND_WAIT: Duration = Duration::from_secs(15);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// usbip-win2's command-line client: its installer's folder, then PATH.
pub(super) fn usbip_exe() -> Option<PathBuf> {
    let mut folders: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|folder| PathBuf::from(folder).join("USBip"))
        .collect();
    if let Some(path) = std::env::var_os("PATH") {
        folders.extend(std::env::split_paths(&path));
    }
    folders
        .into_iter()
        .map(|folder| folder.join("usbip.exe"))
        .find(|exe| exe.is_file())
}

/// Whether Steam is running on the host, which `auto` needs: without Steam
/// Input, games that read XInput would not see a Steam Deck controller.
pub(super) fn steam_running() -> bool {
    crate::process::processes().is_ok_and(|list| {
        list.iter()
            .any(|process| process.name.eq_ignore_ascii_case("steam.exe"))
    })
}

/// Runs usbip.exe and returns what it printed.
fn usbip(exe: &Path, args: &[&str]) -> Result<String> {
    let mut child = Command::new(exe)
        .args(args.iter().map(OsStr::new))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    let deadline = Instant::now() + COMMAND_WAIT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("usbip.exe {} did not finish", args.join(" "));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    if !status.success() {
        bail!(
            "usbip.exe {} failed ({status}): {}",
            args.join(" "),
            err.trim()
        );
    }
    Ok(out)
}

/// One attached Steam Deck controller. Dropping it detaches it.
pub(super) struct DeckPad {
    export: Option<Export<SteamDeck>>,
    exe: PathBuf,
    /// The USB/IP server's TCP port.
    tcp: String,
    /// usbip-win2's hub port the controller is plugged into.
    port: u32,
    last_rumble: Option<(u16, u16)>,
}
impl DeckPad {
    /// Serves a controller whose Steam settings follow `serial` and has
    /// usbip-win2 attach it.
    pub(super) fn plug(exe: &Path, serial: &str) -> Result<Self> {
        let export = Export::listen(SteamDeck::new(serial), REPORT_PERIOD)
            .context("starting the USB/IP server")?;
        let tcp = export.port().to_string();
        let out = usbip(
            exe,
            &[
                "--tcp-port",
                &tcp,
                "attach",
                "--remote",
                "127.0.0.1",
                "--bus-id",
                BUS_ID,
                "--terse",
                "--once",
            ],
        )?;
        let port = out
            .lines()
            .rev()
            .find_map(|line| line.trim().parse().ok())
            .with_context(|| format!("usbip.exe attach printed no port: {}", out.trim()))?;
        Ok(Self {
            export: Some(export),
            exe: exe.to_owned(),
            tcp,
            port,
            last_rumble: None,
        })
    }
    pub(super) fn port(&self) -> u32 {
        self.port
    }
    /// Applies a controller, motion or touch event.
    pub(super) fn apply(&self, event: &Event) {
        if let Some(export) = &self.export
            && export.with(|deck| deck.state.apply(event))
        {
            export.changed();
        }
    }
    /// Rumble Steam set since the last call, as the VHF driver's Xbox
    /// feedback report: low and high motor speed, then trigger motors.
    pub(super) fn feedback(&mut self) -> Option<(u16, Vec<u8>)> {
        let rumble = self.export.as_ref()?.with(SteamDeck::take_rumble)?;
        if self.last_rumble.replace(rumble) == Some(rumble) {
            return None;
        }
        let mut data = rumble.0.to_le_bytes().to_vec();
        data.extend_from_slice(&rumble.1.to_le_bytes());
        data.extend_from_slice(&[0; 4]);
        Some((4, data))
    }
}
impl Drop for DeckPad {
    fn drop(&mut self) {
        let port = self.port.to_string();
        if let Err(error) = usbip(&self.exe, &["detach", "--port", &port]) {
            tracing::warn!(
                error = format!("{error:#}"),
                "Steam Deck controller detach failed"
            );
        }
        // Closing the server would otherwise leave the driver trying to
        // attach the controller again.
        drop(self.export.take());
        let _ = usbip(
            &self.exe,
            &[
                "--tcp-port",
                &self.tcp,
                "attach",
                "--remote",
                "127.0.0.1",
                "--bus-id",
                BUS_ID,
                "--stop",
            ],
        );
    }
}
