# Rust performance evidence

Measured locally on 2026-10-01: Ryzen 7 5800X3D (8 cores/16 threads), RX 7900 XT, AMD driver 32.0.31041.1004, Windows x64, Rust 1.98.1 release builds. The initial HEVC/AV1 runs captured a 1968×2184 HDR desktop. The later 2.0 candidate runs captured a 2560×1440 SDR desktop and converted/scaled it to the requested format; physical HDR remained disabled. Neither source establishes native 4K capture performance. Hosts ran on loopback with isolated configurations and display changes disabled. The installed production service was preserved; it was stopped during the 2.0 candidate tests.

The measured reason to switch is video FEC generation: the Rust implementation is 1.26–1.40× faster than the original C++ implementation on representative video blocks, using 21–29% less CPU time for identical parity bytes. GPU-resident HDR processing, bounded texture/encoder queues and a Rust-rendered console are additional implementation benefits. Changing language alone does not establish a performance improvement, and the FEC results do not establish a whole-host or end-to-end latency improvement.

## Controlled comparison with the original C++ FEC

The benchmark loads the original C++ host's Reed–Solomon wrapper from baseline commit `f23ee0c9e7857887be7f774de6ac5153500a7e53`, with its pinned nanors implementation at `19f07b513e924e471cadd141943c1ec4adc8d0e0`. The source verification script checks every compiled reference file against its pinned SHA-256. GCC 16.1.0 builds the reference with `-O3 -ftree-vectorize -funroll-loops`; Rust uses the release profile. Both select AVX2 on this Ryzen 7 5800X3D. The original runtime ISA dispatch is retained.

| Data + parity shards, 1416 bytes each | Original C++ median | Rust median | Speedup | CPU time saved |
| --- | ---: | ---: | ---: | ---: |
| 32 + 7 | 6.96 µs | 5.51 µs | 1.26× | 21% |
| 96 + 20 | 63.03 µs | 45.87 µs | 1.37× | 27% |
| 192 + 39 | 250.01 µs | 178.72 µs | 1.40× | 29% |

Each result is the median of seven half-second runs, with Rust/C++ order alternated. Caller shard buffers and pointer views are allocated outside the timing; the original per-frame matrix allocation, encoding and release remain inside it. Every parity byte is compared before and after each round. A small generic Cauchy case (4 + 2 shards of 144 bytes) measures 0.139 µs in C++ and 0.133 µs in Rust, effectively equal; it is not the fixed Moonlight audio FEC matrix.

The Rust changes precompute finite-field inverses and reuse AVX2 input loads across four parity rows. SSSE3 and scalar fallbacks preserve the wire format. Tests cover every field coefficient, short and unaligned payloads, row-group tails and the 255-shard boundary. This is a component comparison against the previous implementation, with reproducible source inputs, rather than a comparison against an earlier slow Rust prototype.

```powershell
# Initialize third-party/nanors in a source checkout first.
pwsh -NoProfile -ExecutionPolicy Bypass -File .\rust\tests\build-fec-reference.ps1 -ArtifactDirectory C:\path\to\artifacts
cargo build -p butterpollo-core --example protocol_performance --release --locked
.\target\release\examples\protocol_performance.exe C:\path\to\artifacts\cpp-nanors-reference\nanors-reference.dll > fec.json
```

The release package includes the same probe as `butterpollo-protocol-performance.exe`. The reference DLL is a test fixture built separately; it is not shipped or linked into the Rust host. Building or running this comparison does not start either host or change display settings.

## Repeat-frame encoder capacity

The 2.0 candidate PyroWave record path completed **961 frames in 8.003 seconds at 3840×2160/120 HDR**, or **120.09 fps**, at an 800 Mbps target. Submission-to-observed-output time averaged **2.17 ms**, with **3.21 ms p95**. GPU conversion uses shared D3D11/Vulkan planar inputs and reads back only the encoded bitstream. This repeated-frame test covers conversion, encoding and framing; it excludes live capture, packetization/FEC, transport, decoding and display latency. It is a capacity measurement, not a whole-host comparison against C++.

```powershell
.\butterpollo-performance.exe --codec pyrowave --records --hdr --width 3840 --height 2160 --fps 120 --bitrate 800000 --seconds 8 --paced
```

The same real captured frame was repeatedly converted and encoded, with 20 warmup submissions followed by an eight-second unpaced run. Counts refer to returned encoded frames. Capture occurs before the timed loop; network, client decoding and display latency are excluded. Requested frame rate was 120; bitrates were 20/40/80 Mbps at 1080p/1440p/4K respectively.

