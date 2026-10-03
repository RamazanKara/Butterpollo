# Butterpollo and Vibepollo 2.0

Butterpollo's Rust host is meant to replace Vibepollo 2.0.0 (commit `8a8c4b03a280ab9f567beb380110abb80f5220b8`) on Windows. This file lists what matches, what differs on purpose and what is still missing, as of 2026-10-03. "Same" means the Rust host reads the same settings and files and gives clients the same answers; it does not mean every hardware path has been exercised. Evidence is at the end.

Left out on purpose: WebRTC streaming, session history and host statistics pages, the ViGEm and SudoVDA fallbacks, and Linux and macOS hosting.

## Deliberate differences

Three defaults differ because they measured lower latency on the test PC (see [PERFORMANCE.md](PERFORMANCE.md)). Setting the Vibepollo value restores Vibepollo's behaviour.

| Setting | Vibepollo default | Butterpollo default |
| --- | --- | --- |
| `capture` (automatic) | Windows Graphics Capture on Windows 11 23H2 and later | Desktop Duplication, except for VRR and game-provided frame generation on a virtual display |
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
| Codecs and stream | Same | H.264, HEVC, AV1, HDR, 10-bit SDR, 4:4:4 where the encoder supports it, PyroWave, FEC, reference frame invalidation (AMF and NVENC). |
| Capture | Same, different default | Desktop Duplication and WGC with recovery; input and capture follow the secure desktop (UAC, lock screen). |
| Virtual displays | Mostly the same | Per device, shared or off; layouts exclusive, extended, primary, isolated; HDR; permanent count (also the old `dd_vdd_static_monitor_count`). Missing: choosing the GPU that renders the virtual display (`adapter_name` only picks the capture and encode GPU), reclaiming displays after a host restart (driver protocol 3.7), and creating one automatically on a host with no active display. |
| Display layout | Same | Golden layout restore (skipped while a display it names is disconnected), restore after a stream or crash, mode remapping, the display restore hotkey (`dd_snapshot_restore_hotkey`). `dd_wa_dummy_plug_hdr10` turns VSync off but does not force HDR on. |
| Device display mode | Same | A device's `display_mode` sets its display's resolution and refresh in place of the host's policies; the stream keeps the client's rate. |
| Letterboxing | Same | A source of another shape keeps its aspect ratio between black bars, in the GPU and the software encoders. |
| Input | Same | Keyboard (key code mask, synthetic modifiers), mouse, touch, pen, controllers through the VHF driver, DualSense triggers and feedback. |
| Audio | Same | Endpoint matching by id, name, description or adapter; Steam Streaming Speakers; surround Opus. |
| Apps | Same | Commands, preparation and undo, detached commands, URLs and documents, working folder inference, `APOLLO_*` variables, starting before sign-in. |
| Steam library | Same | `steam_*` settings, sync on demand and every 30 seconds, covers from Steam's cache or store as PNG, `/api/steam/*`. A Steam app's stream ends when the game's processes exit. |
| Playnite | Missing | No plugin connection, sync or Playnite launches yet. Imported apps that only have a `playnite-id` stream the desktop without starting the game. |
| Lossless Scaling | Missing | `lossless_scaling_*` settings and the app's `lossless-scaling-*` fields are ignored. |
| Frame limiting | Same | RTSS and NVIDIA profiles, game-provided frame generation, NVIDIA Smooth Motion; not the Lossless Scaling provider. |
| Web console | Rebuilt | A new console (Svelte) covers overview, library, devices, settings, logs, maintenance and API tokens. Missing endpoints: `/api/browse`, `/api/playnite/*`, `/api/lossless_scaling/status`, `/api/apps/purge_autosync`, `/api/apps/{uuid}/icon`. |
| Service and setup | Same | `setup.exe` upgrades a Vibepollo installation in place (drivers, service, firewall, shortcuts) and can uninstall it. The service restarts the host after a crash. `--creds` sets the console sign-in. The service's credentials folder is limited to SYSTEM and Administrators at every start. |
| Tray | Partial | Open, disconnect, restart and quit. No notifications (pairing requests, app started or stopped, new version) and no force-close of the app. |
| Updates | Partial | Lists releases; does not compare versions or notify. |
| Logs and support | Same | Rotating logs (`log_path`), live tail in the console, crash dumps and support bundle. |

## Evidence

- Workspace tests: 166 pass (`cargo test --locked --release --workspace`), Clippy with warnings denied, and the web console's type check.
- Native AMD tests on the test PC: GPU colour and letterbox conversion, cursor composition, AMF loss recovery, Opus surround, WGC teardown, and the software encoder letterbox test.
- Live on the test PC with the installed service: phone streaming at 1968x2184, 120 Hz, HDR, on a per-device virtual display; two clients streaming at once on their own displays; capture recovery after a lost Desktop Duplication session; OTP pairing; the installer upgrading the Vibepollo installation in place.
- Steam: discovery of 17 installed apps across three libraries, appinfo names and types, play history, covers (including store downloads), and a sync that adds the apps once and then reports no change.

Not verified on hardware: NVIDIA and Intel encoders, RTX HDR, Lossless Scaling and Playnite (not implemented), the secure desktop during a stream (UAC, lock screen) and streaming the sign-in screen after a reboot, a Steam game's stream ending when the game exits, and the restore hotkey.

## Reproducible verification

```powershell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p butterpollo-windows --locked -- --ignored --skip native_nvenc_loss_recovery_and_444_hdr_decode --skip native_av1_geometry_and_hdr_are_preserved --test-threads=1 --nocapture
```

The native command needs the packaged codec DLLs on `PATH`, an AMD D3D11/AMF adapter and a local network route; it does not change display modes, audio defaults or the installed service. Set `BUTTERPOLLO_TEST_OPUS_ROOT` to the packaged runtime directory, `BUTTERPOLLO_TEST_FFMPEG` to an independent FFmpeg decoder and `BUTTERPOLLO_TEST_RFI_REPORT` to a report file. AMD AV1 exact-size decoding of unaligned sizes (such as 1968x2184) is a driver limitation; HEVC passes the same sizes.

On an NVIDIA host with a display attached to that adapter:

```powershell
$env:BUTTERPOLLO_TEST_NVENC = '1'
$env:BUTTERPOLLO_TEST_NVENC_REPORT = 'C:\path\to\artifacts\nvenc.json'
$env:BUTTERPOLLO_TEST_FFMPEG = 'C:\path\to\ffmpeg.exe'
cargo test -p butterpollo-windows --locked native_nvenc_loss_recovery_and_444_hdr_decode -- --ignored --test-threads=1 --nocapture
```

`tests/web_api.py`, `tests/console_browser.cjs`, `tests/session_restart.py`, `tests/interop.py` and `tests/otp_pairing.py` run against isolated test-owned profiles and stop only their own processes. See [PERFORMANCE.md](PERFORMANCE.md) for measurements.
