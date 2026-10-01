# Butterpollo Rust

The Windows host, protocol implementation, native helpers and administration console are written in Rust. The executables do not link the previous Butterpollo C++ host. Rust renders the console and handles ordinary HTML forms; no JavaScript, Vue or Node build is needed. Codec libraries, device drivers and GPU SDKs remain external dependencies.

This is the Rust replacement under development on `codex/butterpollo-rust`, based on `2.0.0-beta.3-butter.4`. Do not interpret the C++ performance measurements in the parent README as measurements of this implementation. The installed production service is independent of this checkout.

## Build

Requirements: Windows x64, MSYS2 UCRT64 with GCC, Clang, CMake, Ninja, Vulkan headers, Opus and oneVPL; Rust 1.98.1 GNU; and a Microsoft x64 C++ SDK/toolchain for the NVIDIA adapter. The host uses GNU codec libraries; only the small Rust TrueHDR DLL uses the MSVC target to link NVIDIA's SDK library.

```powershell
rustup toolchain install 1.98.1-x86_64-pc-windows-gnu --profile minimal --component rustfmt --component clippy
rustup target add x86_64-pc-windows-msvc --toolchain 1.98.1-x86_64-pc-windows-gnu

.\rust\build.ps1 -FetchDependencies -Package
```

Run in a Visual Studio x64 developer PowerShell for TrueHDR. Alternatively pass `-MsvcSdk` pointing to an xwin layout with `crt/lib/x86_64`, `sdk/lib/um/x86_64` and `sdk/lib/ucrt/x86_64`. `-SkipTrueHdr` builds without the optional NVIDIA DLL. Existing SDKs can be supplied with `-FfmpegRoot`, `-PyrowaveRoot` and `-NvidiaRoot`; downloads are pinned and checked. CMake is used to build the external PyroWave SDK, not the host.

The script checks formatting, tests, lints, builds the host, service and GPU/protocol performance probes plus the optional TrueHDR DLL, then packages runtime libraries, artwork, notices and a SHA-256 manifest. A locked Cargo dependency tree and pinned SDK revisions are included. CI is `.github/workflows/rust-windows.yml`.

## Run and migration

```powershell
.\butterpollo.exe --config-dir C:\path\to\a\config-copy --bind 0.0.0.0
```

Without arguments the host uses `%LOCALAPPDATA%\ButterpolloRust\config` and listens on all IPv4 interfaces, preserving the previous host's LAN discovery behavior; the web interface is `https://localhost:47990`. `address_family=both` enables dual-stack listeners, and `bind_address` or `--bind` selects an interface. Initial credential setup requires a local connection. Use `--port 48123 --bind 127.0.0.1` for an isolated instance: web 48124, HTTPS 48118 and RTSP 48144. Standard Moonlight UDP port offsets remain compatible.

Copy the complete original configuration directory, including certificates, `sunshine_state.json`, `vibeshine_state.json`, `apps.json` and `sunshine.conf`, before testing migration. Absolute paths in the copied configuration still refer to their original locations; change those paths to the copy when isolating it. Credentials, certificate identities, app UUIDs, artwork IDs, permissions and unknown configuration/state fields are preserved. Imported legacy clients and booleans are normalized. State writes replace files atomically.

The service uses `ApolloService` for compatibility and `%PROGRAMDATA%\Butterpollo\config`. `service.ps1` manages the Rust installation and refuses to alter a service belonging to another executable. Service installation is a separate explicit action; running or building the host never installs it.

## Implementation

| Crate | Responsibility |
| --- | --- |
| `core` | NV pairing, AES/RSA, RTSP/SDP, media encryption, Cauchy FEC, input parsing, audio mixing/resampling, app identities, permissions and durable state |
| `windows` | DXGI/WGC, HDR conversion/ICC leases, AMF LTR recovery, direct NVENC/reference recovery/4:4:4, native QSV frames, software encoding, PyroWave, NGX bridge, WASAPI/Opus/routing, SendInput/touch/pen/VHF, clipboard, display topology/recovery, RTSS/NVAPI, process jobs, tray and SCM |
| `host` | TLS/HTTP, Moonlight endpoints, administration/auth, encrypted RTSP, ENet control, UDP media, scheduling and lifecycle |
| `truehdr-runtime` | Rust MSVC DLL directly calling NVIDIA's NGX C ABI |
| `vulkan-layer` | Rust implicit Vulkan layer providing HDR swapchain formats during owned HDR sessions |

There is no WebRTC, SudoVDA, ViGEm, FFmpeg AMF encoder wrapper or legacy display helper in the Rust build. AMD capture surfaces remain in D3D11 memory through GPU scaling, HDR conversion and AMF encoding. Native NVENC calls the installed NVIDIA driver directly with reviewed API 11.0–13.0 compatibility and capability-gated reference recovery. D3D11 supplies 4:2:0 and 8-bit 4:4:4; ten-bit 4:4:4 uses GPU-only CUDA interop without CPU readback. `nvenc` and `nvenc_experimental` select this native path; `nvenc_legacy` selects the FFmpeg compatibility path. Quick Sync 4:2:0 imports D3D11 frames. NVIDIA/Intel codec execution still requires hardware validation. TrueHDR shares the capture device and snapshots NGX output in GPU memory. Textures and per-frame HDR metadata remain owned until native codec references release them. All frame pools and native encoder queues are bounded. PyroWave, software and unsupported native formats use the CPU compatibility path. Shader math preserves absolute ST.2084 luminance and resizes scRGB in linear light. Reported AMF latency includes asynchronous codec completion.

