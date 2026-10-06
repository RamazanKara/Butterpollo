# Butterpollo and Vibepollo 2.0

Butterpollo's Rust host is meant to replace Vibepollo 2.0.0 (commit `8a8c4b03a280ab9f567beb380110abb80f5220b8`) on Windows. This file lists what matches, what differs on purpose and what is still missing, as of 2026-10-06. "Same" means the Rust host reads the same settings and files and gives clients the same answers; it does not mean every hardware path has been exercised. Evidence is at the end.

Left out on purpose: WebRTC streaming, session history and host statistics pages, the ViGEm and SudoVDA fallbacks, and Linux and macOS hosting.

## Deliberate differences

The table compares the current defaults. In rc.9, Automatic capture prefers WGC; WGC-selected streams also use guarded source-phase pacing, while explicit Desktop Duplication keeps its previous pacing default. See [PERFORMANCE.md](PERFORMANCE.md) for measured benefits, fallback behavior and hardware limits.

| Setting | Vibepollo default | Butterpollo default |
| --- | --- | --- |
| `capture` (automatic) | Windows Graphics Capture on Windows 11 23H2 and later | Windows Graphics Capture on physical and virtual displays, with Desktop Duplication fallback when WGC cannot open |
| `amd_quality` | `balanced` | `speed` |
| `frame_limiter_auto_virtual_framegen` | `enabled` (virtual display at 4x the stream rate) | `legacy` (2x); a VRR request still gets 1000 Hz |

Settings changed in the console are saved at once and most take effect from the next stream; Vibepollo applies some (log level, display revert, RTX HDR) to a running host. The console says when a restart is needed.

## Feature by feature

