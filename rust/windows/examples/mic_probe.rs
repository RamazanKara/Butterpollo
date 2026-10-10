//! The host half of a client microphone, measured on the real devices: tone
//! bursts are Opus-encoded and sealed as a client sends them, opened and
//! decoded as the host does, played into Steam Streaming Microphone, and
//! recorded back from "Microphone (Steam Streaming Microphone)" as a game or
//! chat app would. Reports the delay from a packet reaching the host to its
//! sound reaching the recording app, and whether the tone came through whole.
//!
//! usage: mic_probe SECONDS [LOSS_PERCENT] [--install]
//! Needs libopus-0.dll next to the probe or in BUTTERPOLLO_TEST_OPUS_ROOT.
#![allow(clippy::undocumented_unsafe_blocks)]
use anyhow::{Context, Result, bail};
use butterpollo_core::mic::{self, Arrival, Playout, Sequencer};
use butterpollo_windows::{
    audio::OpusDecoder,
    audio_route,
    capture::{ComGuard, Priority},
    mic::Render,
    timing::Timer,
};
use std::{
    ffi::c_void,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::PROPERTYKEY,
        Media::Audio::*,
        System::{
            Com::{StructuredStorage::*, *},
            Performance::*,
        },
    },
    core::GUID,
};

const FRIENDLY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};
const PACKET: usize = 960;
/// A burst of tone every this many packets, lasting `BURST` packets.
const EVERY: usize = 25;
const BURST: usize = 5;
const LEVEL: f32 = 0.3;

fn qpc_100ns() -> i64 {
    let (mut counter, mut frequency) = (0, 0);
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (counter as i128 * 10_000_000 / frequency as i128) as i64
}

type Create = unsafe extern "C" fn(i32, i32, i32, *mut i32) -> *mut c_void;
type Encode = unsafe extern "C" fn(*mut c_void, *const f32, i32, *mut u8, i32) -> i32;
type Ctl = unsafe extern "C" fn(*mut c_void, i32, ...) -> i32;
struct Encoder {
    state: *mut c_void,
    encode: Encode,
    _library: libloading::Library,
}
impl Encoder {
    fn new(directory: &std::path::Path) -> Result<Self> {
        unsafe {
            let library = libloading::Library::new(directory.join("libopus-0.dll"))?;
            let create = *library.get::<Create>(b"opus_encoder_create\0")?;
            let encode = *library.get::<Encode>(b"opus_encode_float\0")?;
            let ctl = *library.get::<Ctl>(b"opus_encoder_ctl\0")?;
            let mut error = 0;
            // VOIP, with in-band FEC for 10% loss, as a voice client would.
            let state = create(48000, 1, 2048, &mut error);
            if error != 0 || state.is_null() {
                bail!("Opus encoder: {error}");
            }
            ctl(state, 4012, 1i32);
            ctl(state, 4014, 10i32);
            Ok(Self {
                state,
                encode,
                _library: library,
            })
        }
    }
    fn encode(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        let mut out = vec![0; 1275];
        let n = unsafe {
            (self.encode)(
                self.state,
                samples.as_ptr(),
                samples.len() as i32,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        if n < 0 {
            bail!("Opus encoding error {n}");
        }
        out.truncate(n as usize);
        Ok(out)
    }
}

fn capture_endpoint() -> Result<IMMDevice> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let list = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        for index in 0..list.GetCount()? {
            let device = list.Item(index)?;
            let store = device.OpenPropertyStore(STGM_READ)?;
            let mut value = store.GetValue(&FRIENDLY)?;
            let name = PropVariantToStringAlloc(&value)
                .ok()
                .and_then(|text| {
                    let name = text.to_string().ok();
                    CoTaskMemFree(Some(text.0.cast()));
                    name
                })
                .unwrap_or_default();
            let _ = PropVariantClear(&mut value);
            if name
                .to_ascii_lowercase()
                .contains("steam streaming microphone")
            {
                return Ok(device);
            }
        }
    }
    bail!("Microphone (Steam Streaming Microphone) is not present")
}

