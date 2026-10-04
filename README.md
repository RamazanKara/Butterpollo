# Butterpollo

**A Moonlight game-streaming host for Windows, written in Rust and built around AMD Radeon.** It keeps your stream quick while a game is pushing the GPU as hard as it can.

[![Butterpollo in 35 seconds: why frames arrive late, the measured difference, and a live stream in the console](docs/media/demo.gif)](docs/media/demo.mp4)

<sub>35-second demo. Click it for the full 1080p video.</sub>

**[Download Butterpollo 2.0.0-rc.1](https://github.com/RamazanKara/Butterpollo/releases/tag/2.0.0-rc.1)**: run `butterpollo-setup-2.0.0-rc.1.exe`. It installs fresh or upgrades Vibepollo 2.0 in place, and your settings, paired devices and game library come along.

## The trick: stay off the game's queue

Sunshine and the hosts built on it (Apollo, Vibepollo and their forks) copy each captured frame and convert its colours with Direct3D 11. All of that lands on the GPU's graphics queue, the same queue your game is filling with its own frames. So the host's small job waits until the game's current frame is done. Idle, nobody notices. In a demanding game, I measured the colour conversion alone taking 7.2 ms instead of 0.9 ms, and capture copies 8.4 ms instead of 0.75 ms.

Butterpollo moves that work to Direct3D 12 compute queues, which the GPU runs next to the game instead of behind it. Under the same load the conversion finishes in about 0.25 ms. AMD's encoder then reads the converted frame straight from Direct3D 12, so nothing goes back to the graphics queue. Getting this right meant two pieces of careful synchronisation: one with Windows' compositor, so a frame is never copied before Windows has finished writing it, and one fence per output texture for the AMD encoder. Both were verified frame by frame on real streams ([details](rust/PERFORMANCE.md#october-4-capture-copies-and-conversion-beside-a-game)).

## What it measures

RX 7900 XT, AV1 10-bit HDR at 1968×2184 and 120 fps, next to a game-like load that keeps the GPU busy:

| | Graphics queue | Butterpollo |
|---|---:|---:|
| Frame to finished bitstream, mean | 14.3 ms | **2.8 ms** |
| Same, 95th percentile | 32.6 ms | **2.9 ms** |
| Frames encoded per second (target 120) | 90 | **120** |
| Full stream, render to decoded picture | 52.9 ms | **45.5 ms** |
| Full stream with an idle GPU | 13.0 ms | **12.4 ms** |

"Graphics queue" is the same build with the compute path switched off (`gpu_compute_conversion = false`), which handles frames the way Sunshine-based hosts do. The first three rows encode moving test frames. The full-stream rows come from encrypted streams to an independent client that reads a moving barcode, so they include Windows' compositor and decoding. The encode itself sits at the hardware's floor of about 3 ms, so the idle gain is small; the big win shows up the moment a game uses the GPU.

## Tuned for AMD, end to end

- Native AMF through AMD's C interface, with ultra-low-latency encoding and the `speed` preset by default. No FFmpeg in the AMD path.
- Desktop Duplication capture by default, with the virtual display at twice the stream rate (240 Hz for a 120 fps stream). Both measured lower latency than Vibepollo's defaults; Vibepollo's values are one setting away.
- Video error correction uses 21-29% less CPU time than the C++ host, with byte-identical output.
- AV1, HEVC and H.264, HDR10, 10-bit SDR, and PyroWave for very fast wired networks. NVIDIA (NVENC) and Intel (Quick Sync) encoders are included but still need testing on real hardware, and the compute-queue path is AMD-only for now.

## Everything from Vibepollo 2.0, rebuilt in Rust

The host, its Windows service, the installer and the web console's server are written in Rust; none of the old C++ host is linked. It speaks the same protocol, reads the same settings and keeps the same files, so Moonlight, Artemis and Nonary's Moonlight builds connect as before.

- Per-device virtual displays with exclusive, extended and isolated layouts, HDR, and the layout restored after every stream.
- Steam library sync with covers, and streams that end when the Steam game exits.
- Playnite through Vibepollo's plugin, including the fullscreen app. Lossless Scaling profiles and frame generation per app.
- PIN and one-time-PIN pairing, per-device permissions, display modes and overrides.
- RTSS and NVIDIA frame limiting, NVIDIA Smooth Motion and RTX HDR.
- A new web console with live stream stats: frame rate, encode p95, host time and frame age.

[PARITY.md](rust/PARITY.md) lists every feature against Vibepollo 2.0 with its evidence. Left out on purpose: WebRTC streaming, the session history pages, the ViGEm and SudoVDA fallbacks, and Linux and macOS.

## Install

1. Download `butterpollo-setup-2.0.0-rc.1.exe` from [Releases](https://github.com/RamazanKara/Butterpollo/releases).
2. Run it. It installs the host, service, virtual display and gamepad drivers, or upgrades Vibepollo 2.0 or an earlier Butterpollo in place.
3. Open the console at `https://localhost:47990`, pair your device and stream.

Prefer a portable copy? Extract `butterpollo-rust-2.0.0-rc.1-windows-x64.zip` and open **Start Butterpollo.exe**; it offers to import your Vibepollo or Apollo profile and leaves the original alone. The builds are unsigned, so keep your previous installer and a copy of your config if you might want to go back.

## Where it stands

This is a release candidate, and I stream with it on an RX 7900 XT. Still on the list: NVIDIA and Intel need hardware testing, AMD's AV1 encoder pads some odd sizes (1968×2184 decodes as 1984×2186, [AMF issue 423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423); HEVC is exact), and a few Vibepollo extras are still missing (listed in [PARITY.md](rust/PARITY.md)). Bug reports are very welcome; attach the support bundle from the console's Logs page.

More: [release notes](rust/RELEASE_NOTES.md) · [building and running](rust/README.md) · [performance evidence](rust/PERFORMANCE.md) · [the C++ fork's history](docs/butterpollo-cpp.md)

The name comes from the first tester's verdict on an early capture fix: "smooth as butter". *Pollo* is Spanish for chicken.

## Credits and license

Butterpollo is GPL-3.0, like everything it builds on. Thanks to Nonary for Vibepollo, ClassicOldSong for Apollo, and LizardByte and the Sunshine contributors, whose work defines the protocol and features Butterpollo carries forward. PyroWave and Granite are by Themaister (MIT), and the PyroWave Moonlight protocol and clients are joemossjr16's work.
