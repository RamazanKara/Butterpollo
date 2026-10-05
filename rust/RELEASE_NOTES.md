# Butterpollo 2.0.0-rc.4 for Windows

Butterpollo's host, service, setup and console are now written in Rust. It replaces Vibepollo 2.0.0 and earlier Butterpollo builds on Windows: run `butterpollo-setup-2.0.0-rc.4.exe` and it upgrades the existing installation in place, keeping settings, paired devices, the app library and covers. Codec SDKs and Windows drivers remain external components; the setup installs the drivers.

## New in rc.4 (candidate validation)

- WGC selected by the installed service now runs capture in a hidden process belonging to the signed-in user. This addresses the `CreateForMonitor` error `0x80070424` observed when SYSTEM tried to open the per-user Windows capture broker. Three synchronized GPU textures transfer frames to the host without CPU readback; compute copies remain enabled on supported AMD GPUs.
- The helper has bounded frame queues, peer identity checks and automatic cleanup. A helper exit reopens capture through the existing recovery path. Lock and UAC desktops select Desktop Duplication, with WGC retried when the normal desktop returns. Logs distinguish requested capture from the actual backend.
- Portable helper validation on the RX 7900 XT passed seven local 720p60 motion cases, including HEVC, AV1, HDR output from an SDR source, and compute disabled. Each sustained 60 fresh pictures per second with no decode errors. Forced helper termination recovered WGC in 357 ms and all 1,249 received pictures decoded successfully. These results do not establish performance under GPU load, native HDR capture, or the installed SYSTEM service; those checks remain pending for this candidate.

## New in rc.3

- A stream configured for WGC now falls back to Desktop Duplication if WGC cannot open at startup, matching capture recovery. Logs preserve the Windows error and identify the fallback. This keeps the stream available; it does not provide service-mode WGC or establish equivalent VRR/frame-generation capture.
- **WGC compute copies are now on by default on supported AMD GPUs.** On the RX 7900 XT fixture, mean decoded picture age fell from 14.8 to 11.5 ms at idle and from 52.9 to 34.6 ms under heavy GPU load. Loaded fresh-picture delivery was about 3% lower (51.4 to 50.0 FPS); neither path sustained 60 FPS under that stress. Unsupported sharing falls back to graphics copies. Set `wgc_compute_copy=false` in the configuration, restart the host and reconnect to compare; `gpu_compute_conversion=false` disables all compute copies and conversion. These measurements do not establish the RX 9070 XT result.
- **Capture recovery no longer adds a fixed 150 ms pause.** Each active stream releases its old encoder, frames and filters before capture reopens. Failed reopen attempts retain a bounded retry delay.
- If compute cannot share a captured texture, fallback now copies that same frame. Previously a static desktop could stall while waiting for another update. Desktop Duplication also logs its actual format and dimensions on each open to help diagnose restarts.
- Active capture now keeps the display awake, matching Vibepollo. The request ends with the capture worker and restores the thread's previous power requirements. This addresses capture being starved when Windows turns off an idle display.
- Video packet pacing no longer produces catch-up bursts after a late send. It accounts for wire overhead and caps known local Ethernet routes at 80% of link speed. A wired host still cannot infer a Wi-Fi client's capacity; the reported RX 9070 XT latency difference remains under investigation.

## New in rc.2

- **The mouse pointer no longer disappears.** Desktop Duplication reports the pointer's shape only when it changes, so every capture restart (a mode change, the secure desktop, another client's display arriving) lost the pointer until it changed shape. It is now carried into the new capture.
- **HDR10 metadata in AMF streams.** HEVC and AV1 streams from AMD now carry the mastering display and content light level metadata and signal their colour range, as FFmpeg-based hosts do. Verified in the bitstream for HEVC and AV1, limited and full range.
- **Host errors are no longer silent.** A stream that ends on a host error used to return Moonlight quietly to the app list, as if the stream had been closed on purpose. Moonlight now shows an error with a code, and the host log says why (`session failed`).
- **A packet Windows can't send right away no longer ends the stream.** Video and audio packets are dropped instead, as Vibepollo does, and the video socket gets Vibepollo's 1 MB send buffer.
- **Measured against Vibepollo 2.0.** At 1080p60 HEVC HDR on the same PC with the same encoder settings, the picture reached the client in 13.8 ms against 16.0 ms idle, and in 42.4 ms against 96.4 ms beside a game-like GPU load, with 51 rather than 24 new pictures a second. Decoded HDR colours match the expected values on both hosts.

