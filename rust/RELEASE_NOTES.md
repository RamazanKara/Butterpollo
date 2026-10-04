# Butterpollo 2.0.0-rc.2 for Windows

Butterpollo's host, service, setup and console are now written in Rust. It replaces Vibepollo 2.0.0 and earlier Butterpollo builds on Windows: run `butterpollo-setup-2.0.0-rc.2.exe` and it upgrades the existing installation in place, keeping settings, paired devices, the app library and covers. Codec SDKs and Windows drivers remain external components; the setup installs the drivers.

## New in rc.2

- **The mouse pointer no longer disappears.** Desktop Duplication reports the pointer's shape only when it changes, so every capture restart (a mode change, the secure desktop, another client's display arriving) lost the pointer until it changed shape. It is now carried into the new capture.
- **HDR10 metadata in AMF streams.** HEVC and AV1 streams from AMD now carry the mastering display and content light level metadata and signal their colour range, as FFmpeg-based hosts do. Verified in the bitstream for HEVC and AV1, limited and full range.
- **Host errors are no longer silent.** A stream that ends on a host error used to return Moonlight quietly to the app list, as if the stream had been closed on purpose. Moonlight now shows an error with a code, and the host log says why (`session failed`).
- **A packet Windows can't send right away no longer ends the stream.** Video and audio packets are dropped instead, as Vibepollo does, and the video socket gets Vibepollo's 1 MB send buffer.
- **Measured against Vibepollo 2.0.** At 1080p60 HEVC HDR on the same PC with the same encoder settings, the picture reached the client in 13.8 ms against 16.0 ms idle, and in 42.4 ms against 96.4 ms beside a game-like GPU load, with 51 rather than 24 new pictures a second. Decoded HDR colours match the expected values on both hosts.

## Install

- `butterpollo-setup-2.0.0-rc.2.exe` installs or upgrades the host, the `ApolloService` service, the virtual display and gamepad drivers, firewall rules and shortcuts, and can uninstall them. Settings, paired devices, the library and covers are kept.
- For a portable copy, extract `butterpollo-rust-2.0.0-rc.2-windows-x64.zip` and open **Start Butterpollo.exe**. The first launch offers to import a Vibepollo or Apollo profile and leaves the original untouched. Install the drivers separately in that case.
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

- AMD's AV1 encoder pads some sizes: 1968×2184 decodes as 1984×2186 ([AMF issue 423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423)); Vibepollo has the same result. HEVC is exact.
- Verified on an AMD RX 7900 XT. NVIDIA and Intel encoders, RTX HDR, a real Playnite and Lossless Scaling, the secure desktop during a stream and streaming the sign-in screen after a reboot are not yet verified on hardware. NVIDIA and Intel keep the graphics-queue capture path.
- Still missing from Vibepollo: choosing the GPU that renders the virtual display, reclaiming virtual displays after a host restart, Playnite focus retries and fullscreen relaunch, `/api/browse`, `/api/apps/{uuid}/icon`, and tray app notifications and state icons.

Details: [PARITY.md](PARITY.md) for each feature and its evidence, [PERFORMANCE.md](PERFORMANCE.md) for measurements and how to reproduce them.
