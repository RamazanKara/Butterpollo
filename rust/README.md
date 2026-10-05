# Butterpollo Rust

The Windows host, protocol implementation, native helpers, service and setup are written in Rust. The executables do not link the previous Butterpollo C++ host. The console is a Svelte app (`rust/web`) built into the package; the host serves it and keeps server-rendered pages as a fallback. Codec libraries, device drivers and GPU SDKs remain external dependencies.

**Windows test candidate 2.0.0-rc.8.** This Rust replacement ports the Windows streaming changes in Vibepollo 2.0.0 (`8a8c4b03a280ab9f567beb380110abb80f5220b8`) onto the previous Butterpollo baseline. See [what changed for players](RELEASE_NOTES.md), [feature evidence](PARITY.md) and [Rust performance measurements](PERFORMANCE.md). The retained C++ measurements in the parent README are separate.

## Start streaming

1. Run `butterpollo-setup-<version>.exe` from the release. It installs the host, service and drivers, or upgrades a Vibepollo or Butterpollo installation in place with its settings and paired devices. For a portable copy instead, extract the Windows x64 ZIP into a folder and open **Start Butterpollo.exe**.
2. On the first portable launch, choose whether to import your Vibepollo/Apollo profile. Select its folder containing `sunshine.conf`, or the installation folder containing `config/sunshine.conf`. Import copies settings, paired devices, identity, library and covers into a new Rust profile. The original profile is retained. Choose **No** for a fresh setup.
3. The launcher opens the console at `https://localhost:47990` (or your configured port). Create the local administrator account if prompted, then use the connection checklist to check video, display and sound.
4. Open Moonlight on another device on the same network. Add this PC if discovery does not find it. Enter **the PIN shown by Moonlight** in **Devices**, then launch **Desktop**. Start with 1080p/60 and choose your preferred resolution, rate and HDR after the first successful stream.

Opening the launcher again returns to the running profile's console. It also opens the profile of the Rust service installed from the same package. A stopped service or a port occupied by another host produces an actionable error. Logs are in the profile's `logs/butterpollo.log` unless configured otherwise. Portable mode needs no service installation; virtual-display and controller features then need their Windows drivers installed separately.