Video FEC generation is measured at 1.26–1.40× the speed of the original C++ baseline on representative video blocks, with every parity byte identical and 21–29% less CPU time. The AVX2 implementation shares input loads across parity rows and preserves SSSE3/scalar fallbacks. The package includes `butterpollo-protocol-performance.exe`; [PERFORMANCE.md](PERFORMANCE.md) explains the exact reference sources and reproduction. This is a measured component improvement; whole-host C++ streaming performance remains unmeasured.

Static frames respect `minimum_fps_target`; force it to `1000` for repeat-frame throughput measurements. Windows UDP segmentation batches equal-size video packets while preserving independent datagrams and falls back to ordinary sends when unsupported. `video_max_batch_size_kb` accepts the previous 16/32/64 KiB limits; pacing also caps each burst to two milliseconds of its wire budget.

## Validation

Native Windows unit tests cover malformed wire input, authenticated encryption, replay rejection, legacy CBC/GCM behavior, FEC packet boundaries, scalar/SIMD equivalence, audio rates/channel masks, configuration/state migration, scoped API tokens and Windows job teardown. Independent `tests/interop.py` and `tests/moonlight_client.c` perform real PIN pairing, encrypted RTSP, video transport/decryption/FEC, FFmpeg decode and Opus decode. `tests/pyrowave_client.c` decodes the Rust encoder's container through the external vendor decoder.

The test machine is an AMD RX 7900 XT with an HDR display. Sustained release streams at 640×480, 30 fps passed H.264, HEVC, AV1, HEVC Main10 HDR and AV1 Main10 HDR decoding. New 20-second HEVC HDR tests sustained 120 fps host output at 1080p and scaled 4K; independent software decoding reached about 119 fps with no decode errors. HDR frames contain ten-bit BT.2020/PQ metadata. See [performance evidence and reproducible probes](PERFORMANCE.md) for timings, decoder limits and test scope. The RX 7900 XT pads 1080p AV1 to 1082 lines; the strict dimension probe rejects that result, so exact 1080p uses HEVC. PyroWave SDR and HDR container decoding also passed; that vendor decoder probe checks the HDR container flag and decoded output, not a complete ten-bit client rendering path. Unoptimized Rust builds are unsuitable for streaming performance checks.

Administration checks exercise CSRF, method-specific scopes behind Rust forms, escaped HTML, one-time token secrets, refresh/revocation, app CRUD/order, live TrueHDR overrides, covers, output redirection, exit monitoring, display layouts, baseline comparison, maintenance and support ZIP integrity. The console exposes global, application and client settings through ordinary HTML forms and preserves unknown fields. Browser checks run with JavaScript disabled at desktop and mobile sizes. Independent client checks verify permission boundaries. Native tests cover process-tree termination, minidumps, UDP datagram boundaries, texture retention, GPU colour reference points, strict loss-recovery decoding for all three AMD codecs and 21 actual Opus surround/quality/duration round trips.

NVIDIA/Intel hardware encoding, NVIDIA TrueHDR conversion, VHF controllers, the compatible VDD driver, secure-desktop input and an actual SCM-installed service require their respective hardware/privileges. Tests do not install or replace the existing service or change the user's physical display modes.

## Previous feature support

Full production parity is not yet established. Retained remote monitors, previous remote catalogue controls and confirmations, independent input sessions, permanent monitor counts, stable monitor identities, render scaling, exact fractional refresh, old golden snapshots, exclusive/isolated arrangements, HDR ICC leases, client hooks, application lifecycle policies, RTSS/NVAPI frame limiting, Vulkan interception, native crash reports and periodic release checks are implemented. Application overrides take precedence over client overrides; resolution and refresh policies remain independent. Imported app IDs continue to resolve after cover changes. Permanent counts are applied only when explicitly configured. Recovery restores only host-owned changes that the user has not subsequently altered.

The Rust implementation now also includes NVIDIA power/presentation/HAGS policy leases, opt-in WGC publication alignment, TrueHDR visible-window and asynchronous driver-profile selection, native HDR bypass, MHC2 calibration luminance, permanent-only IPv4 UPnP leases and IGDv2 IPv6 pinholes. Remembered browser sessions migrate from the previous state format, survive restart, rotate refresh tokens and preserve revocation. The Rust console reuses all 22 previous locale catalogues, with English fallback for new messages. Virtual displays carry client/application labels and luminance, renew their owner leases and recreate lost owned monitors; display policies and capture reconnect after recreation. Moonlight receives the MAC address of its local network interface for wake-on-LAN.

See [the baseline feature inventory and validation matrix](PARITY.md). Platform code is implemented, but full production parity is not certified: display activation/DPI/recreation needs a privileged host and the compatible VDD driver; NVIDIA/Intel/TrueHDR/VHF operations need their respective hardware. The legacy `src`, CMake and installer sources remain as migration references; this Rust build does not compile them.

Useful probes: `--diagnostics`, `--capture-smoke --hdr`, `--encoder-smoke amf --codec hevc --hdr` and `--encoder-smoke pyrowave --codec pyrowave --encoder-output frame.bin`. Display recovery journals only changes owned by the host and restores them after parent-process death, provided the user has not subsequently changed that setting.
