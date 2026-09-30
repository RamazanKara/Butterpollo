# Butterpollo Rust

The Windows host, protocol implementation and native helpers are written in Rust. The executables do not link the previous Butterpollo C++ host. The existing Vue administration app is served by Rust; codec and GPU SDKs retain their vendor C ABIs.

This is the Rust replacement under development on `codex/butterpollo-rust`, based on `2.0.0-beta.3-butter.4`. Do not interpret the C++ performance measurements in the parent README as measurements of this implementation. The installed production service is independent of this checkout.

## Build

Requirements: Windows x64, MSYS2 UCRT64 with GCC, Clang, CMake, Ninja, Vulkan headers, Opus and oneVPL; Rust 1.98.1 GNU; and a Microsoft x64 C++ SDK/toolchain for the NVIDIA adapter. The host uses GNU codec libraries; only the small Rust TrueHDR DLL uses the MSVC target to link NVIDIA's SDK library.

```powershell
rustup toolchain install 1.98.1-x86_64-pc-windows-gnu --profile minimal --component rustfmt --component clippy
rustup target add x86_64-pc-windows-msvc --toolchain 1.98.1-x86_64-pc-windows-gnu

# Build the retained web app with Node 24 and npm ci.
$env:SUNSHINE_WEB_OUTPUT_DIR = "$env:LOCALAPPDATA\ButterpolloRust\web"
Push-Location src_assets/common/assets/web
npm ci
npm run build
Pop-Location

.\rust\build.ps1 -FetchDependencies -Package -WebAssets $env:SUNSHINE_WEB_OUTPUT_DIR
```

Run in a Visual Studio x64 developer PowerShell for TrueHDR. Alternatively pass `-MsvcSdk` pointing to an xwin layout with `crt/lib/x86_64`, `sdk/lib/um/x86_64` and `sdk/lib/ucrt/x86_64`. `-SkipTrueHdr` builds without the optional NVIDIA DLL. Existing SDKs can be supplied with `-FfmpegRoot`, `-PyrowaveRoot` and `-NvidiaRoot`; downloads are pinned and checked. CMake is used to build the external PyroWave SDK, not the host.

The script checks formatting, tests, lints, builds the two executables and the optional TrueHDR DLL, then packages runtime libraries, web assets, notices and a SHA-256 manifest. A locked Cargo dependency tree and pinned SDK revisions are included. CI is `.github/workflows/rust-windows.yml`.

## Run and migration

```powershell
.\butterpollo.exe --config-dir C:\path\to\a\config-copy --bind 0.0.0.0
```

Without arguments the host binds to loopback and uses `%LOCALAPPDATA%\ButterpolloRust\config`; the web interface is `https://localhost:47990`. Initial credential setup requires a local connection. Use `--port 48123 --bind 127.0.0.1` for an isolated instance: web 48124, HTTPS 48118 and RTSP 48144. Standard Moonlight UDP port offsets remain compatible.

Copy the complete original configuration directory, including certificates, `sunshine_state.json`, `vibeshine_state.json`, `apps.json` and `sunshine.conf`, before testing migration. Absolute paths in the copied configuration still refer to their original locations; change those paths to the copy when isolating it. Credentials, certificate identities, app UUIDs, artwork IDs, permissions and unknown configuration/state fields are preserved. Imported legacy clients and booleans are normalized. State writes replace files atomically.

The service uses `ApolloService` for compatibility and `%PROGRAMDATA%\Butterpollo\config`. `service.ps1` manages the Rust installation and refuses to alter a service belonging to another executable. Service installation is a separate explicit action; running or building the host never installs it.

## Implementation

