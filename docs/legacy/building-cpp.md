> **Historical C++ reference.** This page preserves the earlier Sunshine-derived host documentation. For the current Butterpollo Rust host, use the [current build guide](../building.md).

# Building Butterpollo

Butterpollo builds for Windows with CMake, Ninja and MSYS2 UCRT64. The supported installer pipeline is [.github/workflows/butterpollo-windows.yml](../../.github/workflows/butterpollo-windows.yml), which calls [ci-windows.yml](../../.github/workflows/ci-windows.yml). That workflow is the source of truth for pinned dependencies and packaging flags.

## Dependencies

Install the UCRT64 compiler, CMake, Ninja, Boost, C++/WinRT, curl-winssl, MinHook, miniupnpc, nlohmann-json, oneVPL, OpenSSL, Opus, Python and Vulkan headers listed in the CI workflow. Use Node.js 22 and npm for the browser interface. The installer additionally requires WiX and .NET.

```sh
git clone --recurse-submodules https://github.com/RamazanKara/Butterpollo.git
cd Butterpollo
```

CMake downloads the pinned prebuilt FFmpeg library; it is retained for Intel QuickSync and software encoding. AMD uses native AMF and NVIDIA uses native NVENC. CUDA interop for NVIDIA 4:4:4 remains part of the Windows encoder.

The Windows packaging configuration validates the virtual-display, VHF gamepad and TrueHDR packages. Follow CI's download steps and pass the verified package directories and contract pins when configuring. The dependency download scripts are in [scripts](../../scripts). Keep TrueHDR enabled: it remains a supported feature.

## Compile and test

Run in an MSYS2 UCRT64 shell, with Node.js on PATH and the verified packaging inputs from CI:

```sh
cmake -S . -B build -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_TESTS=ON \
  -DBUILD_WERROR=ON
cmake --build build --parallel
ctest --test-dir build --output-on-failure --timeout 120
```

The packaging inputs omitted from this short command are required; copy their exact flags from CI's **Build Windows** step. CI also runs workflow, installer, driver and native-AMF contract checks. Tests must pass without exclusions.

The `web_ui` target installs locked npm dependencies with lifecycle scripts disabled, generates design tokens, type-checks Vue and builds the only interface into `build/assets/web`. The host and installer depend on it. The production UI is served at `/`; old `/v2` URLs redirect to it.

For frontend-only work:

```sh
cd src_assets/common/assets/web
npm ci --ignore-scripts
npm run dev
npm run build
npm run test:unit
npm run format:check
```

## PyroWave

Enable `SUNSHINE_ENABLE_PYROWAVE=ON` and point `SUNSHINE_PYROWAVE_ROOT` to the installed pinned PyroWave C API library. [scripts/build_pyrowave.sh](../../scripts/build_pyrowave.sh) builds the revision used in CI, including Granite and volk. Packaging ships the shared DLL beside the host and includes their MIT licenses.

With tests enabled, `pyrowave_selftest` exercises the production conversion, encoder, framing and decoder on a Vulkan-capable GPU. It is a manual hardware test rather than a CTest dependency. A passing self-test does not establish compatibility with a live Moonlight client.

## Installer

CI produces an unsigned `VibepolloSetup.exe` and MSI with release provenance. The executable, service, install directory, registry keys and MSI upgrade identity keep their existing names so upgrades preserve paired devices and configuration. User-visible project and support links point to Butterpollo.