| Area | Status | Notes |
| --- | --- | --- |
| Profile and settings | Same | Imports a Vibepollo or Apollo profile in place. The parser accepts what Vibepollo accepts (BOM, quoted and hex numbers, lists, enum spellings, `driver_decides`, `wgcc`) and logs values it ignores. An invalid `apps.json` starts the host with no apps and is kept as `apps.json.invalid`. The shared virtual display GUID in `vibeshine_state.json` is reused. |
| Per-app and per-device overrides | Same | Only Vibepollo's list of stream, input, display and encoder keys can be overridden; other keys are logged and ignored. |
| Pairing | Same | PIN and one-time PIN (`/api/otp`), pending requests in the console, per-device permissions and enable switch. |
| Moonlight endpoints | Same | `serverinfo` extras (virtual display, frame limiter, permissions, server commands), `/applist` with Vibepollo's placeholder for devices without the list permission, `/launch` and `/resume` with `VirtualDisplayDriverReady`, `/unpair` on HTTP and HTTPS, `/bitrate` capped by `max_bitrate` and 500 Mbps, ABR capability reported as client-driven. |
| Codecs and stream | Same | H.264, HEVC, AV1, HDR, 10-bit SDR, 4:4:4 where the encoder supports it, PyroWave, FEC, reference frame invalidation (AMF and NVENC). rc.6 isolates optional PyroWave capability checks in a bounded user process, so a driver/overlay crash or stall during that check cannot terminate the host. |
| Capture | Same; hardware limits below | Desktop Duplication with recovery. Service-mode WGC uses a signed-in-user helper, avoiding SYSTEM's `CreateForMonitor` error `0x80070424`. GPU-only frame transfer and compute copies were verified through the installed service with native HDR on an RX 7900 XT. Lock/UAC selects Desktop Duplication and returning to the normal desktop retries WGC; these transitions, VRR and game-frame-generation operation still need hardware validation with the helper. |
| Virtual displays | Mostly the same | Per device, shared or off; layouts exclusive, extended, primary, isolated; HDR; permanent count (also the old `dd_vdd_static_monitor_count`). rc.6 treats `virtualDisplay=0` as no client opt-in, preserving the host policy; a positive opt-in preserves shared mode. Device-forced virtual displays take precedence over physical app output. Missing: choosing the GPU that renders the virtual display (`adapter_name` only picks the capture and encode GPU), reclaiming displays after a host restart (driver protocol 3.7), and creating one automatically on a host with no active display. |
| Display layout | Same | Golden layout restore (skipped while a display it names is disconnected), restore after a stream or crash, mode remapping, the display restore hotkey (`dd_snapshot_restore_hotkey`). `dd_wa_dummy_plug_hdr10` turns VSync off but does not force HDR on. |
| Device display mode | Same | A device's `display_mode` sets its display's resolution and refresh in place of the host's policies; the stream keeps the client's rate. |
| Letterboxing | Same | A source of another shape keeps its aspect ratio between black bars, in the GPU and the software encoders. |
| Input | Implemented; driver and validation limits | Keyboard (key code mask, synthetic modifiers and extended keypad Enter), mouse, touch, pen, controllers through the VHF driver, DualSense triggers and feedback. Controller touch packets preserve their touchpad index, but the bundled driver supports one surface with two contacts; secondary-pad events are safely ignored. Keypad hold/repeat/release and touchpad isolation have automated coverage. Input was disabled in the exact Moonlight PC 6.2.0 streaming matrix, so that matrix does not provide a new live-input validation. |
| Audio | Same | Endpoint matching by id, name, description or adapter; Steam Streaming Speakers; surround Opus. |
| Apps | Same | Commands, preparation and undo, detached commands, URLs and documents, working folder inference, `APOLLO_*` variables, starting before sign-in. |
| Steam library | Same | `steam_*` settings, sync on demand and every 30 seconds, covers from Steam's cache or store as PNG, `/api/steam/*`. A Steam app's stream ends when the game's processes exit. |
| Playnite | Mostly the same | Talks to Vibepollo's Playnite plugin (shipped in the package, installed from the console). Apps with a `playnite-id` start through Playnite with the stream's environment and end when Playnite reports the game stopped; ending the stream closes the game's processes. The library syncs recent, category, plugin or all installed games with Vibepollo's rules, and the "Playnite (Fullscreen)" app opens fullscreen mode. Missing: window focus retries (`playnite_focus_*`), relaunching fullscreen mode after a game, setting covers back into Playnite, `/api/playnite/cover` and `/launch`. |
| Lossless Scaling | Mostly the same | The app's `lossless-scaling-*` fields become Vibepollo's "Vibeshine" profile for the game's programs; Lossless Scaling is restarted and its hotkey pressed (or auto scale with `lossless_scaling_legacy_auto_detect`), and closed with the profile removed afterwards. Frame generation holds the game at the app's limit or half the target. The game is the first new windowed process (in its Steam or Playnite folder when known) rather than Vibepollo's CPU and memory scoring, and it is not re-targeted if the game changes process. |
| Frame limiting | Implemented; RTSS service cap verified | RTSS and NVIDIA profiles, game-provided frame generation, NVIDIA Smooth Motion and Lossless Scaling frame generation. rc.5 replaces RTSS's user-helper reply files in the protected service directory with private pipes and handles RTSS's profile file lock. The installed SYSTEM service applied a live 59.94 FPS RTSS cap on the RX 7900 XT, confirmed by an independent SDK read and renderer timing. Limiter ownership is separate from retained displays so disconnect restores and resume reacquires it. Simulated SDK apply/restore/crash/timeout tests cover failures; NVIDIA remains unverified on hardware. |
| Web console | Rebuilt | A new console (Svelte) covers overview, library (with Steam and Playnite), devices, settings, logs, maintenance and API tokens, and per-app Lossless Scaling. Missing endpoints: `/api/browse`, `/api/apps/{uuid}/icon`. |
| Service and setup | Same | `setup.exe` upgrades a Vibepollo installation in place (drivers, service, firewall, shortcuts) and can uninstall it. The service restarts the host after a crash. `--creds` sets the console sign-in. The service's credentials folder is limited to SYSTEM and Administrators at every start. |
| Tray | Mostly the same | Open, disconnect, restart and quit; notifications for pairing requests, paired devices and new versions. Missing: app started or stopped notifications, state icons and force-closing the app. |
| Updates | Implemented | Notifies first by default, with opt-in automatic installation. Downloads the exact Windows installer from the official GitHub release, verifies its size and SHA-256 digest, waits for one minute without active/pending sessions, remote monitors or host apps, and upgrades through the installed service. Failed copying/startup restores previous package files. Portable builds retain release-page downloads. Release candidates follow the existing prerelease opt-in. |
| Logs and support | Same | Rotating logs (`log_path`), live tail in the console, crash dumps and support bundle. |

## Evidence

