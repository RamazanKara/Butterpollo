# Third-party code

Butterpollo Rust is GPL-3.0-only. Its administration console is rendered by the Rust host. Rust package names, versions, license declarations and repositories are recorded in the packaged `rust-dependencies.json`; exact versions and registry checksums are in `Cargo.lock`. Source is available in the corresponding repository checkout.

External SDKs are not a port of the original host:

- FFmpeg SDK: LizardByte/build-deps `v2026.516.30821`, Windows AMD64 archive SHA-256 `2f7a2c2fc6be9b96de3c6f654389f73a5e5d369d7e802d017894fae96247661d`. Static encoding libraries include FFmpeg/libavcodec/libavutil/libswscale, x264, x265, SVT-AV1 and HDR10+ support. FFmpeg and codec source/build recipes: https://github.com/LizardByte/build-deps/tree/v2026.516.30821. GPL source and distribution requirements apply to the combined executable.
- AMF headers are provided by the FFmpeg SDK; AMF runtime is loaded from the installed AMD graphics driver. SDK: https://github.com/GPUOpen-LibrariesAndSDKs/AMF.
- PyroWave `89f7e47d4abbf650c91fae766728af866c5e32a0`, API 0.6.0. `scripts/build_pyrowave.sh` builds upstream with its pinned Granite/volk dependencies and installs their MIT notices. Source: https://github.com/Themaister/pyrowave.
- Opus, oneVPL and GNU runtime DLLs come from MSYS2 UCRT64. Their installed license notices are copied into the package. MSYS2 source/build recipes: https://github.com/msys2/MINGW-packages.
- NVIDIA RTX Video SDK 1.1.0 archive SHA-256 `abf4f34e2b5a618e355b0d5a0365d8ecc3db4396e756e4c850a867e1ae2ed69e`. The optional adapter links NVIDIA's NGX import library and ships `nvngx_truehdr.dll` under the included NVIDIA RTX Video SDK license. It is excluded with `-SkipTrueHdr`.
- Windows GPU, audio, input, service and security APIs are imported through Microsoft's Rust `windows` crate. Vulkan headers define the PyroWave ABI; Vulkan is supplied by the installed graphics driver.
- The host compiles its embedded HLSL GPU conversion shaders through Windows' D3DCompiler API. These shaders are part of Butterpollo's source; the Windows compiler and GPU driver remain system dependencies.

The standalone C programs under `rust/tests` are independent interoperability fixtures. Moonlight-common-c, FFmpeg decoding, OpenSSL and Opus are used by these fixtures and are not compiled as original host implementation code.
