# Butterpollo 1.x: the C++ fork (history)

[Current documentation](README.md) · [Archived C++ references](https://github.com/RamazanKara/Butterpollo/blob/2.0.0-rc.23/docs/legacy/README.md) · [Current architecture](architecture.md)

These are the notes from Butterpollo's C++ releases (`2.0.0-beta.3-butter.1` to `butter.4`), a fork of Vibepollo 2.0.0-beta.3. Butterpollo 2.0 replaced that host with the Rust host in [`rust/`](../rust/README.md); see the [main README](../README.md) for the current release. The C++ sources were removed from `main` after 2.0.0-rc.23; [that tag](https://github.com/RamazanKara/Butterpollo/tree/2.0.0-rc.23) keeps them. Some things below no longer apply: Butterpollo 2.0 brings back the Steam, Playnite and Lossless Scaling integrations, and installs with `butterpollo-setup-<version>.exe` instead of `VibepolloSetup.exe`.

Butterpollo is a Windows game-streaming host built on [Vibepollo](https://github.com/Nonary/Vibepollo), which builds on [Apollo](https://github.com/ClassicOldSong/Apollo) and [Sunshine](https://github.com/LizardByte/Sunshine). It has one job: get each frame from your PC to your Moonlight client as fast and as evenly as possible. Anything that doesn't serve that job has been taken out. A change goes in only if it makes streaming faster or smoother, and I measure it before it ships.

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

Encoder (native AMF, `encoder = amdvce`):
- The encode thread queries output directly on the low-latency path. Nothing sleeps on a fixed poll while a frame is due.
- Finished packets go out before the next capture wait and before the next colour conversion, so a frame never waits behind newer work.
- The input backlog is bounded, and a surface is reused only after the driver releases it.
- `amd_quality` defaults to `speed`.

Capture (Windows Graphics Capture):
- The virtual display runs at 2x the stream rate (240 Hz for 120 fps) instead of 4x (480 Hz). In a blind A/B/C test on the phone stream, 2x had slightly lower host latency (3.17-3.19 vs 3.25-3.26 ms median). It also had much steadier frame age: how old a frame already is when the host picks it up was 1.78 ms median and about 2 ms p99 at 240 Hz, against 2.0 ms median and 4.2-5.7 ms p99 at 480 Hz. The helper also handles half as many capture callbacks. `frame_limiter_auto_virtual_framegen = enabled` brings back 4x.
- The encoder reads the capture helper's shared frame directly. The host used to copy each frame on a separate GPU device and hand it over, which cost about 0.5 ms median and 0.8 ms p99 in an off-screen model of the pipeline. `wgc_direct_encoder_input = disabled` switches back.
- The capture helper copies each frame once instead of twice when nothing is in the way.
- The capture helper publishes only the frames the host will use. With a 240 Hz virtual display and a 120 fps stream, about half the frames used to be copied and then replaced before the host looked at them. The host now tells the helper when it takes its next frame. If a newer frame is due before then, the helper holds the current one without copying it and publishes it just before the host's slot only if nothing newer arrived. A game running at or below the stream rate is published immediately, as before. While this is active the helper's activity rate limit is off, so it can no longer drop the one frame the host needed. In a simulation of the pipeline it halves the helper's copies at 240 Hz, and the host gets the newest frame at least as often as before. It has not been measured on a live stream yet. `wgc_slot_aligned_publish = disabled` switches back.
- The once-per-second display check runs on a worker thread, not the frame-pacing thread.
- The capture device gets the same realtime GPU thread priority as the encoder device.

System:
- Packetization writes the frame header and payload directly into the final packet buffer. Each client keeps its own pacing clock.
- Input batching combines safe mouse/scroll deltas and stops before signed 16-bit overflow; draining the queue no longer copies every candidate packet.
- Rumble and HDR feedback are checked within a 10 ms idle control-server interval. Deferred display checks skip a busy operation gate.
- Mouse-unplug recovery is checked once per second while streaming.
- Audio capture converts the device's 100 ns period to milliseconds correctly, with a nonzero event wait.
- WGC frame-arrival workers register their own MMCSS task; the helper runs at above-normal process priority.
- Default logging is info for every release channel. Explicit debug/verbose configuration is preserved.
- The host (while streaming) and the capture helper opt out of Windows 11 power throttling (EcoQoS and ignored timer resolution).
- Encoder control events (bitrate, reference invalidation, IDR) no longer spin when their producer holds the lock.

Diagnostics:
- Every 10 seconds the log prints `Host latency stages`: capture, convert, submit, encode and deliver, each as median/p99/max. It also shows which stage caused each spike and how old the frame already was when the host picked it up. If something stutters, that line shows where.
- With WGC capture the log also prints `WGC helper publish to host claim` every 10 seconds: how long a published frame waited before the host took it.

Removed, because none of it is needed to stream:
- Linux, macOS and FreeBSD support. Butterpollo builds for Windows only.
- WebRTC browser streaming. Moonlight is unaffected.
- Playnite, Steam library and Lutris integration, and Lossless Scaling automation. Apps you added yourself keep working, including `steam://` commands. Apps synced from Playnite without a command are skipped at startup.
- The stats and session history pages and the SQLite database behind them. The Devices page still shows who is connected, and the Overview page has a Stop stream button.
- The legacy display helper, SudoVDA fallback, and ViGEm gamepad backend. The current display helper, virtual-display driver and VHF gamepad backend remain.
- FFmpeg AMF and Media Foundation encoding. AMD always uses native AMF; saved experimental/legacy AMD encoder names migrate to it. NVIDIA NVENC, Intel QuickSync, software encoding, and PyroWave remain available.
- The classic interface. The consolidated interface is served at `/`, with redirects from old `/v2` links.
- Docker files, upstream issue bots, docs-site tooling and dead code.

## PyroWave (experimental)

[PyroWave](https://github.com/Themaister/pyrowave) is an intra-only wavelet codec that runs as Vulkan compute. Every frame stands alone, and encoding is very fast: in a self-test on an RX 7900 XT, colour conversion, encode and packetizing took 0.5-0.7 ms per frame at 1080p and about 0.7 ms at 4K. The cost is bandwidth. It needs a few hundred Mbit/s, so it is only for fast wired networks.

Butterpollo's PyroWave stream is wire-compatible with the PyroWave Moonlight clients from [pyrowave-streaming](https://github.com/joemossjr16/pyrowave-streaming): the Artemis fork for Android and the Moonlight-Qt build for Windows, Linux and the Steam Deck. Other Moonlight clients don't know the codec and keep using H.264, HEVC or AV1.

To use it, turn on `pyrowave` (Advanced settings, or `pyrowave = enabled` in `sunshine.conf`) and restart the host. At startup the host checks that the GPU can encode PyroWave and only then offers it. It does 4:2:0 and 4:4:4 in SDR, and 4:2:0 in HDR10. The client picks the bitrate, and the host keeps each frame small enough for the stream's error correction to cover it.

Phones decode PyroWave more slowly than hardware codecs. The Artemis fork measured about 5.7 ms per frame at 1972x1248 on a Snapdragon 8 Elite Gen 5, so use a lower stream resolution there.

## Install

Download `VibepolloSetup.exe` from [Releases](https://github.com/RamazanKara/Butterpollo/releases).

The first releases install as a drop-in replacement for Vibepollo: same install folder, same service, same config and paired devices. Back up `C:\Program Files\Apollo\config` first and keep your current installer in case you want to go back. The installers are unsigned test builds.

To keep an existing Vibepollo virtual controller driver, install with:

```
VibepolloSetup.exe /qn INSTALL_VIRTUAL_GAMEPAD_DRIVER=0
```

Settings I stream with on AMD:

```
encoder = amdvce
amd_usage = ultralowlatency
amd_quality = speed
amd_preanalysis = disabled
amd_av1_latency_mode = lowest
```

## Next

- 120 Hz (1x) was inconclusive: capture dropped to 25-68 fps for part of that test. I'll look at it before touching the 1x path.
- Slot-aligned publishing needs numbers from a live stream. The `WGC helper publish to host claim` line is there to get them.
- PyroWave has passed a GPU self-test (real conversion, encode and framing, decoded and compared against a reference), but not yet a stream to a real client.
- NVIDIA and Intel GPUs have been tested and work. Their encoder code is inherited from Vibepollo unchanged.

Bug reports are welcome if they include GPU and driver, client, resolution/fps/codec, and a few `Host latency stages` log lines.

## Credits and license

Butterpollo is GPL-3.0, like everything it builds on. Thanks to Nonary for Vibepollo, ClassicOldSong for Apollo, and LizardByte and the Sunshine contributors. Everything outside the list above is their work. PyroWave and Granite are by Themaister (MIT), and the PyroWave Moonlight protocol and clients are joemossjr16's work.