- AMD AV1's raw bitstream remains padded at some sizes on the RX 7900 XT: 1920×1080 decodes as 1920×1082, 1968×2184 as 1984×2186, and 2184×1968 as 2240×1968. The independent exact-geometry gate still fails for these cases in SDR and HDR, including AMF alignment modes 3 (`NO_RESTRICTIONS`) and 4 (`8X2_ONLY`). [Moonlight PC 6.2.0 explicitly crops RDNA3 padding](https://github.com/moonlight-stream/moonlight-qt/blob/v6.2.0/app/streaming/video/ffmpeg.cpp#L1772) back to the negotiated size when each padding amount is below 64 pixels; its [D3D11 renderer uses the cropped dimensions](https://github.com/moonlight-stream/moonlight-qt/blob/v6.2.0/app/streaming/video/ffmpeg-renderers/d3d11va.cpp#L819). Three exact-client checks passed this metadata path: 1920×1080 SDR and HDR, plus 1968×2184 SDR. Client logs confirmed the padded coded sizes, expected eight/ten-bit decoding, cropping to the negotiated dimensions and D3D11 rendering, with no decoder errors and normal streaming-window disconnects. These checks used host SHA-256 `cbc837071d1b27a02050a3db1f16ec18aa444e54b4ecacd49c33b24534491ca6`. They do not validate displayed edge pixels, native HDR source/display accuracy, distinct-picture smoothness, or the untested 2184×1968 client case. The strict raw-bitstream gate remains unchanged.
- October 5 rc.4 helper: 188 ordinary workspace tests pass, including five new transport/ownership checks; 22 hardware tests are excluded from that count. Seven local 720p60 motion cases cover direct WGC, the helper, HEVC, AV1, HDR output from an SDR source and compute disabled, all at 60 fresh pictures per second without decode errors. Forced helper termination reopened WGC in 357 ms; all 1,249 received pictures decoded. After a normal installer upgrade, the SYSTEM service launched the helper as the signed-in user and captured a 2184×1968 native HDR virtual display for an AV1 120 FPS session with compute enabled at both ends. The client confirmed correct pictures and responsive input. This live session is functional evidence, not a controlled throughput or latency comparison.
- October 5 final workspace/native run: 200 pass, including 20 opt-in native checks, on an active moving desktop. This includes opt-in WGC compute synchronization and same-frame fallback after a sharing failure. The known failing AMD AV1 geometry check and unavailable NVIDIA hardware check were excluded; AV1 was also run separately and still fails. The standalone DDX snapshot check receives no frame with the display off but passes on the active desktop; the separate cold-start display transitions remain unresolved. Clippy with warnings denied and the web console's type check pass.
- Native AMD tests on the test PC: GPU colour and letterbox conversion, cursor composition, AMF loss recovery, Opus surround, WGC teardown, and the software encoder letterbox test.
- Independent wired LAN receiver on the Intel NUC: H.264 and WGC/HEVC at 1080p60, HEVC HDR and aligned AV1 at 720p60, exact decoded geometry, pixel contrast and nonzero Opus audio. A 230-second HEVC stream decoded 13,838/13,838 pictures and continued fresh capture claims beyond the display's idle timeout. The NUC's 4K60 hardware readback test failed the throughput requirement and is not counted as a performance pass. Wi-Fi and the reporter's RX 9070 XT remain unverified.
- Live on the test PC with the installed service: phone streaming at 1968x2184, 120 Hz, HDR, on a per-device virtual display; two clients streaming at once on their own displays; capture recovery after a lost Desktop Duplication session; OTP pairing; the installer upgrading the Vibepollo installation in place.
- Steam: discovery of 17 installed apps across three libraries, appinfo names and types, play history, covers (including store downloads), and a sync that adds the apps once and then reports no change.
- Playnite, against a stand-in that speaks the plugin's protocol (Playnite is not installed on the test PC): plugin install, library sync with a converted cover, a launch that passes the stream's 88 environment variables and ends on `gameStopped`, and closing the app mid-game.
- Lossless Scaling, with a stand-in program and settings file (Lossless Scaling is not installed on the test PC): the profile names the game's program and is removed when the app closes.

Not verified on hardware: NVIDIA and Intel encoders, RTX HDR, a real Playnite with the plugin, a real Lossless Scaling (its hotkey and window handling), the secure desktop during a stream (UAC, lock screen) and streaming the sign-in screen after a reboot, a Steam game's stream ending when the game exits, the restore hotkey and tray notifications.

## Reproducible verification

```powershell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p butterpollo-windows --locked -- --ignored --skip native_nvenc_loss_recovery_and_444_hdr_decode --skip native_av1_geometry_and_hdr_are_preserved --test-threads=1 --nocapture
```

The native command needs the packaged codec DLLs on `PATH`, an AMD D3D11/AMF adapter, working interactive WGC, a moving desktop and a local network route; it does not change display modes, audio defaults or the installed service. `motion_probe DISPLAY SECONDS REPORT.json current 128` can provide movement on an explicitly selected active output without changing its mode. Set `BUTTERPOLLO_TEST_OPUS_ROOT` to the packaged runtime directory, `BUTTERPOLLO_TEST_FFMPEG` to an independent FFmpeg decoder and `BUTTERPOLLO_TEST_RFI_REPORT` to a report file. AMD AV1 exact-size decoding of unaligned sizes (such as 1968x2184) is a driver limitation; HEVC passes the same sizes.

On an NVIDIA host with a display attached to that adapter:

```powershell
$env:BUTTERPOLLO_TEST_NVENC = '1'
$env:BUTTERPOLLO_TEST_NVENC_REPORT = 'C:\path\to\artifacts\nvenc.json'
$env:BUTTERPOLLO_TEST_FFMPEG = 'C:\path\to\ffmpeg.exe'
cargo test -p butterpollo-windows --locked native_nvenc_loss_recovery_and_444_hdr_decode -- --ignored --test-threads=1 --nocapture
```

`tests/web_api.py`, `tests/console_browser.cjs`, `tests/session_restart.py`, `tests/interop.py` and `tests/otp_pairing.py` run against isolated test-owned profiles and stop only their own processes. See [PERFORMANCE.md](PERFORMANCE.md) for measurements.