/// Records the Steam microphone; each 1 ms slice's peak with its QPC time.
fn record(stop: &AtomicBool) -> Result<Vec<(i64, f32)>> {
    let _com = ComGuard::new()?;
    let device = capture_endpoint()?;
    let _priority = Priority::new();
    let timer = Timer::new()?;
    let mut packets = Vec::new();
    unsafe {
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let f = *format;
        let float = f.wFormatTag == 3
            || (f.wFormatTag == 65534
                && f.cbSize >= 22
                && (*(format as *const WAVEFORMATEXTENSIBLE)).SubFormat.data1 == 3);
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 200_000, 0, format, None);
        CoTaskMemFree(Some(format as *const c_void));
        init?;
        let bits = f.wBitsPerSample;
        if !(float && bits == 32) && bits != 16 {
            bail!("unsupported capture format: {bits} bits");
        }
        let rate = i64::from(f.nSamplesPerSec);
        let channels = usize::from(f.nChannels);
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        while !stop.load(Ordering::Acquire) {
            timer.until(Instant::now() + Duration::from_millis(1));
            loop {
                let (mut data, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0, 0, 0);
                if capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
                    .is_err()
                    || frames == 0
                {
                    break;
                }
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                // One peak per 1 ms slice, timed from the packet's first frame.
                let slice = (rate / 1000) as usize;
                for start in (0..frames as usize).step_by(slice) {
                    let end = (start + slice).min(frames as usize);
                    let peak = if silent {
                        0.
                    } else {
                        (start * channels..end * channels)
                            .map(|i| {
                                if float {
                                    (*(data as *const f32).add(i)).abs()
                                } else {
                                    f32::from(*(data as *const i16).add(i)).abs() / 32768.
                                }
                            })
                            .fold(0., f32::max)
                    };
                    packets.push((qpc as i64 + start as i64 * 10_000_000 / rate, peak));
                }
                capture.ReleaseBuffer(frames)?;
            }
        }
        let _ = client.Stop();
    }
    Ok(packets)
}