## Install

- `butterpollo-setup-2.0.0-rc.4.exe` installs or upgrades the host, the `ApolloService` service, the virtual display and gamepad drivers, firewall rules and shortcuts, and can uninstall them. Settings, paired devices, the library and covers are kept.
- For a portable copy, extract `butterpollo-rust-2.0.0-rc.4-windows-x64.zip` and open **Start Butterpollo.exe**. The first launch offers to import a Vibepollo or Apollo profile and leaves the original untouched. Install the drivers separately in that case.
- These are unsigned test builds. Keep a copy of your configuration and the previous installer for rollback.

## Lower latency

- On AMD GPUs, captured frames are copied and converted on D3D12 compute queues, and AMF encodes from D3D12. The game no longer delays this work on the graphics engine. In a full AV1 HDR 1968×2184 120 fps stream beside a heavy GPU load, the picture arrived 7-9 ms sooner, and 50 rather than 45 new pictures reached the client each second. Idle, it arrived about 0.6 ms sooner; encoding was already at the hardware limit (2.8-3.3 ms). `gpu_compute_conversion` (Settings › Capture) turns this off.
- Defaults that measured lower latency: Desktop Duplication capture (WGC for VRR and game frame generation on a virtual display), AMF `speed` quality, and a virtual display at twice the stream rate. Setting Vibepollo's values restores its behaviour.
- Video FEC uses 21-29% less CPU time than the C++ host, with identical output; encrypted packets avoid extra copies.

## Vibepollo 2.0 features

- Moonlight pairing (PIN and one-time PIN), per-device permissions, display modes and overrides, `/bitrate`, `/unpair`, and the virtual display, permission and frame limiter replies of Vibepollo.
- H.264, HEVC, AV1, HDR, 10-bit SDR, 4:4:4 where supported, PyroWave and VRR with Nonary's Moonlight client, and reference frame invalidation.
- Per-device virtual displays with exclusive, extended, primary and isolated layouts, golden layout restore and the restore hotkey.
- Steam library sync with covers, and streams that end when the Steam game exits. Playnite through Vibepollo's plugin (shipped in the package), including the fullscreen app. Lossless Scaling profiles and frame generation per app.
- Frame limiting through RTSS and NVIDIA profiles, NVIDIA Smooth Motion and RTX HDR.
- A new web console: overview with performance history, library with Steam and Playnite, devices, settings, logs, maintenance and API tokens.
- Tray notifications for pairing requests and new versions; release checks that skip streams.

Left out on purpose: WebRTC streaming, session history and host statistics pages, the ViGEm and SudoVDA fallbacks, and Linux and macOS hosting.

## Known limits

- The RX 9070 XT report of 4.7 versus 3.9 ms on Wi-Fi remains open pending the tester's comparison. Packet pacing and recovery fixes address observed problems, but are not proof that this latency difference is resolved.
- DDX startup from an inactive desktop remains under investigation. Some local tests received blank pictures and required two capture restarts before valid content arrived; a standalone snapshot test can receive no initial picture. Keeping the display awake fixes continued capture through idle time, but does not resolve this startup condition.
- The rc.4 WGC helper still needs validation through the installed SYSTEM service, on a native HDR virtual display, and across lock/UAC transitions. Portable helper tests cover GPU transfer, codec output and recovery but cannot prove those privileged session paths.
- AMD's AV1 encoder pads some sizes: 1968×2184 decodes as 1984×2186 ([AMF issue 423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423)); Vibepollo has the same result. HEVC is exact.
- Verified on an AMD RX 7900 XT. NVIDIA and Intel encoders, RTX HDR, a real Playnite and Lossless Scaling, the secure desktop during a stream and streaming the sign-in screen after a reboot are not yet verified on hardware. NVIDIA and Intel keep the graphics-queue capture path.
- Still missing from Vibepollo: choosing the GPU that renders the virtual display, reclaiming virtual displays after a host restart, Playnite focus retries and fullscreen relaunch, `/api/browse`, `/api/apps/{uuid}/icon`, and tray app notifications and state icons.

Details: [PARITY.md](PARITY.md) for each feature and its evidence, [PERFORMANCE.md](PERFORMANCE.md) for measurements and how to reproduce them.
