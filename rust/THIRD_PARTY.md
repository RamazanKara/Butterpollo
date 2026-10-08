# Third-party code

Butterpollo Rust is GPL-3.0-only. Its administration console is a Svelte application served by the Rust host; the bundle includes the Svelte runtime (MIT License, Svelte Contributors), and the console's npm dependencies and their versions are recorded in `rust/web/package-lock.json`. Rust package names, versions, license declarations and repositories are recorded in the packaged `rust-dependencies.json`; exact versions and registry checksums are in `Cargo.lock`. Source is available in the corresponding repository checkout.

External SDKs are not a port of the original host:

- FFmpeg SDK: LizardByte/build-deps `v2026.516.30821`, Windows AMD64 archive SHA-256 `2f7a2c2fc6be9b96de3c6f654389f73a5e5d369d7e802d017894fae96247661d`. Static encoding libraries include FFmpeg/libavcodec/libavutil/libswscale, x264, x265, SVT-AV1 and HDR10+ support. FFmpeg and codec source/build recipes: https://github.com/LizardByte/build-deps/tree/v2026.516.30821. GPL source and distribution requirements apply to the combined executable.
- AMF headers are provided by the FFmpeg SDK; AMF runtime is loaded from the installed AMD graphics driver. SDK: https://github.com/GPUOpen-LibrariesAndSDKs/AMF.
- Native NVENC/CUDA ABI headers are vendored unchanged from FFmpeg/nv-codec-headers commit `33a9ede8d9914299d9262539c576a15bd0a19621`, the baseline's Video Codec SDK 13.0 headers. Their permission/copyright notices remain in the headers and are copied into the package. Only installed Windows system driver DLLs are loaded; no NVIDIA import library, CUDA toolkit or previous host implementation is linked. Source: https://github.com/FFmpeg/nv-codec-headers/tree/33a9ede8d9914299d9262539c576a15bd0a19621.
- PyroWave `186f0393b77f7755953b5ecde994bb1cec2e4155`, API 0.6.0; Granite `b6cffd5ce81f540f0855e6778428483e14763d9b`. `rust/tools/build_pyrowave.sh` builds the pinned upstream SDK and applies Vibepollo 2.0's buffer-pool, 4:4:4 payload-sizing and short-decoder-block patches from `rust/codec-patches/pyrowave`. Their source patches are retained; packaged `pyrowave-build-info.txt` records revisions and patches. MIT notices are included for PyroWave/Granite/volk. Source: https://github.com/Themaister/pyrowave/tree/186f0393b77f7755953b5ecde994bb1cec2e4155.
- Opus, oneVPL and GNU runtime DLLs come from MSYS2 UCRT64. Their installed license notices are copied into the package. MSYS2 source/build recipes: https://github.com/msys2/MINGW-packages.
- NVIDIA RTX Video SDK 1.1.0 archive SHA-256 `abf4f34e2b5a618e355b0d5a0365d8ecc3db4396e756e4c850a867e1ae2ed69e`. The optional adapter links NVIDIA's NGX import library and ships `nvngx_truehdr.dll` under the included NVIDIA RTX Video SDK license. It is excluded with `-SkipTrueHdr`.
- Windows GPU, audio, input, service and security APIs are imported through Microsoft's Rust `windows` crate. Vulkan headers define the PyroWave ABI; Vulkan is supplied by the installed graphics driver.
- The host compiles its embedded HLSL GPU conversion shaders through Windows' D3DCompiler API. These shaders are part of Butterpollo's source; the Windows compiler and GPU driver remain system dependencies.

The standalone C programs under `rust/tests` are independent interoperability fixtures. Moonlight-common-c, FFmpeg decoding, OpenSSL and Opus are used by these fixtures and are not compiled as original host implementation code.

## ViGEmClient

`windows/src/input/vigem.rs` ports the DeviceIoControl protocol and target lifecycle from [ViGEmClient](https://github.com/nefarius/ViGEmClient): `src/ViGEmClient.cpp`, `include/ViGEm/Common.h` and `include/ViGEm/km/BusShared.h`. It uses Windows APIs directly and does not link the C client. Controller mappings and DS4 calibration follow [Vibepollo's Windows input code](https://github.com/Nonary/Vibepollo/blob/master/src/platform/windows/input.cpp), under GPL-3.0. ViGEmBus itself is a separate driver; see below for the copy setup installs.

MIT License

Copyright (c) 2018 Benjamin Höglinger-Stelzer
Copyright (c) 2016-2019 Nefarius Software Solutions e.U. and Contributors
Copyright (c) 2017-2023 Nefarius Software Solutions e.U. and Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## ViGEmBus

The package carries ViGEmBus 1.22.0's unmodified setup, `drivers/vigembus/ViGEmBus_1.22.0_x64_x86_arm64.exe` (SHA-256 `89220a7865076b342892f98865f3499fb7c4cfd673159e89d352c360fd014c6a`, signed by Nefarius Software Solutions e.U.), from https://github.com/nefarius/ViGEmBus/releases/tag/v1.22.0. Butterpollo's setup runs it silently when ViGEmBus is not installed yet; it installs as its own product and stays installed when Butterpollo is removed.

BSD 3-Clause License

Copyright (c) 2016-2020, Nefarius Software Solutions e.U.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
