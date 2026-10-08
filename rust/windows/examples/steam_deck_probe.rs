//! Attaches a virtual Steam Deck controller through usbip-win2 without a
//! Moonlight client, as the host does for a Steam Deck, and drives it so
//! Windows, SDL and Steam can be checked by hand. Run elevated:
//!
//! `cargo run -p butterpollo-windows --example steam_deck_probe -- [seconds]`
//!
//! For the given time (default 60 s) it presses A every second, sweeps the
//! left stick, turns the gyro and strokes the right trackpad, prints rumble
//! Steam sends, then detaches the controller and stops any reattach.
use butterpollo_core::{
    input::Input,
    steam_deck::{REPORT_PERIOD, SteamDeck},
    usbip::{BUS_ID, Export},
};
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

fn usbip_exe() -> Option<PathBuf> {
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

fn usbip(exe: &PathBuf, args: &[&str]) -> String {
    let out = Command::new(exe)
        .args(args)
        .output()
        .expect("run usbip.exe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("usbip {} -> {} {}", args.join(" "), out.status, text.trim());
    text
}

fn main() {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let exe = usbip_exe().expect("usbip-win2's usbip.exe not found");
    let export = Export::listen(SteamDeck::new("BUTTERPOLLOPROBE"), REPORT_PERIOD).unwrap();
    let tcp = export.port().to_string();
    let attach = [
        "--tcp-port",
        tcp.as_str(),
        "attach",
        "--remote",
        "127.0.0.1",
        "--bus-id",
        BUS_ID,
        "--terse",
        "--once",
    ];
    let out = usbip(&exe, &attach);
    let port: u32 = out
        .lines()
        .rev()
        .find_map(|line| line.trim().parse().ok())
        .expect("attach printed no port");
    eprintln!(
        "attached on usbip-win2 port {port}, server port {tcp}, imported {}",
        export.imported()
    );

    let start = Instant::now();
    let mut tick = 0u32;
    while start.elapsed() < Duration::from_secs(seconds) {
        let t = start.elapsed().as_secs_f32();
        let a = if (t as u32).is_multiple_of(2) {
            0x1000
        } else {
            0
        };
        let events = [
            Input::Controller {
                id: 0,
                active: 1,
                buttons: a,
                left_trigger: 0,
                right_trigger: 0,
                sticks: [((t * 2.).sin() * 20000.) as i16, 0, 0, 0],
            },
            Input::Motion {
                id: 0,
                kind: 2,
                xyz: [0., (t * 3.).sin() * 90., 0.],
            },
            Input::ControllerTouch {
                id: 0,
                event: 3,
                touchpad: 1,
                pointer: 1,
                x: (t.sin() + 1.) / 2.,
                y: 0.5,
                pressure: 0.5,
            },
        ];
        for event in &events {
            if export.with(|deck| deck.state.apply(event)) {
                export.changed();
            }
        }
        if let Some((low, high)) = export.with(SteamDeck::take_rumble) {
            eprintln!("rumble from the host: low {low} high {high}");
        }
        tick += 1;
        if tick.is_multiple_of(200) {
            eprintln!("{:.0} s, imported {}", t, export.imported());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    usbip(&exe, &["detach", "--port", &port.to_string()]);
    drop(export);
    let mut stop = attach[..7].to_vec();
    stop.push("--stop");
    usbip(&exe, &stop);
    usbip(&exe, &["port"]);
}
