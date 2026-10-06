# Butterpollo

**A Moonlight game-streaming host for Windows, written in Rust and built around AMD Radeon.** Its compute path reduces streaming delays beside demanding GPU work; the measured gains and remaining limits are below.

[![Butterpollo in 35 seconds: why frames arrive late, the measured difference against Vibepollo 2.0, and a live stream in the console](docs/media/demo.gif)](docs/media/demo.mp4)

<sub>35-second demo. Click it for the full 1080p video.</sub>

**[Download Butterpollo 2.0.0-rc.10](https://github.com/RamazanKara/Butterpollo/releases/tag/2.0.0-rc.10)**: run `butterpollo-setup-2.0.0-rc.10.exe`. It installs fresh or upgrades Vibepollo 2.0 in place, and your settings, paired devices and game library come along.

rc.10 distinguishes HDR from wide-gamut SDR and protects previously inactive displays during virtual-display startup. It retains WGC and guarded pacing as defaults, the Moonlight PC 6.2.0 H.264 fix, and the earlier RTSS and firewall repairs. Updates notify first, with automatic installation available as an opt-in. See the [release notes and validation limits](rust/RELEASE_NOTES.md#new-in-rc10).

## The trick: stay off the game's queue

Sunshine and the hosts built on it (Apollo, Vibepollo and their forks) copy each captured frame and convert its colours with Direct3D 11. All of that lands on the GPU's graphics queue, the same queue your game is filling with its own frames. So the host's small job waits until the game's current frame is done. Idle, nobody notices. In a demanding game, I measured the colour conversion alone taking 7.2 ms instead of 0.9 ms, and capture copies 8.4 ms instead of 0.75 ms.

Butterpollo moves that work to Direct3D 12 compute queues, which the GPU runs next to the game instead of behind it. Under the same load the conversion finishes in about 0.25 ms. AMD's encoder then reads the converted frame straight from Direct3D 12, so nothing goes back to the graphics queue. Getting this right meant two pieces of careful synchronisation: one with Windows' compositor, so a frame is never copied before Windows has finished writing it, and one fence per output texture for the AMD encoder. Both were verified frame by frame on real streams ([details](rust/PERFORMANCE.md#october-4-capture-copies-and-conversion-beside-a-game)).

## What it measures

These are historical controlled comparisons from October 4, retained with their original settings. They are not a rerun of every rc.10 path or a guarantee of 60 fresh pictures under GPU saturation.

RX 7900 XT, HEVC 10-bit HDR at 1080p and 60 fps, the setting most people stream at. Vibepollo 2.0 and Butterpollo ran one after the other on the same PC with the same encoder settings, three times each, idle and next to a game-like load (177 fps beside Vibepollo, 174 fps beside Butterpollo, which sends twice as many frames):

| 1080p60, mean of 3 runs | Vibepollo 2.0 | Butterpollo |
|---|---:|---:|
| Idle: render to decoded picture | 16.0 ms | **13.8 ms** |
| Idle: host latency shown in Moonlight | 2.75 ms | **1.96 ms** |
| Next to the game: render to decoded picture | 96.4 ms | **42.4 ms** |
| Same, 95th percentile | 137.0 ms | **56.5 ms** |
| Next to the game: new pictures per second (of 60) | 23.9 | **51.4** |
| Next to the game: host latency shown in Moonlight | 60.6 ms | **9.0 ms** |

Colours match too: the decoded HDR pictures from both hosts land on the expected black and white levels, and Butterpollo's are within half a 10-bit step of the reference on average ([runs and colour check](rust/PERFORMANCE.md#against-vibepollo-20)).

Where the gain comes from, measured on Butterpollo itself with the compute path switched off and on:

| 1080p60 | Graphics queue | Butterpollo |
|---|---:|---:|
| Frame to finished bitstream, mean | 21.5 ms | **2.0 ms** |
| Same, 95th percentile | 37.5 ms | **2.3 ms** |
| Frames encoded per second (target 60) | 58.9 | **60** |
| Full stream, render to decoded picture, mean | 41.0 ms | **33.5 ms** |
| Same, 95th percentile | 54.4 ms | **42.3 ms** |

And at a phone's 1968×2184 with AV1 10-bit HDR and 120 fps:

| 1968×2184 at 120 fps | Graphics queue | Butterpollo |
|---|---:|---:|
| Frame to finished bitstream, mean | 14.3 ms | **2.8 ms** |
| Same, 95th percentile | 32.6 ms | **2.9 ms** |
| Frames encoded per second (target 120) | 90 | **120** |
| Full stream, render to decoded picture, mean | 52.9 ms | **45.5 ms** |

"Graphics queue" is the same build with the compute path switched off (`gpu_compute_conversion = false`), which handles frames the way Sunshine-based hosts do. The bitstream rows encode moving test frames. The render-to-picture rows, here and in the Vibepollo table, come from encrypted streams to an independent client that reads a moving barcode, so they include Windows' compositor and decoding. With an idle GPU the two paths are level at 1080p60 (14.0 ms) and Butterpollo is 0.6 ms ahead at 1968×2184 and 120 fps (12.4 against 13.0 ms); the big win shows up the moment a game uses the GPU. Each table is one batch of alternating runs. How fast the GPU gets through the load drifts from batch to batch (Butterpollo's 1080p60 picture took 33.5 ms in one and 42.4 ms in the other), so compare numbers within a table, not across tables.

## Tuned for AMD, end to end

- Native AMF through AMD's C interface, with ultra-low-latency encoding and the `speed` preset by default. No FFmpeg in the AMD path.
- Automatic capture prefers WGC with Desktop Duplication fallback; explicit capture choices are retained. Guarded pacing improves measured 60 FPS freshness when capture reports surplus updates. The virtual display defaults to twice the stream rate (240 Hz for a 120 fps stream). Controlled rc.9 motion and helper-recovery checks passed; [the evidence](rust/PERFORMANCE.md) also records saturation failures and untested hardware.
- Video error correction uses 21-29% less CPU time than the C++ host, with byte-identical output.
- AV1, HEVC and H.264, HDR10, 10-bit SDR, and PyroWave for very fast wired networks. NVIDIA (NVENC) and Intel (Quick Sync) encoders are included but still need testing on real hardware, and the compute-queue path is AMD-only for now.

## Vibepollo 2.0 features, rebuilt in Rust

The host, its Windows service, the installer and the web console's server are written in Rust; none of the old C++ host is linked. It speaks the same protocol, reads the same settings and keeps the same files, so Moonlight, Artemis and Nonary's Moonlight builds connect as before.

- Per-device virtual displays with exclusive, extended and isolated layouts, HDR, and owned layout restoration. A disconnected app can retain its virtual display until the app is closed.
- Steam library sync with covers, and streams that end when the Steam game exits.
- Playnite through Vibepollo's plugin, including the fullscreen app. Lossless Scaling profiles and frame generation per app.
- PIN and one-time-PIN pairing, per-device permissions, display modes and overrides.
- RTSS and NVIDIA frame limiting, NVIDIA Smooth Motion and RTX HDR.
- A new web console with live stream stats: frame rate, encode p95, host time and frame age.

[PARITY.md](rust/PARITY.md) lists every feature against Vibepollo 2.0 with its evidence. Left out on purpose: WebRTC streaming, the session history pages, the ViGEm and SudoVDA fallbacks, and Linux and macOS.

## Install

1. Download `butterpollo-setup-2.0.0-rc.10.exe` from [Releases](https://github.com/RamazanKara/Butterpollo/releases).
2. Run it. It installs the host, service, virtual display and gamepad drivers, or upgrades Vibepollo 2.0 or an earlier Butterpollo in place.
3. Open the console at `https://localhost:47990`, pair your device and stream.

Prefer a portable copy? Extract `butterpollo-rust-2.0.0-rc.10-windows-x64.zip` and open **Start Butterpollo.exe**; it offers to import your Vibepollo or Apollo profile and leaves the original alone. The builds are unsigned, so keep your previous installer and a copy of your config if you might want to go back.

## Where it stands

This is a release candidate, tested on an RX 7900 XT. The exact released Moonlight PC 6.2.0 client passed the scoped codec and reconnect checks; its HDR requests used an SDR source. NVIDIA and Intel still need hardware testing, and uncapped GPU saturation can still drop fresh pictures. AMD's AV1 encoder pads some sizes (1968×2184 decodes as 1984×2186, [AMF issue 423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423)); Moonlight 6.2.0's existing crop handling passed three scoped checks, while HEVC encodes the exact dimensions. The remote RX 9070 XT/Wi-Fi report and remaining Vibepollo extras are tracked in [PARITY.md](rust/PARITY.md). Bug reports are welcome; attach the support bundle from the console's Logs page.

More: [release notes](rust/RELEASE_NOTES.md) · [building and running](rust/README.md) · [performance evidence](rust/PERFORMANCE.md) · [the C++ fork's history](docs/butterpollo-cpp.md)

The name comes from the first tester's verdict on an early capture fix: "smooth as butter". *Pollo* is Spanish for chicken.

## Credits and license

Butterpollo is GPL-3.0, like everything it builds on. Thanks to Nonary for Vibepollo, ClassicOldSong for Apollo, and LizardByte and the Sunshine contributors, whose work defines the protocol and features Butterpollo carries forward. PyroWave and Granite are by Themaister (MIT), and the PyroWave Moonlight protocol and clients are joemossjr16's work.