| HEVC HDR output | Earlier Rust CPU path, fps | Rust GPU path, fps | GPU encode-call mean |
| --- | ---: | ---: | ---: |
| 1920×1080 | 6.41 | 689.08 | 1.45 ms |
| 2560×1440 | 3.77 | 420.75 | 2.38 ms |
| 3840×2160 | 1.69 | 202.68 | 4.93 ms |

The baseline is commit `319a3833b37119c6dac37339c7363bedc0bc0bee`, before this change. Both runs used the same capture dimensions, GPU and driver; the desktop content was not frozen across the runs. This is a repeated-frame capacity comparison against the earlier **Rust** implementation, not a controlled comparison against C++ and not a live-stream frame rate. Encode-call duration also excludes asynchronous codec completion; use completed-output latency or full-stream measurements to assess latency.

After setting AMF's AV1 alignment mode to allow unaligned input dimensions, the packaged probe produced 849.62 fps with 1080p HDR/120 input in a separate repeat-frame run. Previously initialization failed because AMF's default alignment excludes a 1080-line image. However, the full-stream decoder found a coded height of 1082, and the strict dimension check rejected that run. AMD documents two padded rows on this hardware for 1080p AV1; use HEVC for exact 1080p output or an aligned AV1 resolution such as 1440p/4K. This is not a validated exact-resolution 1080p AV1 result. [AMF's alignment documentation](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/AV1-Encoder#av1-specific-api)

That capacity run's submission-to-observed-output mean was 10.40 ms under an intentionally saturated queue; it is not a latency-oriented workload.

## Full encrypted Moonlight streams

The independent C fixture uses Moonlight-common-c for encrypted RTSP, control and UDP video/audio transport, then FFmpeg and Opus to decode the results. It rejects codec fallback and verifies the decoded dimensions, ten-bit HDR, BT.2020 primaries and PQ transfer. Each performance run lasts 20 seconds after connection initialization. Client frame rates include startup and buffered decoder frames; host steady rate uses session counter deltas after two seconds.

| Output and requested rate | Decoder threads | Host steady fps | Decoded fps | Completed encode mean / p95 |
| --- | ---: | ---: | ---: | ---: |
| H.264 SDR 1080p/120 | 4 | 120.01 | 118.11 | 2.29 / 2.60 ms |
| HEVC HDR 1080p/120 | 1 | 120.03 | 118.70 | 2.27 / 2.60 ms |
| HEVC HDR 4K/120 | 1 | 119.89 | 105.47 | 6.22 / 6.50 ms |
| HEVC HDR 4K/120 | 4 | 120.02 | 117.98 | 6.33 / 6.70 ms |
| AV1 HDR 4K/120 | 4 | 120.04 | 117.96 | 5.43 / 5.80 ms |

All five runs reported zero codec/Opus decoding errors. The single-threaded 4K decoder took 9.33 ms per submitted packet, exceeding the 8.33 ms frame budget; its receive queue eventually dropped packets and requested keyframes. The three latest four-thread runs enable `wgc_slot_aligned_publish=true` and `amd_ltr_frames=4`. HEVC 4K/120 received 2371 complete frames and decoded 2368 in 20.072 seconds; three frames remained buffered in the decoder. AV1 4K/120 received 2514 complete frames and decoded 2512 in 21.295 seconds. These client totals include startup/teardown, while host steady rates use counter deltas. Completed encode latency covers conversion/submission until the host observes encoded output, including asynchronous work. It excludes capture age, FEC/packetization, networking, decoding and display scanout.

### Final 2.0 candidate standard-codec checks

The final candidate repeated the three standard-codec checks with a 2560×1440 SDR capture source, a requested 120 fps and four software decoder threads. Each encrypted stream ran for 20 seconds and passed strict dimensions/color, permissions and client hooks with zero video/audio decode errors. HEVC/AV1 convert and scale that source to ten-bit 4K HDR; this does not validate native HDR capture or native 4K capture.

| Output | Host steady fps | Decoded fps | Host processing mean / p95 |
| --- | ---: | ---: | ---: |
| H.264 SDR 1080p | 120.03 | 118.13 | 3.37 / 6.70 ms |
| HEVC HDR 4K | 120.05 | 118.20 | 7.05 / 12.50 ms |
| AV1 HDR 4K | 120.04 | 118.26 | 7.14 / 13.00 ms |

The final wire-header processing metric includes capture age and completed encoding; PyroWave also includes its sender wait. It excludes packetization after the header, network transit, decoding and scanout. Earlier completed-encode measurements above exclude capture age, so their latency columns are not directly comparable. Client frame rates include startup/teardown.

## Vibepollo 2.0 PyroWave transport

The pinned Nonary Moonlight-common-c transport at `d6a11bc685b41037b352a96f29d08276fe5359ba` receives/decrypts/FEC-recovers record packets, then the independent vendor decoder renders to a CPU buffer. Four 20-second encrypted streams at 1920×1080/120 and 200 Mbps pass with zero video/audio decode failures or partial frames. The source is a 2560×1440 SDR desktop. HDR cases verify the encoded profile and Moonlight HDR control state; normalized eight-bit CPU readback does not validate ten-bit Qt HDR rendering or display scanout.

| Profile | Host steady fps | Decoded fps | Host processing mean / p95 |
| --- | ---: | ---: | ---: |
| SDR 4:2:0 | 119.57 | 116.46 | 3.08 / 8.40 ms |
| HDR 4:2:0 | 119.95 | 116.51 | 3.08 / 9.60 ms |
| SDR 4:4:4 | 119.96 | 117.23 | 2.66 / 5.30 ms |
| HDR 4:4:4 | 119.97 | 117.44 | 1.88 / 2.40 ms |

Host processing includes capture age, completed encoding and waiting for the PyroWave sender, as recorded in Moonlight's wire header. It excludes packetization after the header, network transit, decoding and scanout. The console's encode p95 measures the encoder separately. Client rates include startup/teardown. The HDR 4:4:4 row is the final repeat after enforcing a minimum one-tick advance on the 90 kHz RTP clock; it decoded all 2,357 received frames with no partial frames. These measurements do not establish a whole-host advantage over C++.

At an 800 Mbps target, the same loopback sender is the limiting stage: a 30-second 1080p run delivers 101.49 steady host fps and decodes 2,993 frames (99.40 fps including startup). Host processing averages 8.10 ms with 14.30 ms p95. Older pending intra frames are replaced; they do not accumulate in an unbounded queue. This verifies continued decoding across many RTP sequence wraps and the visible backpressure counter, not 800 Mbps playback at 120 fps or a physical-network stress test.

The fixture requires MSYS2 UCRT64 GCC/CMake/Ninja, OpenSSL/Opus development libraries and the pinned patched PyroWave SDK. It is separate from the shipped Rust host:

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File rust/tests/build-pyrowave-client.ps1 -ArtifactDirectory C:\tests\pyrowave -PyrowaveRoot C:\path\to\pyrowave-186f0393
$env:BUTTERPOLLO_TEST_CLIENT_EXE = 'C:\tests\pyrowave\moonlight-pyrowave-client.exe'
$env:PATH = 'C:\msys64\ucrt64\bin;' + $env:PATH
python rust/tests/interop.py C:\tests\pyrowave pyrowave-hdr-444 1920 1080 120 20 200000 4
```

## CPU fallback conversion

This comparison used exactly the same synthetic 1968×2184 FP16 scRGB image, with patterned RGB data, resized to each output dimension. Each version ran for at least three seconds. The new CPU implementation uses lookup tables, precomputed resize columns and at most eight Rayon workers, with a serial fallback if worker creation fails.

| Output | Earlier Rust mean | New Rust mean | Speedup | Maximum 16-bit output difference |
| --- | ---: | ---: | ---: | ---: |
| 1920×1080 | 134.41 ms | 8.89 ms | 15.1× | 0 |
| 2560×1440 | 240.34 ms | 14.67 ms | 16.4× | 0 |
| 3840×2160 | 539.96 ms | 34.26 ms | 15.8× | 0 |

These timings cover RGB HDR conversion/scaling only, not GPU upload or encoding. The zero difference applies to this test image; it is not a universal bit-identical claim. Unit tests separately check FP16 decoding, PQ reference luminances, linear-light resizing and already-PQ ten-bit input.

## Color and ownership checks

The Rust GPU converter keeps absolute luminance through 10,000 nits. A separate HEVC decode of grayscale patches at 0/80/1000/10000 nits returned limited-range P010 luma codes 64/490/723/940 and neutral chroma 512/512. Hardware tests compare primaries and grayscale against a CPU reference within two ten-bit codes, check a linear-light resize and verify that frames retained by the codec are not overwritten when the bounded pool is reused. New 4:4:4 tests verify independent adjacent chroma and planar ten-bit HDR codes. The packed AYUV shader is checked through its compatible RGBA render-target view on AMD; actual AYUV resource allocation and NVIDIA CUDA interop require NVIDIA hardware. The abandoned vendor conversion path clipped the 10,000-nit patch; that path is not used.

Native Winsock checks delivered eight separate datagrams in two send calls on both IPv4 and IPv6, then verified the ordinary-send fallback and a short trailing datagram. This validates segmentation boundaries and the reduced call count; it does not establish a network throughput speedup. Video batches respect the previous 16/32/64 KiB setting, a 64-packet/65,507-byte ceiling and a two-millisecond wire budget.

Independent strict FFmpeg loss fixtures encode 64 frames, omit frames 5–8 and 17–20, and decode all 56 retained frames for H.264, HEVC and AV1. HEVC/AV1 use two LTR recoveries; H.264 uses one LTR recovery and an IDR fallback at its reference-counter wrap. Actual Opus round trips cover 21 stereo/5.1/7.1/custom-layout, quality and packet-duration combinations, preserve every channel and stay inside the transport packet budget. These checks establish recovery/audio correctness, not an end-to-end latency improvement.

## Reproduce on another machine

The release ZIP includes `butterpollo-performance.exe`, which captures one frame and prints JSON. It does not start a service or change display configuration. A functioning local display/capture session and the relevant codec/GPU driver are required.

```powershell
.\butterpollo-performance.exe --encoder amf --codec hevc --hdr --width 3840 --height 2160 --fps 120 --bitrate 80000 --seconds 8
.\butterpollo-performance.exe --encoder amf --codec hevc --hdr --width 3840 --height 2160 --fps 120 --bitrate 80000 --seconds 8 --paced
.\butterpollo-performance.exe --encoder amf --codec hevc --hdr --width 3840 --height 2160 --fps 120 --bitrate 80000 --seconds 8 --cpu
```

Unpaced mode measures capacity with a bounded queue; paced mode measures completed-output latency at the requested cadence. `--cpu` reads back the captured frame once and measures the CPU compatibility encoding path. JSON reports source dimensions, adapter, returned-frame count, call time and submission-to-observed-output time. The source probe is `windows/examples/performance.rs`; `cargo build -p butterpollo-windows --example performance --release --locked` builds it with the SDK environment from the main build.

For full streams, use an isolated host configured with base port 48123, the fixture credentials `test` / `rust-smoke-only` and a Desktop app. These are test-only credentials for a loopback fixture. `tests/build-moonlight-client.ps1` builds the independent C client against the initialized Moonlight-common-c submodule. The MSYS2 UCRT64 environment additionally needs FFmpeg decoding development libraries and OpenSSL (`mingw-w64-ucrt-x86_64-ffmpeg` and `mingw-w64-ucrt-x86_64-openssl`), alongside Opus, CMake, Ninja and GCC. Python needs `requests` and `cryptography`. The probe creates a client certificate, performs real PIN pairing, grants only its fixture launch permissions and cancels/unpairs that fixture afterward.

```powershell
$env:BUTTERPOLLO_TEST_PORT = '48123'
.\rust\tests\build-moonlight-client.ps1 -ArtifactDirectory C:\path\to\artifacts
python rust/tests/interop.py C:\path\to\artifacts hevc-hdr 1920 1080 120 20 20000 1
python rust/tests/interop.py C:\path\to\artifacts hevc-hdr 3840 2160 120 20 80000 4
python rust/tests/interop.py C:\path\to\artifacts av1-hdr 3840 2160 120 20 80000 4
```

The positional arguments after the artifact directory are codec, width, height, requested fps, duration, requested bitrate in kbps and software decoder thread count. Reports contain per-half-second host counters and the derived host steady rate. Requested Moonlight bitrates can be adjusted during negotiation; the actual encoder bitrate is in each session sample.

## Limits

This machine validates AMD AMF. Native NVENC now calls the installed NVIDIA driver directly, supports reviewed API versions 11.0–13.0, reference frame invalidation, D3D11 4:2:0/8-bit 4:4:4 and GPU-only CUDA interop for ten-bit 4:4:4. Seven mock-driver tests exercise compatibility, asynchronous ownership, timeout teardown, metadata lifetime, loss recovery and bitrate changes; NVIDIA execution/performance still needs NVIDIA hardware. QSV has native D3D11 imports, and TrueHDR has a shared-device GPU path; these need Intel/NVIDIA hardware respectively. Unsupported native formats, PyroWave and software encoding use CPU compatibility paths. These measurements do not establish network streaming outside loopback, multiple concurrent 4K sessions, dynamic game content, native 4K capture or end-to-end input/display latency. The GPU texture pools and native encoder queues are bounded to eight retained frames; capacity runs may intentionally keep those queues occupied. [PARITY.md](PARITY.md) separates implemented features from native validation.

A whole-host C++ A/B performance comparison remains outstanding. The available local C++ executable was an older `butter.2` build, and its startup performed global virtual-display recovery despite the isolated configuration. It was stopped before streaming tests. The controlled FEC comparison above uses the exact baseline sources without starting the C++ host. Original C++ streaming measurements in the parent README remain reference data for that implementation.