| Crate | Responsibility |
| --- | --- |
| `core` | NV pairing, AES/RSA, RTSP/SDP, media encryption, Cauchy FEC, input parsing, audio mixing/resampling, app identities, permissions and durable state |
| `windows` | DXGI/WGC, HDR color conversion/ICC leases, AMF, NVENC/QSV/software encoding, PyroWave, NGX bridge, WASAPI/Opus, SendInput/touch/pen/VHF, clipboard, temporary/permanent displays and recovery, process jobs, tray and SCM |
| `host` | TLS/HTTP, Moonlight endpoints, administration/auth, encrypted RTSP, ENet control, UDP media, scheduling and lifecycle |
| `truehdr-runtime` | Rust MSVC DLL directly calling NVIDIA's NGX C ABI |

There is no WebRTC, SudoVDA, ViGEm, FFmpeg AMF encoder wrapper or legacy display helper in the Rust build. Capture is currently copied to CPU memory for the encoder path. Native GPU surface transfer and the old 120 fps latency target require further performance work.

## Validation

Native Windows unit tests cover malformed wire input, authenticated encryption, replay rejection, legacy CBC/GCM behavior, FEC packet boundaries, scalar/SIMD equivalence, audio rates/channel masks, configuration/state migration, scoped API tokens and Windows job teardown. Independent `tests/interop.py` and `tests/moonlight_client.c` perform real PIN pairing, encrypted RTSP, video transport/decryption/FEC, FFmpeg decode and Opus decode. `tests/pyrowave_client.c` decodes the Rust encoder's container through the external vendor decoder.

The test machine is an AMD RX 7900 XT with an HDR display. Sustained release streams at 640×480, 30 fps passed H.264, HEVC, AV1, HEVC Main10 HDR and AV1 Main10 HDR decoding. HDR frames contain BT.2020/PQ metadata. PyroWave SDR and HDR container decoding also passed; the independent PyroWave decoder probe checks the HDR container flag and decoded output, not a complete ten-bit client rendering path. These checks establish interoperability, not 4K/120 fps performance. Unoptimized Rust builds are unsuitable for streaming performance checks.

The administration tests cover anonymous and authenticated CSRF, scoped tokens, refresh/session revocation, app CRUD, covers, output redirection and exit monitoring, display layout validation, baseline capture/comparison, maintenance status and support ZIP integrity. Browser checks cover real sign-in and all eight main pages at desktop and mobile sizes. Native Windows tests verify owned process-tree termination and a valid minidump; the separate recovery probe verifies abrupt parent death and preservation of a newer journal owner.

NVIDIA/Intel hardware encoding, NVIDIA TrueHDR conversion, VHF controllers, the compatible VDD driver, secure-desktop input and an actual SCM-installed service require their respective hardware/privileges. Tests do not install or replace the existing service or change the user's physical display modes.

## Remaining compatibility work

Full production parity is not yet established. Retained remote-monitor leases/topology, independent input sessions, permanent monitor counts, per-client HDR ICC leases, client connection/disconnection commands, app output/exit/auto-detach policies, native crash reports, support bundles and release checks are implemented. Application configuration overrides precede client overrides; resolution and refresh policies are independent. Permanent counts are applied only when `dd_virtual_display_permanent_count` is explicitly configured. ICC changes restore the prior association after the last lease or a crash, provided the association still matches the host's change. Manual refresh rates are rounded to integer hertz by the current GDI mode setter.

Display topology and monitor lifetimes need validation with the compatible VDD driver. Exclusive virtual-display layouts, frame limiter/RTSS/Steam overrides and the Vulkan interception layer remain unported. Integration status reports their availability without claiming they are active. Configuration keys are retained even where their behavior has not been ported. The legacy `src`, CMake and installer sources remain as a migration reference; this Rust build does not compile them.

Useful probes: `--diagnostics`, `--capture-smoke --hdr`, `--encoder-smoke amf --codec hevc --hdr` and `--encoder-smoke pyrowave --codec pyrowave --encoder-output frame.bin`. Display recovery journals only changes owned by the host and restores them after parent-process death, provided the user has not subsequently changed that setting.
