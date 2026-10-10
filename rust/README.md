# Rubylight developer guide

[Documentation](../docs/README.md) · [Build guide](../docs/building.md) · [Architecture](../docs/architecture.md) · [API](../docs/api.md) · [Release notes](RELEASE_NOTES.md)

The Windows host, protocol implementation, native helpers, service and setup are written in Rust. The browser console is Svelte. The Rust executables do not link the previous C++ host; codec libraries, GPU SDKs and Windows drivers are external dependencies.

The workspace version is **2.0.0**. This guide covers development and isolated validation. For feature status and exact hardware evidence, use [PARITY.md](PARITY.md) and [PERFORMANCE.md](PERFORMANCE.md).

## Start streaming

Use [Getting started](../docs/getting-started.md) for installation, migration, pairing and client selection. [Configuration](../docs/configuration.md) covers capture, HDR, virtual displays, RTSS and updates; [Troubleshooting](../docs/troubleshooting.md) covers symptoms and support reports.

## Build

The Cargo workspace is at the repository root. Build from a Windows x64 checkout with the pinned toolchain in [rust-toolchain.toml](../rust-toolchain.toml) and the SDK environment described in the [build guide](../docs/building.md).

```powershell
rustup toolchain install 1.98.1-x86_64-pc-windows-gnu --profile minimal --component rustfmt --component clippy
rustup target add x86_64-pc-windows-msvc --toolchain 1.98.1-x86_64-pc-windows-gnu

.\rust\build.ps1 -FetchDependencies -Package
```

Run in a Visual Studio x64 developer PowerShell for TrueHDR, or pass `-MsvcSdk` for a compatible xwin SDK layout. `-SkipTrueHdr` omits that optional NVIDIA DLL. Existing dependency roots can be supplied with `-FfmpegRoot`, `-PyrowaveRoot` and `-NvidiaRoot`.

The [build script](build.ps1) checks formatting, tests and lints, then builds and packages the runtime, helpers, performance probes, console, notices and SHA-256 manifest. PyroWave uses a pinned SDK identity and matching patches. CMake builds the external PyroWave SDK; Cargo builds the host. [rust-windows.yml](../.github/workflows/rust-windows.yml) defines the Windows CI path.

Use release builds for stream-performance work. The [build guide](../docs/building.md) documents dependencies, output paths and packaging options.

## Run and migration

Run a packaged host against an explicitly selected development profile:

```powershell
.\butterpollo.exe --config-dir C:\tests\butterpollo-profile --port 48123 --bind 127.0.0.1
```

This isolated base port uses web 48124, HTTPS 48118 and RTSP 48144. Standard Moonlight UDP offsets remain compatible. Initial administrator setup requires a local connection.

| Mode | Default profile | Service |
| --- | --- | --- |
| Portable / direct host | `%LOCALAPPDATA%\ButterpolloRust\config` | Optional |
| Installed package | `%PROGRAMDATA%\Butterpollo\config` | `ApolloService` for migration compatibility |

Without arguments, the host listens on all IPv4 interfaces with the console at `https://localhost:47990`. `address_family=both` enables dual-stack listeners; `bind_address` or `--bind` selects an interface. Running or building the host does not install a service. [service.ps1](service.ps1) manages a portable service installation and refuses to alter one belonging to another executable.

Import into an empty destination without starting a host:

```powershell
.\butterpollo.exe --config-dir C:\tests\new-profile --import-config C:\path\to\old\config
```

Import copies identity, settings, state, app IDs, permissions, certificates and covers into owned paths. Unknown fields survive. Game paths and preparation commands keep their existing meaning. Missing configured identity files, links/junctions or excessive profile sizes abort the import before the new profile is committed. The launcher refuses to overwrite a nonempty profile. State writes are atomic.

Keep development profiles and ports distinct from the installed service. Tests under `rust/tests` use isolated profiles and stop their own processes.

## Implementation

| Workspace member | Responsibility |
| --- | --- |
| [core](core) | Moonlight protocol, crypto, FEC, input parsing, audio mixing, permissions and state. |
| [windows](windows) | Capture, GPU conversion, native codecs, audio, input, displays, limiter integration, tray and SCM. |
| [host](host) | HTTP/TLS, administration, RTSP, ENet control, UDP transport and stream lifecycle. |
| [setup](setup) | Installation, upgrade and recovery. |
| [vulkan-layer](vulkan-layer) | Implicit Vulkan HDR layer for owned sessions. |
| [truehdr-runtime](truehdr-runtime) | Separately built Rust MSVC DLL calling NVIDIA's NGX C ABI. |

