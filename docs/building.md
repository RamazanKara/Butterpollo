# Build Rubylight

[Docs](README.md) · [Getting started](getting-started.md) · [Configuration](configuration.md)

The current Windows host, service, launcher and installer are built with Rust. Use [rust/build.ps1](../rust/build.ps1) for the complete build and package; [.github/workflows/rust-windows.yml](../.github/workflows/rust-windows.yml) records the CI environment. The [Rust developer guide](../rust/README.md) describes the workspace and implementation.

To install a release and start streaming, use [Getting started](getting-started.md). The [archived C++ build guide](https://github.com/RamazanKara/Rubylight/blob/2.0.0-rc.23/docs/legacy/building-cpp.md) covers the earlier CMake host, whose sources live at the 2.0.0-rc.23 tag.

## Windows prerequisites

Use Windows x64 with PowerShell, Git, Rustup and Node.js 22. In an MSYS2 UCRT64 shell, install these packages, matching CI:

```sh
pacman -S --needed git mingw-w64-ucrt-x86_64-gcc mingw-w64-ucrt-x86_64-clang mingw-w64-ucrt-x86_64-cmake mingw-w64-ucrt-x86_64-ninja mingw-w64-ucrt-x86_64-vulkan-headers mingw-w64-ucrt-x86_64-opus mingw-w64-ucrt-x86_64-libvpl
```

The default MSYS2 path is `C:\msys64`; pass `-MsysRoot` if yours differs. The host uses the GNU Rust target and GNU codec libraries. The optional TrueHDR DLL uses the MSVC target and needs an x64 Microsoft C++ toolchain and Windows SDK.

Clone the repository and install the pinned toolchains in PowerShell:

```powershell
git clone https://github.com/RamazanKara/Rubylight.git
cd Rubylight

rustup toolchain install 1.98.1-x86_64-pc-windows-gnu --profile minimal --component rustfmt --component clippy
rustup target add x86_64-pc-windows-msvc --toolchain 1.98.1-x86_64-pc-windows-gnu
```

The pinned version is defined by [rust-toolchain.toml](../rust-toolchain.toml) and the build script; `Cargo.lock` pins Rust dependencies.

## Build and package

Run the full build from the repository root in a **Visual Studio x64 developer PowerShell** so the TrueHDR adapter can find the Microsoft libraries:

```powershell
.\rust\build.ps1 -FetchDependencies -Package
```

The script fetches the pinned FFmpeg and NVIDIA SDK archives, builds the pinned PyroWave SDK and extracts the signed display/gamepad packages and the Playnite plugin from the pinned Vibepollo release. Archive hashes, PyroWave build identity and driver signatures are checked. CMake builds the external PyroWave SDK; Cargo builds the host.

The build checks Rust formatting, runs ordinary workspace tests and Clippy with warnings denied, then builds the release executables, performance probes and TrueHDR adapter. Packaging also builds the Svelte console and includes runtime libraries, drivers, the Playnite plugin, artwork, licenses, the optional Moonlight source patch and a SHA-256 file manifest. Building a package does not install it.

Default outputs are under `%LOCALAPPDATA%\ButterpolloRust\target`:

| Output | Contents |
| --- | --- |
| `release\` | Compiled executables, runtime DLLs and examples. |
| `butterpollo-rust-release\` | Portable package with console assets and dependencies. |
| `butterpollo-rust-release.zip` | ZIP of the portable package. |
| `butterpollo-setup-<version>.exe` | Installer carrying that package; version comes from the workspace manifest. |

SDK downloads use `%LOCALAPPDATA%\ButterpolloRust\sdk`. Choose a separate build location with `-TargetDirectory` and a dependency cache with `-Dependencies`.

Useful options:

| Option | Purpose |
| --- | --- |
| `-DebugBuild` | Build debug executables for development. Use release builds for streaming measurements. |
| `-SkipTrueHdr` | Omit the optional NVIDIA TrueHDR adapter and its MSVC SDK requirement. |
| `-MsvcSdk <path>` | Use an xwin SDK layout containing `crt/lib/x86_64`, `sdk/lib/um/x86_64` and `sdk/lib/ucrt/x86_64`. |
| `-FfmpegRoot`, `-PyrowaveRoot`, `-NvidiaRoot` | Reuse existing SDKs. The PyroWave identity must match the pinned revision and patches. |
| `-DriverRoot <path>` | Supply the signed release's `drivers` folder for packaging. |
| `-SkipTests` | Skip workspace tests and Clippy for a local iteration. Formatting still runs; this is not the release validation path. |

For a build without TrueHDR:

```powershell
.\rust\build.ps1 -FetchDependencies -SkipTrueHdr -Package
```

## SDK environment

Cargo alone cannot build the Windows crates: their build scripts generate bindings from pinned SDK headers and link pinned codec libraries. The build script sets these variables for the session it runs in; set them yourself to run `cargo` in another shell.

| Variable | Read by | Points at |
| --- | --- | --- |
| `BUTTERPOLLO_FFMPEG_ROOT` | `rust/windows/build.rs` | The pinned FFmpeg SDK (`include/` with the AMF headers, and `lib/`). |
| `BUTTERPOLLO_PYROWAVE_ROOT` | `rust/windows/build.rs` | The pinned PyroWave SDK built by `rust/tools/build_pyrowave.sh`. |
| `BUTTERPOLLO_VULKAN_INCLUDE` | `rust/windows/build.rs` | Vulkan 1.4 headers, `C:\msys64\ucrt64\include` in MSYS2. |
| `BUTTERPOLLO_SYSTEM_LIBS` | `rust/windows/build.rs` | MSYS2's `ucrt64\lib`, for libvpl and the C++ runtime. |
| `LIBCLANG_PATH` | bindgen | MSYS2's `ucrt64\bin`, which holds `libclang.dll`. |
| `NV_RTX_VIDEO_SDK` | `rust/truehdr-runtime/build.rs` | NVIDIA's RTX Video SDK, only for the optional TrueHDR adapter. |
| `BUTTERPOLLO_MSVC_ROOT`, `BUTTERPOLLO_DRIVER_ROOT` | `rust/build.ps1` | Defaults for `-MsvcSdk` and `-DriverRoot`. |

`butterpollo-core` needs none of them and builds and tests on any OS with plain `cargo test -p butterpollo-core`.

## Development checks

After the build script has configured the SDK paths in the same PowerShell session, the ordinary Rust checks are:

```powershell
cargo +1.98.1-x86_64-pc-windows-gnu fmt --all -- --check
cargo +1.98.1-x86_64-pc-windows-gnu test --workspace --locked
cargo +1.98.1-x86_64-pc-windows-gnu clippy --workspace --all-targets --locked -- -D warnings
```

The TrueHDR crate is outside the main workspace; the build script checks and builds it separately for `x86_64-pc-windows-msvc`.

Hardware and environment-dependent tests are selected separately. Their fixtures can exercise capture, controllers or displays; choose a fixture for the intended machine and preserve its restoration checks. [PERFORMANCE.md](../rust/PERFORMANCE.md) records measured workloads, and [PARITY.md](../rust/PARITY.md) distinguishes implementation from hardware validation. Ordinary test success alone is not a GPU compatibility result.

## Web console

The current interface is the Svelte app in [rust/web](../rust/web). Its commands, starting from the repository root, are:

```powershell
cd rust/web
npm ci --no-audit --no-fund
npm run check
npm run build
npm run dev
```

CI runs `npm run check`; packaging runs the production build and copies `dist` into `assets/web`. The package script uses a separate web build folder so a WSL checkout's Linux `node_modules` cannot be reused as Windows dependencies.

The development server proxies `/api` to `https://localhost:47990` by default. Set `BUTTERPOLLO_HOST` before starting it to point at an isolated development profile. Requests go to that real host, including actions taken in the console. See [vite.config.ts](../rust/web/vite.config.ts) for the proxy configuration.

## Website

[site/](../site) builds the project website at https://ramazankara.github.io/Rubylight/: a landing page plus these guides, rendered from the repository's Markdown. [.github/workflows/pages.yml](../.github/workflows/pages.yml) publishes it whenever `site/`, `docs/`, `README.md` or the Markdown in `rust/` changes on `main`. Until **Settings → Pages → Source** is set to GitHub Actions, it builds and checks the site but skips the deployment. To preview it, from the repository root:

```sh
python3 -m pip install -r site/requirements.txt
python3 site/build.py
```

Open `_site/index.html`. The build fails on a link to a file that doesn't exist. The landing page's measurements are written into [site/index.html](../site/index.html); when a newer measurement replaces one, update it there too.
