# Butterpollo 2.0.0-rc.9 test candidate for Windows

Butterpollo's host, service, setup and console are now written in Rust. It replaces Vibepollo 2.0.0 and earlier Butterpollo builds on Windows: run `butterpollo-setup-2.0.0-rc.9-wgc-default-test.exe` and it upgrades the existing installation in place, keeping settings, paired devices, the app library and covers. Codec SDKs and Windows drivers remain external components; the setup installs the drivers.

## New in rc.9 (test candidate)

- Automatic capture now prefers Windows Graphics Capture on physical and virtual displays. Existing explicit capture choices are preserved, including imported `dxgi` and `wgcc` aliases.
- WGC uses the existing signed-in-user capture worker under the Windows service, with Desktop Duplication fallback when WGC cannot open and during lock/UAC desktop transitions. Compute copies remain enabled by default on supported AMD GPUs.
- The capture-setting descriptions now explain the new Automatic choice. Frame limiting, display refresh and capture pacing settings are unchanged.
- This is an unpublished comparison candidate. The public rc.8 release remains unchanged. Controlled WGC-versus-DDX picture-freshness, latency and recovery checks are still required before publishing the new default.

## New in rc.8

- Fixes "adding the firewall rule failed" when reinstalling after an automatic update. Internal extended-length paths no longer leak into Windows installation entries or firewall commands. Setup updates an existing rule in place and removes legacy allowances only after Butterpollo's rule succeeds, so a rejected replacement keeps the previous rules.
- Fixes RTSS automatic startup when Windows requires administrator privileges (error 740 / `0x800702E4`). The installed service first tries a normal user launch, then retries that specific error with the signed-in user's elevated token. RTSS stays in the user's desktop session; the SDK helper remains unelevated.
- The same startup path is used during limiter recovery, addressing the elevation failure that left restoration pending in the reporter's rc.6 log. Unrelated launch failures do not trigger an elevated retry. Portable hosts report how to start RTSS manually as administrator.
- Keeps a service-owned RTSS process alive when reconnecting after a failed restoration, and recognizes equivalent installation paths containing dot components or directory junctions.
- Reduces repeated CPU work in WGC's timing predictor by caching statistics when a new frame arrives. Pacing decisions remain unchanged; this is a small CPU optimization.
- Removes an unintended WGC capture throttle at 60 FPS and below by explicitly setting a zero minimum update interval. The untouched Windows default measured 16 ms and delivered only 55–57 capture updates/sec; explicit zero delivered about 217 in the same native background comparison. A brief AV1 background stream then passed the delivery-rate check at 60.585 FPS, versus 58.039 before, with zero decode errors. These checks do not establish distinct-picture delivery or reduced latency. Streams above 60 FPS retain the tested 1 ms request, and WGC compute stays enabled by default. Full motion acceptance for the revised low-rate path remains pending.
- Corrects a diagnostic timestamp that could describe an older frame after memory-address reuse. Adds optional presentation statistics to the motion probe and a polling comparison without frame notifications; these do not change normal streaming.
- Reproduced error 740 locally with RTSS 7.3.5, then verified an earlier rc.8 candidate's automatic startup through the installed Windows service, a measured 59.94 FPS cap, and restoration to 120 FPS on disconnect while Desktop stayed open. All 208 ordinary automated tests and the explicitly selected native WGC interval test passed. Isolated recovery checks cover user edits, absent settings, a pending journal after elevation denial, and reconnection after SDK failure. The final executable is now installed locally: its hash matches the tested candidate, setup completed successfully, the service reports rc.8, and the firewall rule targets the correct executable. The final build's complete limiter lifecycle and confirmation on the reporter's RX 9070 XT remain pending.

## New in rc.7

- Updates notify first by default. Maintenance offers **Install when idle**, with automatic installation available as an opt-in under General settings. Release candidates follow the existing prerelease setting.
- The host downloads only the exact official Windows installer and verifies its size and GitHub SHA-256 digest. Downloads yield to new sessions; installation waits for one minute without streams, pending connections, remote monitors or host apps. New connections are held off during the installation handoff.
- The service updater stages and verifies the package, backs up replaced files, checks that the requested host version starts, and restores the previous package files after ordinary copying or startup failures. Settings, devices and drivers stay in place. Failed versions are not retried automatically.
- The update panel now displays release candidates correctly, along with download progress, cancellation and the last installation result. The basic HTML console also exposes update actions.
- Validation covers idle/session admission, cancellation, official HTTPS downloads, checksum failures, authentication and CSRF, isolated file recovery, and desktop/mobile UI states. The 199 ordinary automated tests and a separate official-download test passed. A full live service upgrade still needs validation; automatic installation remains opt-in.

## New in rc.6

- A client's `virtualDisplay=0` request now inherits the host's virtual-display setting, matching Vibepollo. Previously it disabled the configured virtual display, leaving the physical monitor active or failing capture when that monitor was off. A positive request also preserves the configured shared-display mode. Explicit app display choices still apply, and the device's "Always use a virtual display" option takes precedence.
- Optional PyroWave capability checks now run in a separate user process. A crash or stall there leaves H.264, HEVC and AV1 available; the worker has a 15-second deadline and is cleaned up on failure. This contains the startup failure investigated in an rc.5 dump: RTSS called into the Vulkan loader during a Wallpaper Engine window callback after the probe's Vulkan modules had unloaded. It does not modify either overlay.
- The earlier AV1 fresh-frame shortfall occurred while Warhammer 3 occupied 92–99% of the GPU. With the game closed, the released rc.5 build delivered 59.84 fresh FPS at a 60 FPS target, decoding all 1,047 received frames without errors. This corrects the earlier performance finding; the RX 9070 XT tester's separate latency comparison remains unverified.

