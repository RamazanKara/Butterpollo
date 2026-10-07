//! Costs on the host's input path: wake, decrypt, SendInput, display
//! lookups, synthetic pointer devices, VHF ioctls and thread priority.
//! Sections: priority wake sendinput display pointer crypto injector batch
//! (default); vhf, pad-contention, contention (explicit).
//! --load=none,normal,highest runs wake, sendinput, vhf and pad-contention
//! beside a CPU-bound child process; --no-streaming keeps the default timer
//! resolution and priority class; --trace prints the input module's logs.
//! Mouse input is zero-distance moves only. vhf and pad-contention plug a
//! neutral virtual pad and remove it: run them while no one streams.
//! pad-contention runs VHF and ViGEm X360 separately.
use butterpollo_windows::capture::Priority;
use std::{
    net::UdpSocket,
    os::windows::io::AsRawSocket,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::HANDLE,
    Networking::WinSock::*,
    System::Threading::*,
    UI::{Controls::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

#[repr(C)]
#[derive(Default)]
struct ThreadBasicInformation {
    exit_status: i32,
    teb: usize,
    client_id: [usize; 2],
    affinity: usize,
    priority: i32,
    base_priority: i32,
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationThread(
        thread: HANDLE,
        class: u32,
        info: *mut core::ffi::c_void,
        len: u32,
        ret: *mut u32,
    ) -> i32;
}
fn thread_priority() -> (i32, i32, i32) {
    let mut info = ThreadBasicInformation::default();
    let mut ret = 0;
    unsafe {
        NtQueryInformationThread(
            GetCurrentThread(),
            0,
            (&mut info as *mut ThreadBasicInformation).cast(),
            size_of::<ThreadBasicInformation>() as u32,
            &mut ret,
        );
        (
            info.priority,
            info.base_priority,
            GetThreadPriority(GetCurrentThread()),
        )
    }
}

fn stats(name: &str, mut v: Vec<f64>) {
    if v.is_empty() {
        println!("{name:52} no samples");
        return;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let mean = v.iter().sum::<f64>() / n as f64;
    println!(
        "{name:52} n={n:5} mean {mean:8.1} p50 {:8.1} p90 {:8.1} p99 {:8.1} max {:9.1} us",
        v[n / 2],
        v[n * 90 / 100],
        v[n * 99 / 100],
        v[n - 1]
    );
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Prio {
    /// No priority change.
    Plain,
    /// butterpollo_windows::capture::Priority::new (the control loop's before
    /// October 7; capture, encode, audio and pacing threads still use it).
    PriorityNew,
    /// butterpollo_windows::capture::Priority::input (the control loop's now).
    PriorityInput,
    /// MMCSS "Games"/HIGH without the SetThreadPriority that follows it.
    MmcssOnly,
}
struct MmcssOnly(HANDLE);
impl MmcssOnly {
    fn new() -> Self {
        unsafe {
            let name: Vec<u16> = "Games\0".encode_utf16().collect();
            let mut index = 0;
            let handle =
                AvSetMmThreadCharacteristicsW(windows::core::PCWSTR(name.as_ptr()), &mut index)
                    .unwrap_or_default();
            let _ = AvSetMmThreadPriority(handle, AVRT_PRIORITY_HIGH);
            Self(handle)
        }
    }
}
impl Drop for MmcssOnly {
    fn drop(&mut self) {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.0);
        }
    }
}
#[allow(dead_code)]
enum PrioGuard {
    A(Priority),
    B(MmcssOnly),
    None,
}
fn apply_prio(p: Prio) -> PrioGuard {
    match p {
        Prio::Plain => PrioGuard::None,
        Prio::PriorityNew => PrioGuard::A(Priority::new()),
        Prio::PriorityInput => PrioGuard::A(Priority::input()),
        Prio::MmcssOnly => PrioGuard::B(MmcssOnly::new()),
    }
}

/// A child process (normal priority class) spinning on every logical CPU at
/// the given relative thread priority, standing in for a CPU-bound game.
struct Load(Option<std::process::Child>);
impl Load {
    fn start(kind: &str) -> Self {
        if kind == "none" {
            return Self(None);
        }
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(format!("--spin={kind}"))
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        Self(Some(child))
    }
}
impl Drop for Load {
    fn drop(&mut self) {
        if let Some(c) = &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
fn spin_child(kind: &str) {
    let priority = match kind {
        "highest" => THREAD_PRIORITY_HIGHEST,
        "critical" => THREAD_PRIORITY_TIME_CRITICAL,
        _ => THREAD_PRIORITY_NORMAL,
    };
    let n = std::thread::available_parallelism().map_or(16, |n| n.get());
    let deadline = Instant::now() + Duration::from_secs(60);
    let threads: Vec<_> = (0..n)
        .map(|_| {
            std::thread::spawn(move || {
                unsafe {
                    let _ = SetThreadPriority(GetCurrentThread(), priority);
                }
                let mut x = 0u64;
                while Instant::now() < deadline {
                    for _ in 0..10_000 {
                        x = std::hint::black_box(
                            x.wrapping_mul(6364136223846793005).wrapping_add(1),
                        );
                    }
                }
            })
        })
        .collect();
    for t in threads {
        let _ = t.join();
    }
}

fn priority_section() {
    println!("== thread priority (NtQueryInformationThread: current, base; GetThreadPriority) ==");
    for prio in [
        Prio::Plain,
        Prio::PriorityNew,
        Prio::PriorityInput,
        Prio::MmcssOnly,
    ] {
        let h = std::thread::spawn(move || {
            let _p = apply_prio(prio);
            std::thread::sleep(Duration::from_millis(20));
            let mut seen = std::collections::BTreeMap::new();
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(1000) {
                *seen.entry(thread_priority()).or_insert(0u32) += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            println!("{prio:?}: (current, base, GetThreadPriority) -> samples {seen:?}");
        });
        h.join().unwrap();
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Wait {
    Poll1,
    PollLong,
    Event,
    Blocking,
    Spin,
}

/// One-way delivery latency: sender stamps the datagram right before
/// send_to; the receiver measures when it has the bytes in hand.
fn wake_case(wait: Wait, prio: Prio, samples: usize) -> Vec<f64> {
    let base = Instant::now();
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = receiver.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(false));
    let (stop_r, ready_r) = (stop.clone(), ready.clone());
    let recv_thread = std::thread::spawn(move || {
        let _p = apply_prio(prio);
        let raw = receiver.as_raw_socket();
        let mut lat = Vec::with_capacity(samples);
        let mut buf = [0u8; 64];
        let event = unsafe { WSACreateEvent().unwrap() };
        match wait {
            Wait::Blocking => {
                receiver.set_nonblocking(false).unwrap();
                receiver
                    .set_read_timeout(Some(Duration::from_millis(50)))
                    .unwrap();
            }
            Wait::Event => unsafe {
                WSAEventSelect(SOCKET(raw as usize), Some(event), FD_READ as i32);
            },
            _ => receiver.set_nonblocking(true).unwrap(),
        }
        ready_r.store(true, Ordering::Release);
        while !stop_r.load(Ordering::Acquire) {
            // Drain everything there, as ENet's receive loop does.
            loop {
                match receiver.recv_from(&mut buf) {
                    Ok((n, _)) if n >= 8 => {
                        let now = base.elapsed().as_nanos() as u64;
                        let sent = u64::from_le_bytes(buf[..8].try_into().unwrap());
                        lat.push((now.saturating_sub(sent)) as f64 / 1000.);
                        if wait == Wait::Blocking {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            match wait {
                Wait::Poll1 => {
                    let _ = butterpollo_windows::net::wait_readable(raw, 1);
                }
                Wait::PollLong => {
                    let _ = butterpollo_windows::net::wait_readable(raw, 50);
                }
                Wait::Event => unsafe {
                    WaitForSingleObject(HANDLE(event.0 as *mut _), 50);
                    let mut ev = WSANETWORKEVENTS::default();
                    WSAEnumNetworkEvents(SOCKET(raw as usize), event, &mut ev);
                },
                Wait::Blocking | Wait::Spin => {}
            }
        }
        lat
    });
    while !ready.load(Ordering::Acquire) {
        std::hint::spin_loop();
    }
    // The sender stands in for the NIC: keep it out of the competition.
    let _sender_prio = MmcssOnly::new();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let timer = butterpollo_windows::timing::Timer::new().unwrap();
    let mut rng = Rng(0x9e3779b97f4a7c15);
    std::thread::sleep(Duration::from_millis(20));
    for _ in 0..samples {
        // Random phase relative to the receiver's 1 ms poll: 0.3-3.3 ms apart.
        let gap = Duration::from_micros(300 + rng.next() % 3000);
        timer.until(Instant::now() + gap);
        let stamp = base.elapsed().as_nanos() as u64;
        sender.send_to(&stamp.to_le_bytes(), address).unwrap();
    }
    std::thread::sleep(Duration::from_millis(60));
    stop.store(true, Ordering::Release);
    let _ = sender.send_to(&[0u8; 8], address);
    let mut lat = recv_thread.join().unwrap();
    lat.truncate(samples);
    lat
}

fn wake_section(samples: usize, loads: &[String]) {
    println!("== UDP loopback send -> bytes in receiver (wait method, receiver priority, load) ==");
    for load in loads {
        let _load = Load::start(load);
        for (wait, prio) in [
            (Wait::Poll1, Prio::PriorityNew),
            (Wait::Poll1, Prio::MmcssOnly),
            (Wait::Poll1, Prio::Plain),
            (Wait::PollLong, Prio::PriorityNew),
            (Wait::Event, Prio::PriorityNew),
            (Wait::Blocking, Prio::PriorityNew),
            (Wait::Spin, Prio::PriorityNew),
        ] {
            if load != "none" && matches!(wait, Wait::PollLong | Wait::Blocking) {
                continue;
            }
            let lat = wake_case(wait, prio, samples);
            stats(&format!("[{load}] {wait:?} {prio:?}"), lat);
        }
    }
    // Raw sendto cost (what ENet's ACK costs before the event is dispatched).
    let sink = UdpSocket::bind("127.0.0.1:0").unwrap();
    sink.set_nonblocking(true).unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    sender.set_nonblocking(true).unwrap();
    let address = sink.local_addr().unwrap();
    let mut v = Vec::new();
    let mut b = [0u8; 64];
    for i in 0..4000 {
        let t = Instant::now();
        let _ = sender.send_to(&[0u8; 20], address);
        if i >= 100 {
            v.push(t.elapsed().as_secs_f64() * 1e6);
        }
        if i % 64 == 0 {
            while sink.recv_from(&mut b).is_ok() {}
        }
    }
    stats("sendto 20 B loopback (ACK-sized)", v);
    let mut v = Vec::new();
    for _ in 0..4000 {
        let t = Instant::now();
        let _ = sink.recv_from(&mut b);
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("recvfrom on empty nonblocking socket", v);
    let mut v = Vec::new();
    for _ in 0..4000 {
        let t = Instant::now();
        let _ = butterpollo_windows::net::wait_readable(sink.as_raw_socket(), 0);
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("WSAPoll(timeout 0) on empty socket", v);
}

fn sendinput_section(loads: &[String]) {
    println!("== SendInput (zero relative mouse moves: no cursor motion) ==");
    let zero = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: MOUSEEVENTF_MOVE,
                ..Default::default()
            },
        },
    };
    for load in loads {
        let _load = Load::start(load);
        let load = load.clone();
        let worker = std::thread::spawn(move || {
            let _p = Priority::new();
            for batch in [1usize, 2, 4, 8] {
                let inputs = vec![zero; batch];
                let mut v = Vec::new();
                let mut failed = 0;
                for i in 0..(1500 / batch).max(200) + 50 {
                    let t = Instant::now();
                    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
                    let us = t.elapsed().as_secs_f64() * 1e6;
                    if sent as usize != batch {
                        failed += 1;
                    }
                    if i >= 50 {
                        v.push(us);
                    }
                    // Space calls like real input (~1 kHz mouse).
                    let until = Instant::now() + Duration::from_micros(500);
                    while Instant::now() < until {
                        std::hint::spin_loop();
                    }
                }
                stats(
                    &format!("[{load}] SendInput {batch}/call (failed {failed}) per call"),
                    v.clone(),
                );
                if batch > 1 {
                    let per: Vec<f64> = v.iter().map(|x| x / batch as f64).collect();
                    stats(&format!("[{load}]   ...per input at {batch}/call"), per);
                }
            }
        });
        worker.join().unwrap();
    }
    let mut v = Vec::new();
    for _ in 0..4000 {
        let t = Instant::now();
        unsafe {
            std::hint::black_box((
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            ));
        }
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("GetSystemMetrics x4 (absolute mouse)", v);
    let mut v = Vec::new();
    for i in 0..4000u32 {
        let t = Instant::now();
        unsafe {
            std::hint::black_box(MapVirtualKeyW(0x41 + (i % 26), MAPVK_VK_TO_VSC));
        }
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("MapVirtualKeyW (always_send_scancodes)", v);
}

fn display_section() {
    println!("== display lookups that run on the control thread ==");
    let displays = butterpollo_windows::capture::displays().unwrap_or_default();
    let name = displays
        .first()
        .map(|d| d.display_name.clone())
        .unwrap_or_default();
    println!("first display {name:?}, {} attached", displays.len());
    let mut v = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        let _ = std::hint::black_box(butterpollo_windows::display::mode(&name));
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("display::mode (current_rect, every 500 ms)", v);
    let mut v = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        let _ = std::hint::black_box(butterpollo_windows::capture::displays());
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("capture::displays (DXGI factory+enum)", v);
    let mut v = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        let _ = std::hint::black_box(butterpollo_windows::display::monitors());
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("display::monitors (QDC + displays())", v);
    let mut v = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        let _ = std::hint::black_box(butterpollo_windows::display::monitors());
        let _ = std::hint::black_box(butterpollo_windows::capture::displays());
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("display_rect equivalent (monitors+displays)", v);
}

fn pointer_section() {
    println!("== synthetic pointer devices (created and destroyed, nothing injected) ==");
    for (label, kind, count) in [("touch x32", PT_TOUCH, 32u32), ("pen x1", PT_PEN, 1)] {
        let mut create = Vec::new();
        let mut destroy = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let device =
                unsafe { CreateSyntheticPointerDevice(kind, count, POINTER_FEEDBACK_NONE) };
            create.push(t.elapsed().as_secs_f64() * 1e6);
            if let Ok(device) = device {
                let t = Instant::now();
                unsafe { DestroySyntheticPointerDevice(device) };
                destroy.push(t.elapsed().as_secs_f64() * 1e6);
            } else {
                println!("{label}: create failed {device:?}");
            }
        }
        stats(&format!("CreateSyntheticPointerDevice {label}"), create);
        stats(&format!("DestroySyntheticPointerDevice {label}"), destroy);
    }
    // What applications see while a synthetic touch device exists: making
    // one ahead of a client's first touch would change it for every stream.
    let touch = || unsafe {
        (
            GetSystemMetrics(SM_MAXIMUMTOUCHES),
            GetSystemMetrics(SM_DIGITIZER),
        )
    };
    let before = touch();
    let device = unsafe { CreateSyntheticPointerDevice(PT_TOUCH, 32, POINTER_FEEDBACK_NONE) };
    std::thread::sleep(Duration::from_millis(200));
    let during = touch();
    if let Ok(device) = device {
        unsafe { DestroySyntheticPointerDevice(device) };
    }
    std::thread::sleep(Duration::from_millis(200));
    println!(
        "SM_MAXIMUMTOUCHES, SM_DIGITIZER: before {before:?}, with a synthetic touch device {during:?}, after {:?}",
        touch()
    );
}

fn vhf_section(loads: &[String]) {
    use butterpollo_core::input::Input;
    println!("== VHF virtual gamepad (neutral pad, removed at the end) ==");
    let worker = {
        let loads = loads.to_vec();
        std::thread::spawn(move || {
            let _p = Priority::new();
            let t = Instant::now();
            let opened = butterpollo_windows::input::Gamepads::open(
                butterpollo_core::input_policy::VHF_AUTO,
            );
            println!(
                "Gamepads::open (SetupDi + CreateFile + version ioctl): {:.1} us -> {}",
                t.elapsed().as_secs_f64() * 1e6,
                match &opened {
                    Ok(_) => "ok".to_owned(),
                    Err(e) => format!("{e:#}"),
                }
            );
            let Ok(mut pads) = opened else { return };
            let neutral = |buttons: u32| Input::Controller {
                id: 0,
                active: 1,
                buttons,
                left_trigger: 0,
                right_trigger: 0,
                sticks: [0; 4],
            };
            let t = Instant::now();
            let plugged = pads.apply(&neutral(0));
            println!(
                "first state (plug ioctl 0x801 + submit 0x803): {:.1} us -> {:?}",
                t.elapsed().as_secs_f64() * 1e6,
                plugged.as_ref().err()
            );
            if plugged.is_ok() {
                // Let PnP, HID class and any readers (Steam, GameInput) settle.
                std::thread::sleep(Duration::from_secs(3));
                for load in &loads {
                    let _load = Load::start(load);
                    for round in 0..2 {
                        let mut v = Vec::new();
                        let started = Instant::now();
                        for _ in 0..1000 {
                            if started.elapsed() > Duration::from_secs(8) {
                                break;
                            }
                            let t = Instant::now();
                            let _ = pads.apply(&neutral(0));
                            v.push(t.elapsed().as_secs_f64() * 1e6);
                            // ~1 kHz controller updates.
                            let until = t + Duration::from_millis(1);
                            while Instant::now() < until {
                                std::hint::spin_loop();
                            }
                        }
                        stats(
                            &format!("[{load}] round {round}: apply state (ioctl 0x803)"),
                            v,
                        );
                    }
                    let mut v = Vec::new();
                    for _ in 0..500 {
                        let t = Instant::now();
                        let _ = std::hint::black_box(pads.feedback());
                        v.push(t.elapsed().as_secs_f64() * 1e6);
                    }
                    stats(&format!("[{load}] feedback 1 pad (ioctl 0x804)"), v);
                }
            }
            let t = Instant::now();
            drop(pads);
            println!(
                "drop (unplug ioctl 0x802 + CloseHandle): {:.1} us",
                t.elapsed().as_secs_f64() * 1e6
            );
        })
    };
    worker.join().unwrap();
}

/// The control loop's input pass with a virtual pad in use: one controller
/// state and one zero-distance mouse move per millisecond, plus the 8 ms
/// refresh and feedback poll. Reports how long the mouse move waited from the
/// start of its pass. A neutral pad is plugged and removed at the end.
fn pad_contention_section(loads: &[String], profile: &'static str) {
    use butterpollo_core::input::Input;
    println!(
        "== {profile}: mouse move behind a controller state (Injector, neutral pad, zero moves) =="
    );
    let loads = loads.to_vec();
    std::thread::spawn(move || {
        let _p = Priority::input();
        let mut injector = butterpollo_windows::input::Injector::new("", profile).unwrap();
        let state = Input::Controller {
            id: 0,
            active: 1,
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            sticks: [0; 4],
        };
        let mouse = Input::Relative { x: 0, y: 0 };
        // Plug the pad and let PnP and its readers settle.
        let t = Instant::now();
        let _ = injector.apply(&state);
        println!(
            "first state through the injector: {:.1} us",
            t.elapsed().as_secs_f64() * 1e6
        );
        std::thread::sleep(Duration::from_secs(3));
        for load in &loads {
            let _load = Load::start(load);
            let (mut pads, mut mice, mut waits, mut passes) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let started = Instant::now();
            let mut poll_at = Instant::now();
            while started.elapsed() < Duration::from_secs(6) {
                let t = Instant::now();
                let _ = injector.apply(&state);
                let m = Instant::now();
                let _ = injector.apply(&mouse);
                pads.push(m.duration_since(t).as_secs_f64() * 1e6);
                mice.push(m.elapsed().as_secs_f64() * 1e6);
                waits.push(t.elapsed().as_secs_f64() * 1e6);
                if Instant::now() >= poll_at {
                    poll_at = Instant::now() + Duration::from_millis(8);
                    let _ = injector.refresh();
                }
                // The gamepad thread polls feedback; the loop collects it.
                let _ = std::hint::black_box(injector.gamepad_reports().count());
                passes.push(t.elapsed().as_secs_f64() * 1e6);
                let until = t + Duration::from_millis(1);
                while Instant::now() < until {
                    std::hint::spin_loop();
                }
            }
            stats(&format!("[{load}] controller state apply"), pads);
            stats(&format!("[{load}] mouse move apply (SendInput)"), mice);
            stats(
                &format!("[{load}] state + mouse move: mouse done after"),
                waits,
            );
            stats(&format!("[{load}] whole pass incl. 8 ms poll"), passes);
        }
        let t = Instant::now();
        drop(injector);
        println!(
            "injector drop (pad unplugged): {:.1} us",
            t.elapsed().as_secs_f64() * 1e6
        );
    })
    .join()
    .unwrap();
}

/// One-off costs on the input thread: creating an injector, and following a
/// display that does not exist yet (looked up again every 500 ms).
fn injector_section() {
    println!("== injector creation and missing-display lookups on the calling thread ==");
    let first = butterpollo_windows::capture::displays()
        .ok()
        .and_then(|d| d.first().map(|d| d.display_name.clone()))
        .unwrap_or_default();
    let config = butterpollo_core::config::Config::default();
    for (label, output) in [
        ("existing display", first.as_str()),
        ("missing display", r"\\.\DISPLAY99"),
    ] {
        let mut v = Vec::new();
        for _ in 0..20 {
            let t = Instant::now();
            let injector =
                butterpollo_windows::input::Injector::new_options(output, "vhf", &config);
            v.push(t.elapsed().as_secs_f64() * 1e6);
            drop(injector);
        }
        stats(&format!("Injector::new_options ({label})"), v);
    }
    let mut injector =
        butterpollo_windows::input::Injector::new_options(r"\\.\DISPLAY99", "vhf", &config)
            .unwrap();
    let mut v = Vec::new();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(3) {
        let t = Instant::now();
        injector.set_output(r"\\.\DISPLAY99");
        let _ = injector.refresh();
        v.push(t.elapsed().as_secs_f64() * 1e6);
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut slow: Vec<f64> = v.iter().copied().filter(|us| *us > 200.).collect();
    slow.sort_by(f64::total_cmp);
    stats("set_output + refresh per pass, display missing", v);
    println!("  passes over 200 us: {slow:.0?}");
    let mut v = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        std::thread::spawn(|| {}).join().unwrap();
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    stats("thread spawn + join (empty)", v);
    let mut v = Vec::new();
    let mut handles = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        handles.push(std::thread::spawn(|| {}));
        v.push(t.elapsed().as_secs_f64() * 1e6);
    }
    for h in handles {
        let _ = h.join();
    }
    stats("thread spawn alone (caller's cost)", v);
}

/// Several events in one pass: zero-distance mouse moves, so nothing moves.
/// Each event applied on its own (one SendInput call each, the control loop
/// before October 7) against the whole pass at once (apply_all).
fn batch_section() {
    use butterpollo_core::input::Input;
    println!("== events applied in one pass (zero mouse moves) ==");
    std::thread::spawn(|| {
        let _p = Priority::input();
        let mut injector = butterpollo_windows::input::Injector::new("", "vhf").unwrap();
        for count in [1usize, 2, 4, 8] {
            let events = vec![Input::Relative { x: 0, y: 0 }; count];
            let (mut each, mut all) = (Vec::new(), Vec::new());
            for i in 0..1300 {
                let t = Instant::now();
                if i % 2 == 0 {
                    for event in &events {
                        let _ = injector.apply(event);
                    }
                } else {
                    let _ = injector.apply_all(&events);
                }
                if i >= 100 {
                    let us = t.elapsed().as_secs_f64() * 1e6;
                    if i % 2 == 0 {
                        each.push(us)
                    } else {
                        all.push(us)
                    }
                }
                let until = Instant::now() + Duration::from_micros(500);
                while Instant::now() < until {
                    std::hint::spin_loop();
                }
            }
            stats(&format!("{count} events per pass, apply each"), each);
            stats(&format!("{count} events per pass, apply_all"), all);
        }
    })
    .join()
    .unwrap();
}
fn crypto_section() {
    println!("== control packet decrypt + decode (per input datagram) ==");
    let key = [7u8; 16];
    // A client mouse-move input (magic 7) inside an encrypted 0x0206 message.
    let mut input = 8u32.to_be_bytes().to_vec();
    input.extend_from_slice(&7u32.to_le_bytes());
    input.extend_from_slice(&5i16.to_be_bytes());
    input.extend_from_slice(&(-3i16).to_be_bytes());
    let mut plain = 0x0206u16.to_le_bytes().to_vec();
    plain.extend_from_slice(&(input.len() as u16).to_le_bytes());
    plain.extend_from_slice(&input);
    for v2 in [true, false] {
        let mut v = Vec::new();
        for seq in 0..20000u32 {
            let mut iv = vec![0u8; if v2 { 12 } else { 16 }];
            if v2 {
                iv[..4].copy_from_slice(&seq.to_le_bytes());
                iv[10] = b'C';
                iv[11] = b'C';
            } else {
                iv[0] = seq as u8;
            }
            let (tag, ct) = butterpollo_core::crypto::gcm_seal(&key, &iv, &plain).unwrap();
            let mut p = 1u16.to_le_bytes().to_vec();
            p.extend_from_slice(&((4 + 16 + ct.len()) as u16).to_le_bytes());
            p.extend_from_slice(&seq.to_le_bytes());
            p.extend_from_slice(&tag);
            p.extend_from_slice(&ct);
            let t = Instant::now();
            let (_, kind, payload) =
                butterpollo_core::packet::decrypt_control(&key, &p, v2).unwrap();
            let event = butterpollo_core::input::decode(&payload).unwrap();
            std::hint::black_box((kind, event));
            v.push(t.elapsed().as_secs_f64() * 1e6);
        }
        stats(&format!("decrypt_control(v2={v2}) + input::decode"), v);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(kind) = args.iter().find_map(|a| a.strip_prefix("--spin=")) {
        spin_child(kind);
        return;
    }
    if args.iter().any(|a| a == "--trace") {
        // Shows input failures logged on other threads, such as the gamepad
        // thread's driver calls.
        tracing_subscriber::fmt()
            .with_env_filter("butterpollo_windows::input=debug")
            .init();
    }
    let sections: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let all = sections.is_empty();
    let has = |s: &str| all || sections.iter().any(|a| *a == s);
    let streaming = !args.iter().any(|a| a == "--no-streaming");
    let loads: Vec<String> = args
        .iter()
        .find_map(|a| a.strip_prefix("--load="))
        .unwrap_or("none")
        .split(',')
        .map(str::to_owned)
        .collect();
    if streaming {
        // What StreamingScope does for the process, without its system-wide
        // side effects (DWM MMCSS, Wi-Fi mode, Mouse Keys).
        butterpollo_windows::timing::disable_power_throttling();
        unsafe {
            let _ = windows::Win32::Media::timeBeginPeriod(1);
            let _ = SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
        }
        println!("process: 1 ms timer resolution, HIGH priority class, no power throttling");
    } else {
        println!("process: default timer resolution and priority class");
    }
    let samples = args
        .iter()
        .find_map(|a| a.strip_prefix("--samples=").and_then(|n| n.parse().ok()))
        .unwrap_or(1500);
    if has("priority") {
        priority_section();
    }
    if has("crypto") {
        crypto_section();
    }
    if has("sendinput") {
        sendinput_section(&loads);
    }
    if has("display") {
        display_section();
    }
    if has("pointer") {
        pointer_section();
    }
    if sections.iter().any(|a| *a == "vhf") {
        vhf_section(&loads);
    }
    if sections.iter().any(|a| *a == "pad-contention") {
        for profile in ["vhf", "x360"] {
            pad_contention_section(&loads, profile);
        }
    }
    if has("injector") {
        injector_section();
    }
    if has("batch") {
        batch_section();
    }
    if has("wake") {
        wake_section(samples, &loads);
    }
    if sections.iter().any(|a| *a == "contention") {
        // Spinners at TIME_CRITICAL (15) on every CPU: only the receiver's
        // priority differs between the two cases.
        println!("== receiver priority under TIME_CRITICAL spinners on all CPUs ==");
        let _load = Load::start("critical");
        for prio in [Prio::PriorityNew, Prio::PriorityInput, Prio::MmcssOnly] {
            let lat = wake_case(Wait::Poll1, prio, samples);
            stats(&format!("[critical] Poll1 {prio:?}"), lat);
        }
    }
    if streaming {
        unsafe {
            let _ = windows::Win32::Media::timeEndPeriod(1);
        }
    }
}
