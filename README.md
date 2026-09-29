# Butterpollo

Butterpollo is a Windows game-streaming host built on [Vibepollo](https://github.com/Nonary/Vibepollo), which builds on [Apollo](https://github.com/ClassicOldSong/Apollo) and [Sunshine](https://github.com/LizardByte/Sunshine). It has one job: get each frame from your PC to your Moonlight client as fast and as evenly as possible. It adds no new features. A change goes in only if it makes streaming faster or smoother, and I measure it before it ships.

The name comes from the first tester's verdict on the WGC fix: "smooth as butter". Also, *pollo* is Spanish for chicken.

## Why this exists

I wrote the native AMD AMF encoder work for Vibepollo ([#461](https://github.com/Nonary/Vibepollo/pull/461)). That PR has been open since August. In the meantime the 2.0 betas brought regressions that made streams less smooth than 1.19: a DXGI display re-scan every second on the frame-pacing thread, and control-event polls that could spin. On top of that, the WGC capture path copied every frame three times before the encoder could start. I got tired of chasing new bugs in a moving target, so Butterpollo pins one base and only accepts latency and smoothness work.

## Measured

RX 7900 XT, AMD driver 32.0.31041.1004. Phone client, AV1 10-bit HDR, 1968x2184, 120 fps, WGC capture, native AMF. Numbers are host processing latency: capture to network send, the figure Moonlight shows. Each range covers several 10-second windows of the same stream.

| Build | Median | p99 | Max | Frames ≥1 ms above median, per 10 s |
|---|---:|---:|---:|---:|
| Before the WGC fix | 3.55-3.66 ms | 4.46-4.72 ms | 4.6-5.4 ms | 2-29 |
| Butterpollo | 3.21-3.28 ms | 3.37-3.74 ms | 3.7-4.6 ms | 0-2 |

Hardware encoding alone takes about 2.4 ms at this resolution, so most of what is left is the encoder itself.

## What is different from Vibepollo 2.0.0-beta.3

Encoder (native AMF, `encoder = amdvce_experimental`):
- The encode thread queries output directly on the low-latency path. Nothing sleeps on a fixed poll while a frame is due.
- Finished packets go out before the next capture wait and before the next colour conversion, so a frame never waits behind newer work.
- The input backlog is bounded, and a surface is reused only after the driver releases it.
- `amd_quality` defaults to `speed`.

Capture (Windows Graphics Capture):
- The encoder reads the capture helper's shared frame directly. The host used to copy each frame on a separate GPU device and hand it over, which cost about 0.5 ms median and 0.8 ms p99 in an off-screen model of the pipeline. `wgc_direct_encoder_input = disabled` switches back.
- The capture helper copies each frame once instead of twice when nothing is in the way.
- The once-per-second display check runs on a worker thread, not the frame-pacing thread.
- The capture device gets the same realtime GPU thread priority as the encoder device.

System:
- The host (while streaming) and the capture helper opt out of Windows 11 power throttling (EcoQoS and ignored timer resolution).
- Encoder control events (bitrate, reference invalidation, IDR) no longer spin when their producer holds the lock.

Diagnostics:
- Every 10 seconds the log prints `Host latency stages`: capture, convert, submit, encode and deliver, each as median/p99/max. It also shows which stage caused each spike and how old the frame already was when the host picked it up. If something stutters, that line shows where.

## Install

Download `VibepolloSetup.exe` from [Releases](https://github.com/RamazanKara/Butterpollo/releases).

The first releases install as a drop-in replacement for Vibepollo: same install folder, same service, same config and paired devices. Back up `C:\Program Files\Apollo\config` first and keep your current installer in case you want to go back. The installers are unsigned test builds.

To keep an existing Vibepollo virtual controller driver, install with:

```
VibepolloSetup.exe /qn INSTALL_VIRTUAL_GAMEPAD_DRIVER=0
```

Settings I stream with on AMD:

```
encoder = amdvce_experimental
amd_usage = ultralowlatency
amd_quality = speed
amd_preanalysis = disabled
amd_av1_latency_mode = lowest
```

## Next

- The virtual display runs at 4x the stream rate (480 Hz for 120 fps). That makes Windows compose frames sooner, but it also means more GPU and capture work. I'm measuring 480 against 240 and 120 Hz before changing the default.
- The capture helper still publishes about 1.5x more frames than the host uses.
- Linux, NVIDIA and Intel code is inherited from Vibepollo unchanged and is not tested here yet.

Bug reports are welcome if they include GPU and driver, client, resolution/fps/codec, and a few `Host latency stages` log lines.

## Credits and license

Butterpollo is GPL-3.0, like everything it builds on. Thanks to Nonary for Vibepollo, ClassicOldSong for Apollo, and LizardByte and the Sunshine contributors. Everything outside the list above is their work.