The [Svelte console](web) is packaged with the host; server-rendered pages remain a fallback. The [architecture guide](../docs/architecture.md) explains compute queues, producer/output fences, service-mode WGC, HDR and PyroWave.

Useful implementation entry points:

- [compute.rs](windows/src/compute.rs): Radeon copies, RGB-to-YUV conversion and D3D12 handoff to AMF.
- [Windows source](windows/src): capture, encoders, virtual displays and native integration.
- [Host source](host/src): session scheduling, configuration, administration and media transport.
- [Core source](core/src): protocol and durable-state primitives.

PyroWave shares D3D11/Vulkan planar textures and returns only the encoded bitstream to the CPU. Native NVENC uses the installed driver, with D3D11 4:2:0/8-bit 4:4:4 and CUDA interop for ten-bit 4:4:4; `nvenc_legacy` selects the FFmpeg compatibility path. Quick Sync imports D3D11 frames. Hardware execution evidence is tracked per path in [PARITY.md](PARITY.md).

## Validation

The ordinary checks (formatting, Clippy with warnings denied, workspace tests) and the SDK environment they need are listed in the [contributor guide](../.github/CONTRIBUTING.md#building-and-checking). CI runs them through [build.ps1](build.ps1) on every push to main and on pull requests that change the Rust tree. The layers beyond them:

| Validation layer | Entry points and scope |
| --- | --- |
| Ordinary tests | Wire parsing, encryption/replay, FEC, input, state, migration, ownership and recovery policy. |
| Administration and browser | [web_api.py](tests/web_api.py), [console_browser.cjs](tests/console_browser.cjs), [session_restart.py](tests/session_restart.py), [otp_pairing.py](tests/otp_pairing.py). |
| Independent standard-codec streams | [interop.py](tests/interop.py), [moonlight_client.c](tests/moonlight_client.c): real pairing, encrypted RTSP, transport/FEC, FFmpeg and Opus decode. |
| Independent PyroWave streams | [build-pyrowave-client.ps1](tests/build-pyrowave-client.ps1), [moonlight_client.c](tests/moonlight_client.c), [pyrowave_transport.py](tests/pyrowave_transport.py). |
| Native GPU, driver and display tests | Explicitly selected ignored tests; requirements and commands in [PARITY.md](PARITY.md#reproducible-verification). |
| Performance and pixel accuracy | Release probes and fixtures indexed in [PERFORMANCE.md](PERFORMANCE.md#reproduce-on-another-machine). |

Native fixtures may require an active moving desktop, codec DLLs, compatible drivers and particular hardware. Display-changing fixtures record restoration separately. Follow each fixture's prerequisites and run them in a suitable idle test session.

The strict AMD AV1 raw-bitstream geometry test retains its known failure at some unaligned sizes. Moonlight PC 6.2.0's crop handling has separate recorded client checks. Likewise, complete frame decoding, native HDR reference pixels and displayed TV appearance are distinct validation layers. [The compatibility matrix](PARITY.md#evidence) preserves those distinctions.

## Previous feature support

The Rust host imports the Vibepollo/Apollo profile format and implements the Windows feature set recorded in [PARITY.md](PARITY.md). Application overrides take precedence over client overrides; display recovery restores host-owned changes that the user has not subsequently altered.

The earlier C++ host, its CMake build and installer were removed after 2.0.0-rc.23; [that tag](https://github.com/RamazanKara/Rubylight/tree/2.0.0-rc.23) and its [archived references](https://github.com/RamazanKara/Rubylight/blob/2.0.0-rc.23/docs/legacy/README.md) keep them for migration research. The released Rust package is built through [build.ps1](build.ps1).

An optional [Moonlight 6.2.0 source patch](compatibility/moonlight-6.2.0/README.md) addresses cold-cache CSV artwork shutdown. Its focused Qt validation is recorded separately from the official Windows client's streaming checks.

## Updates

[Configuration](../docs/configuration.md) describes the notify-first controls and idle-install policy. The updater requires the installed service, verifies the official GitHub installer's advertised size and SHA-256 digest, then waits for one minute without active/pending streams, remote monitors or host apps.

Setup backs up replaced package files, verifies that the requested host version starts and restores those files on copying or startup failure. A failed version is not retried automatically; the console offers a manual retry. Recovery records stay under the service profile's `updates` directory.

The installer is unsigned. Its digest check over HTTPS is an integrity check against the official release, distinct from an Authenticode signature. Portable hosts use release-page downloads. [Release notes](RELEASE_NOTES.md) and the release's validation record describe package provenance.