Standard Moonlight supports H.264, HEVC and AV1. PyroWave and VRR require [Nonary's Moonlight client](https://github.com/Nonary/moonlight-qt). Use PyroWave on a fast wired LAN with hundreds of Mbps available; its client bandwidth calibration estimates capacity before connecting. VRR uses 1000 Hz virtual-display capture while preserving the requested stream frame rate. Present-timing tracking falls back to WGC timestamps when Windows does not allow it.

The Overview page shows recent frame rate, bitrate, encode p95 and performance history. Optional live refresh updates every five seconds and can be paused. Older pending PyroWave frames are replaced when a connection is slow; the page explains when reducing bitrate would help.

## Build

Requirements: Windows x64, MSYS2 UCRT64 with GCC, Clang, CMake, Ninja, Vulkan headers, Opus and oneVPL; Rust 1.98.1 GNU; Node.js 22 for the console; and a Microsoft x64 C++ SDK/toolchain for the NVIDIA adapter. The host uses GNU codec libraries; only the small Rust TrueHDR DLL uses the MSVC target to link NVIDIA's SDK library.

```powershell
rustup toolchain install 1.98.1-x86_64-pc-windows-gnu --profile minimal --component rustfmt --component clippy
rustup target add x86_64-pc-windows-msvc --toolchain 1.98.1-x86_64-pc-windows-gnu

.\rust\build.ps1 -FetchDependencies -Package
```

Run in a Visual Studio x64 developer PowerShell for TrueHDR. Alternatively pass `-MsvcSdk` pointing to an xwin layout with `crt/lib/x86_64`, `sdk/lib/um/x86_64` and `sdk/lib/ucrt/x86_64`. `-SkipTrueHdr` builds without the optional NVIDIA DLL. Existing SDKs can be supplied with `-FfmpegRoot`, `-PyrowaveRoot` and `-NvidiaRoot`; downloads are pinned and checked. CMake is used to build the external PyroWave SDK, not the host.

The script checks formatting, tests, lints, builds the host, launcher, service and GPU/protocol performance probes plus the optional TrueHDR DLL, then packages runtime libraries, artwork, notices and a SHA-256 manifest. A locked Cargo dependency tree and pinned SDK revisions are included. PyroWave is pinned to bitstream `186f0393` with Vibepollo 2.0's three codec patches; the build rejects SDKs missing the matching identity. CI is `.github/workflows/rust-windows.yml`.

## Run and migration

```powershell
.\butterpollo.exe --config-dir C:\path\to\a\config-copy --bind 0.0.0.0
```

Without arguments the host uses `%LOCALAPPDATA%\ButterpolloRust\config` and listens on all IPv4 interfaces, preserving the previous host's LAN discovery behavior; the web interface is `https://localhost:47990`. `address_family=both` enables dual-stack listeners, and `bind_address` or `--bind` selects an interface. Initial credential setup requires a local connection. Use `--port 48123 --bind 127.0.0.1` for an isolated instance: web 48124, HTTPS 48118 and RTSP 48144. Standard Moonlight UDP port offsets remain compatible.

The launcher imports into `%LOCALAPPDATA%\ButterpolloRust\config`, and refuses to overwrite a nonempty profile. A command-line import into an empty destination is also available; it validates and copies the profile, then exits without starting the host:

```powershell
.\butterpollo.exe --config-dir C:\path\to\a\new-profile --import-config C:\path\to\old\config
```

Configured identity/state/library files are copied into owned paths, and existing PNG covers are copied by content. Credentials, certificates, app UUIDs, permissions and unknown fields are retained. Game paths and preparation commands keep their existing meaning. Missing configured identity files, links/junctions or excessive profile sizes abort the import without committing the new profile. Imported legacy clients and booleans are normalized on load. State writes replace files atomically.

The service uses `ApolloService` for compatibility and `%PROGRAMDATA%\Butterpollo\config`. The setup installs it; `service.ps1` manages a portable installation and refuses to alter a service belonging to another executable. Running or building the host never installs the service.

When the service selects WGC, it starts a hidden capture worker as the signed-in user: Windows cannot open the per-user WGC broker directly as SYSTEM (`0x80070424`). Three shared GPU textures carry frames to the host through keyed synchronization; a local pipe carries bounded metadata and checks both process identities. The worker receives capture settings only, and its owned job closes with the capture session. Lock/UAC desktops use Desktop Duplication, with WGC retried when the normal desktop returns. Other WGC startup failures retain the logged DDX fallback. `wgc_user_helper=true` forces this worker path in a portable host for validation; it does not change automatic backend selection. Check the `capture backend opened` log for the actual backend, separately from `requested_capture` in the stream settings.

## Implementation

| Crate | Responsibility |
| --- | --- |
| `core` | NV pairing, AES/RSA, RTSP/SDP, media encryption, Cauchy FEC, input parsing, audio mixing/resampling, app identities, permissions and durable state |
| `windows` | DXGI/WGC, HDR conversion/ICC leases, AMF LTR recovery, direct NVENC/reference recovery/4:4:4, native QSV frames, software encoding, PyroWave, NGX bridge, WASAPI/Opus/routing, SendInput/touch/pen/VHF, clipboard, display topology/recovery, RTSS/NVAPI, process jobs, tray and SCM |
| `host` | TLS/HTTP, Moonlight endpoints, administration/auth, encrypted RTSP, ENet control, UDP media, scheduling and lifecycle |
| `truehdr-runtime` | Rust MSVC DLL directly calling NVIDIA's NGX C ABI |
| `vulkan-layer` | Rust implicit Vulkan layer providing HDR swapchain formats during owned HDR sessions |

On supported AMD GPUs, Desktop Duplication and WGC frames are copied into shared textures and converted on D3D12 compute queues, and AMF encodes from D3D12. This reduces waiting behind a game's graphics work. Unshareable capture textures fall back to D3D11. The compute toggle in Settings > Capture applies to either backend; `wgc_compute_copy=false` independently disables WGC compute copies. Native NVENC calls the installed NVIDIA driver directly with reviewed API 11.0–13.0 compatibility and capability-gated reference recovery. D3D11 supplies 4:2:0 and 8-bit 4:4:4; ten-bit 4:4:4 uses GPU-only CUDA interop without CPU readback. `nvenc` and `nvenc_experimental` select this native path; `nvenc_legacy` selects the FFmpeg compatibility path. Quick Sync 4:2:0 imports D3D11 frames. NVIDIA/Intel codec execution still requires hardware validation. TrueHDR shares the capture device and snapshots NGX output in GPU memory. PyroWave shares the capture D3D11 device, converts to planar textures on the GPU and synchronizes Vulkan imports through a shared fence. Only its encoded bitstream returns to the CPU. Textures and per-frame HDR metadata remain owned until native codec references release them. Frame pools, native encoder queues and per-client PyroWave pending queues are bounded. Software and unsupported native formats retain the compatibility path. Shader math preserves absolute ST.2084 luminance and resizes scRGB in linear light. Reported AMF latency includes asynchronous codec completion. WebRTC, SudoVDA, ViGEm and the legacy display helper remain outside the Butterpollo baseline's scope.

Video FEC generation is measured at 1.26–1.40× the speed of the original C++ baseline on representative video blocks, with every parity byte identical and 21–29% less CPU time. The AVX2 implementation shares input loads across parity rows and preserves SSSE3/scalar fallbacks. The package includes `butterpollo-protocol-performance.exe`; [PERFORMANCE.md](PERFORMANCE.md) explains the exact reference sources and reproduction. This is a measured component improvement; whole-host C++ streaming performance remains unmeasured.

Static frames respect `minimum_fps_target`; force it to `1000` for repeat-frame throughput measurements. Windows UDP segmentation batches equal-size video packets while preserving independent datagrams and falls back to ordinary sends when unsupported. `video_max_batch_size_kb` accepts the previous 16/32/64 KiB limits; pacing also caps each burst to two milliseconds of its wire budget.

## Validation

Native Windows unit tests cover malformed wire input, authenticated encryption, replay rejection, legacy CBC/GCM behavior, FEC packet boundaries and sequence wraps, scalar/SIMD equivalence, audio rates/channel masks, atomic profile migration, scoped API tokens and Windows job teardown. Independent `tests/interop.py` and `tests/moonlight_client.c` perform real PIN pairing, encrypted RTSP, video transport/decryption/FEC, FFmpeg decode and Opus decode. The pinned Nonary transport in `tests/build-pyrowave-client.ps1` and `tests/pyrowave_stream_client.c` verifies continuous SDR/HDR 4:2:0/4:4:4 PyroWave streams, HDR control flags and the authenticated bandwidth probe. `tests/pyrowave_client.c` and `tests/pyrowave_transport.py` independently check twelve vendor-decoded profiles, 24 encrypted/plain FEC cases and deliberate partial loss.

The test machine is an AMD RX 7900 XT with an HDR display. Sustained release streams at 640×480, 30 fps passed H.264, HEVC, AV1, HEVC Main10 HDR and AV1 Main10 HDR decoding. New 20-second HEVC HDR tests sustained 120 fps host output at 1080p and scaled 4K; independent software decoding reached about 119 fps with no decode errors. HDR frames contain ten-bit BT.2020/PQ metadata. See [performance evidence and reproducible probes](PERFORMANCE.md) for timings, decoder limits and test scope. The RX 7900 XT pads 1080p AV1 to 1082 lines; the strict dimension probe rejects that result, so exact 1080p uses HEVC. PyroWave SDR and HDR container decoding also passed; that vendor decoder probe checks the HDR container flag and decoded output, not a complete ten-bit client rendering path. Unoptimized Rust builds are unsuitable for streaming performance checks.

Administration checks exercise CSRF, method-specific scopes behind Rust forms, escaped HTML, one-time token secrets, refresh/revocation, app CRUD/order, live TrueHDR overrides, covers, output redirection, exit monitoring, display layouts, baseline comparison, maintenance and support ZIP integrity. The console exposes global, application and client settings through ordinary HTML forms and preserves unknown fields. Browser checks run with JavaScript disabled at desktop and mobile sizes. Independent client checks verify permission boundaries. Native tests cover process-tree termination, minidumps, UDP datagram boundaries, texture retention, GPU colour reference points, strict loss-recovery decoding for all three AMD codecs and 21 actual Opus surround/quality/duration round trips.

NVIDIA/Intel hardware encoding, NVIDIA TrueHDR conversion, VHF controllers, the compatible VDD driver, secure-desktop input and an actual SCM-installed service require their respective hardware/privileges. Tests do not install or replace the existing service or change the user's physical display modes.

## Previous feature support

Full production parity is not yet established. Retained remote monitors, previous remote catalogue controls and confirmations, independent input sessions, permanent monitor counts, stable monitor identities, render scaling, exact fractional refresh, old golden snapshots, exclusive/isolated arrangements, HDR ICC leases, client hooks, application lifecycle policies, RTSS/NVAPI frame limiting, Vulkan interception, native crash reports and periodic release checks are implemented. Application overrides take precedence over client overrides; resolution and refresh policies remain independent. Imported app IDs continue to resolve after cover changes. Permanent counts are applied only when explicitly configured. Recovery restores only host-owned changes that the user has not subsequently altered.

The Rust implementation now also includes NVIDIA power/presentation/HAGS policy leases, opt-in WGC publication alignment, TrueHDR visible-window and asynchronous driver-profile selection, native HDR bypass, MHC2 calibration luminance, permanent-only IPv4 UPnP leases and IGDv2 IPv6 pinholes. Remembered browser sessions migrate from the previous state format, survive restart, rotate refresh tokens and preserve revocation. The Rust console reuses all 22 previous locale catalogues, with English fallback for new messages. Virtual displays carry client/application labels and luminance, renew their owner leases and recreate lost owned monitors; display policies and capture reconnect after recreation. Moonlight receives the MAC address of its local network interface for wake-on-LAN.

See [the baseline feature inventory and validation matrix](PARITY.md). Platform code is implemented, but full production parity is not certified: display activation/DPI/recreation needs a privileged host and the compatible VDD driver; NVIDIA/Intel/TrueHDR/VHF operations need their respective hardware. The legacy `src`, CMake and installer sources remain as migration references; this Rust build does not compile them.

Useful probes: `--diagnostics`, `--capture-smoke --hdr`, `--encoder-smoke amf --codec hevc --hdr` and `--encoder-smoke pyrowave --codec pyrowave --encoder-output frame.bin`. Display recovery journals only changes owned by the host and restores them after parent-process death, provided the user has not subsequently changed that setting.

## Updates

Maintenance → Updates checks the official Butterpollo releases and offers **Install when idle**. Updates notify first; **Settings → General → Install updates automatically** is off by default. The existing **Include pre-releases** setting controls whether release candidates are offered. Setting the check interval to zero disables automatic checks and installation; manual checks and installation still work.

Installation requires the normal Windows service. Butterpollo verifies the installer against GitHub's SHA-256 digest and advertised size, then waits until streams, pending connections, remote monitors and host apps have stopped for one minute. A disconnected Desktop session can retain an app: quit it from the client or console to let the update proceed. A new connection defers an in-progress download until the host is idle again. A queued download can be cancelled before installation begins. The console reconnects after the restart.

Updates preserve settings, paired devices, apps and existing drivers. Setup backs up replaced package files, verifies that the requested host version starts, and restores those files if copying or startup fails. A failed version is not retried automatically; Maintenance shows the result and offers a manual retry. Backups of a failed installation stay under the service profile's `updates` folder. This recovery covers ordinary installation/startup errors, not every possible power loss or configuration migration failure.

The installers are still unsigned. The updater's integrity check uses the digest returned over HTTPS by the fixed official GitHub repository; it is not an Authenticode signature. Portable hosts continue to use the release-page download.