fn main() -> Result<()> {
    let _com = ComGuard::new()?;
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args
        .next()
        .context("usage: mic_probe SECONDS [LOSS_PERCENT] [--install]")?
        .parse()?;
    let mut loss = 0u32;
    let mut install = false;
    for arg in args {
        if arg == "--install" {
            install = true;
        } else {
            loss = arg.parse()?;
        }
    }
    let directory = std::env::var_os("BUTTERPOLLO_TEST_OPUS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_exe().unwrap().parent().unwrap().into());
    let find = || -> Result<Option<String>> {
        Ok(audio_route::steam_microphone(&audio_route::endpoints()?).map(|e| e.id.clone()))
    };
    let mut id = find()?;
    if id.is_none() && install {
        let config = butterpollo_core::config::Config::default();
        if let Some(saved) = audio_route::install_steam_microphone(&config)? {
            for _ in 0..40 {
                std::thread::sleep(Duration::from_millis(250));
                id = find()?;
                if id.is_some() {
                    std::thread::sleep(Duration::from_millis(500));
                    saved.restore()?;
                    break;
                }
            }
        }
    }
    let id = id.context("Steam Streaming Microphone is not installed (try --install)")?;
    let stop = Arc::new(AtomicBool::new(false));
    let recorder = {
        let stop = stop.clone();
        std::thread::spawn(move || record(&stop))
    };
    let playout = Arc::new(Mutex::new(Playout::default()));
    let renderer = {
        let playout = playout.clone();
        let stop = stop.clone();
        let id = id.clone();
        std::thread::spawn(move || -> Result<()> {
            let _com = ComGuard::new()?;
            let _priority = Priority::new();
            let mut render = Render::open(&id)?;
            while !stop.load(Ordering::Acquire) {
                render.wait(Duration::from_millis(50));
                render.fill(&mut playout.lock().unwrap())?;
            }
            Ok(())
        })
    };
    std::thread::sleep(Duration::from_millis(500));
    let key = [0x5a; 16];
    let key_id = 0xfff0_0000;
    let mut encoder = Encoder::new(&directory)?;
    let mut decoder = OpusDecoder::new(&directory)?;
    let mut sequencer = Sequencer::default();
    let timer = Timer::new()?;
    let packets = seconds as usize * 50;
    let mut onsets = Vec::new();
    let (mut lost, mut concealed, mut recovered) = (0, 0, 0);
    let mut random = 0x2545_f491_u32;
    let mut pcm = Vec::new();
    let start = Instant::now();
    for n in 0..packets {
        timer.until(start + Duration::from_millis(20 * n as u64));
        let tone = n % EVERY < BURST && n >= EVERY;
        let samples: Vec<f32> = (0..PACKET)
            .map(|i| {
                if tone {
                    ((n * PACKET + i) as f32 * std::f32::consts::TAU * 1000. / 48000.).sin() * LEVEL
                } else {
                    0.
                }
            })
            .collect();
        let datagram = mic::seal(
            &key,
            key_id,
            n as u16,
            n as u32 * 20,
            &encoder.encode(&samples)?,
        );
        random ^= random << 13;
        random ^= random >> 17;
        random ^= random << 5;
        // Never the first packet of a burst, so each onset is timed.
        if random % 100 < loss && !(tone && n % EVERY == 0) {
            lost += 1;
            continue;
        }
        // Received: from here on, the host's own path.
        let at = qpc_100ns();
        let packet = mic::parse(&datagram).context("probe datagram")?;
        let opus = mic::open(&key, key_id, &packet)?;
        pcm.clear();
        match sequencer.arrive(packet.sequence) {
            Arrival::Late => continue,
            Arrival::Restart => decoder.reset(),
            Arrival::AfterLoss { missing } => {
                for _ in 1..missing {
                    decoder.decode(None, false, &mut pcm)?;
                    concealed += 1;
                }
                decoder.decode(Some(&opus), true, &mut pcm)?;
                recovered += 1;
            }
            Arrival::Next => {}
        }
        decoder.decode(Some(&opus), false, &mut pcm)?;
        // The burst starts with this packet's own samples, after any concealed ones.
        let offset = (pcm.len() - PACKET) as i64 * 10_000_000 / 48000;
        playout.lock().unwrap().push(&pcm);
        if tone && n % EVERY == 0 {
            onsets.push(at + offset);
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    stop.store(true, Ordering::Release);
    renderer.join().unwrap()?;
    let recorded = recorder.join().unwrap()?;
    let trimmed = playout.lock().unwrap().trimmed;
    let heard: Vec<(i64, f64)> = onsets
        .iter()
        .filter_map(|&onset| {
            recorded
                .iter()
                .find(|&&(at, peak)| at >= onset && peak > LEVEL / 4.)
                .map(|&(at, _)| (onset, (at - onset) as f64 / 10_000.))
                .filter(|&(_, ms)| ms < 400.)
        })
        .collect();
    let mut delays: Vec<f64> = heard.iter().map(|&(_, ms)| ms).collect();
    delays.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        delays
            .get(((delays.len() as f64 - 1.) * p).round() as usize)
            .copied()
            .unwrap_or(f64::NAN)
    };
    // Loudness within bursts: a tone that drops out shows as quiet slices.
    let mut quiet = 0;
    let mut slices = 0;
    for &(onset, delay) in &heard {
        let from = onset + (delay * 10_000.) as i64 + 20 * 10_000;
        let to = onset + (delay * 10_000.) as i64 + (BURST as i64 * 20 - 20) * 10_000;
        for &(_, peak) in recorded.iter().filter(|(at, _)| (from..to).contains(at)) {
            slices += 1;
            if peak < LEVEL / 4. {
                quiet += 1;
            }
        }
    }
    let report = serde_json::json!({
        "seconds": seconds,
        "packets": packets,
        "lost": lost,
        "concealed": concealed,
        "recovered": recovered,
        "trimmed_ms": trimmed * 1000 / 48000,
        "bursts": onsets.len(),
        "heard": delays.len(),
        "received_to_recorded_ms": {
            "avg": delays.iter().sum::<f64>() / delays.len().max(1) as f64,
            "p50": percentile(0.5),
            "p95": percentile(0.95),
            "max": delays.last().copied().unwrap_or(f64::NAN),
        },
        "tone_slices": slices,
        "quiet_tone_slices": quiet,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    if delays.len() < onsets.len() * 9 / 10 {
        bail!(
            "only {} of {} bursts were heard",
            delays.len(),
            onsets.len()
        );
    }
    Ok(())
}