## New in rc.5

- Fixes RTSS helper communication in service installations. The helper runs as the signed-in user and could not write its reply into the service's protected configuration folder. It now exchanges bounded messages over a private local pipe, with both process identities checked. The service retains responsibility for writing the RTSS profile; no folder permissions or helper privileges are changed.
- RTSS profile updates and restoration can proceed while the RTSS window holds the profile open without delete sharing. Unknown profile settings and keys that were originally absent are preserved. A failed write no longer leaves recovery pending when the profile never changed. Failures now log their actual cause and RTSS path, instead of only reporting that no limiter provider was available.
- Limiter ownership now follows pending and connected streams, independently of retained game displays. Disconnecting the final stream restores the original cap even when the desktop remains available to resume. Reconnecting reapplies the cap before streaming starts; an abandoned launch also releases its changes when it expires.
- Display logs now record the client's virtual-display request, selected display and layout. A capture startup timeout identifies the selected display and suggests a virtual display when streaming with the physical monitor off. The reported Artemide monitor wake-up issue remains under investigation; these diagnostics do not claim to fix it.
- Verified the installed SYSTEM service applying a 59.94 FPS RTSS cap on an RX 7900 XT. A separate SDK reader confirmed the fractional profile and enabled limiter, and a renderer requesting 240 FPS on a 120 Hz virtual display presented about 59.94 FPS. The service also captured a native HDR AV1 stream through the WGC user helper with compute enabled. The RTSS fixture additionally covers restoration, unrelated flags, helper crashes and a bounded timeout for a stalled SDK.

## New in rc.4

- WGC selected by the installed service now runs capture in a hidden process belonging to the signed-in user. This addresses the `CreateForMonitor` error `0x80070424` observed when SYSTEM tried to open the per-user Windows capture broker. Three synchronized GPU textures transfer frames to the host without CPU readback; compute copies remain enabled on supported AMD GPUs.
- The helper has bounded frame queues, peer identity checks and automatic cleanup. A helper exit reopens capture through the existing recovery path. Lock and UAC desktops select Desktop Duplication, with WGC retried when the normal desktop returns. Logs distinguish requested capture from the actual backend.
- Verified through the installed SYSTEM service on an RX 7900 XT: a signed-in-user helper captured a native HDR virtual display at 2184×1968 for an AV1 120 FPS session, with compute enabled at both ends. The client confirmed correct pictures and responsive input. This confirms the service fix; it is not a controlled 120 FPS performance measurement.
- Portable helper validation passed seven local 720p60 motion cases, including HEVC, AV1, HDR output from an SDR source, and compute disabled. Each sustained 60 fresh pictures per second with no decode errors. Forced helper termination recovered WGC in 357 ms and all 1,249 received pictures decoded successfully. The helper's performance under heavy GPU load remains to be measured.

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

- `butterpollo-setup-2.0.0-rc.9-wgc-default-test.exe` installs or upgrades the host, the `ApolloService` service, the virtual display and gamepad drivers, firewall rules and shortcuts, and can uninstall them. Settings, paired devices, the library and covers are kept.
- For a portable copy, extract `butterpollo-rust-2.0.0-rc.9-wgc-default-test-windows-x64.zip` and open **Start Butterpollo.exe**. The first launch offers to import a Vibepollo or Apollo profile and leaves the original untouched. Install the drivers separately in that case.
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

- Idle motion checks on the 5120×1440 physical display still show intermittent fresh-frame loss in both earlier and revised builds. Full-screen 120 FPS capture improves with the high-rate WGC workaround, but does not yet sustain 120 fresh pictures per second. See [PERFORMANCE.md](PERFORMANCE.md); successful decoding alone is not a smoothness pass.
- The Artemide reporter's phone and RX 9070 XT are unavailable locally. The virtual-display precedence fix addresses a reproduced protocol bug, but confirmation on that phone is still needed.
- The RX 9070 XT report of 4.7 versus 3.9 ms on Wi-Fi remains open pending the tester's comparison. Packet pacing and recovery fixes address observed problems, but are not proof that this latency difference is resolved.
- DDX startup from an inactive desktop remains under investigation. Some local tests received blank pictures and required two capture restarts before valid content arrived; a standalone snapshot test can receive no initial picture. Keeping the display awake fixes continued capture through idle time, but does not resolve this startup condition.
- WGC lock/UAC transitions and the new helper's performance under heavy GPU load are not yet verified. Installed-service WGC and native HDR capture have been confirmed on the RX 7900 XT; that does not establish the RX 9070 XT result.
- AMD's AV1 encoder pads some sizes: 1968×2184 decodes as 1984×2186 ([AMF issue 423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423)); Vibepollo has the same result. HEVC is exact.
- Verified on an AMD RX 7900 XT. NVIDIA and Intel encoders, RTX HDR, a real Playnite and Lossless Scaling, the secure desktop during a stream and streaming the sign-in screen after a reboot are not yet verified on hardware. NVIDIA and Intel keep the graphics-queue capture path.
- Still missing from Vibepollo: choosing the GPU that renders the virtual display, reclaiming virtual displays after a host restart, Playnite focus retries and fullscreen relaunch, `/api/browse`, `/api/apps/{uuid}/icon`, and tray app notifications and state icons.

Details: [PARITY.md](PARITY.md) for each feature and its evidence, [PERFORMANCE.md](PERFORMANCE.md) for measurements and how to reproduce them.
