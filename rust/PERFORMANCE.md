# Rust performance evidence

Measured locally on 2026-10-01: Ryzen 7 5800X3D (8 cores/16 threads), RX 7900 XT, AMD driver 32.0.31041.1004, Windows x64, Rust 1.98.1 release builds. The initial HEVC/AV1 runs captured a 1968×2184 HDR desktop. The later 2.0 candidate runs captured a 2560×1440 SDR desktop and converted/scaled it to the requested format; physical HDR remained disabled. Neither source establishes native 4K capture performance. Hosts ran on loopback with isolated configurations and display changes disabled. The installed production service was preserved; it was stopped during the 2.0 candidate tests.

The measured reason to switch is video FEC generation: the Rust implementation is 1.26–1.40× faster than the original C++ implementation on representative video blocks, using 21–29% less CPU time for identical parity bytes. GPU-resident HDR processing, bounded texture/encoder queues and a Rust-rendered console are additional implementation benefits. Changing language alone does not establish a performance improvement, and the FEC results do not establish a whole-host or end-to-end latency improvement.

## Capture and encoder wakeups on Windows

On 2026-10-02, the installed candidate's short condition-variable timeouts were measured on the same Windows machine. A requested 250 µs encoder poll could sleep for a scheduler tick. Capture consumers now wait on their own unnamed event together with the worker's high-resolution timer. A capture arriving before the wait remains signaled; one client resetting its event cannot consume another client's notification. The wait does not spin or change the system timer resolution. Windows documents this combination in [waitable timers](https://learn.microsoft.com/en-us/windows/win32/sync/waitable-timer-objects) and [wait functions](https://learn.microsoft.com/en-us/windows/win32/sync/wait-functions).

| Requested wait | Condition-variable mean / p95 | Capture event + timer mean / p95 |
| --- | ---: | ---: |
| 250 µs | 15.293 / 16.309 ms | 0.615 / 1.010 ms |
| 500 µs | 15.206 / 16.289 ms | 0.971 / 1.052 ms |
| 16.667 ms | 30.775 / 32.101 ms | 16.798 / 17.010 ms |

Each case uses 40 actual waits in the same release process. These measurements establish lower timeout overshoot on this Windows installation, not a 25× whole-stream speedup. Reproduce with `cargo run -p butterpollo-windows --example wait_performance --release --locked` from `rust` in the SDK environment used for the build.

An unchanged desktop image also no longer consumes the next encode slot. Fresh content arriving after a waiting deadline can be submitted immediately, while encoding submissions retain the configured cadence. Resuming after a longer static interval starts a new cadence without a catch-up burst. Tests cover scheduler overshoot, static resumes, capture arriving before a wait, independent capture consumers and timer reuse.

The session API and five-second host log samples now report capture-to-packetization host processing separately from encoder latency. This uses the same duration written into Moonlight's frame header, without resetting fresh capture timestamps or reducing resolution, refresh rate, HDR or bitrate. Full-stream idle-desktop comparisons are recorded below; a dynamic game and the customer's client remain separate acceptance checks.

Independent encrypted Moonlight decoding captured the existing native 1968×2184 HDR display, with HEVC Main10, a requested 40 Mbps, 20-second requested runs and the default 20 FPS static repeat target. The actual negotiated encoder bitrate was 30,988 kbps. Display changes were disabled in the isolated loopback profiles; the installed service stayed running without a client during these comparisons. Both builds decoded all four runs without a reported failure.

| Requested stream rate | Installed candidate host mean / p95 | Revised host mean / p95 |
| --- | ---: | ---: |
| 60 FPS | 4.548 / 4.700 ms | 4.183 / 4.200 ms |
| 120 FPS | 4.187 / 4.200 ms | 4.120 / 4.200 ms |

Measured host steady rates rose from 16.83/18.15 FPS to approximately 20 FPS for the static desktop. This restores the configured repeat cadence; it does not demonstrate a 60/120 FPS motion rate or a large improvement in idle encoder processing. The customer's reported 12 ms average / 30 ms maximum was not reproduced in these idle loopback runs, so that workload still needs a client retest.

## Customer regression checks on 2026-10-02

The connected phone reported missing audio, sluggish mouse input and approximately 11 ms average / 20 ms maximum host processing at 1968×2184/120 AV1 HDR. Its actual host log confirmed WGC was being opened even though the requested backend was `ddx`, and virtual speaker routing failed with `0x88890008`. The revised build recognizes the old capture alias, composes DDX's separately supplied cursor in the GPU color pass, and restores the previous PCM/float speaker format choices, including valid 24-bit samples in a 32-bit container. Input polling uses the same high-resolution timer as capture.

A same-process input wait comparison (40 waits per case) measured the previous requested 1 ms thread sleep at 1.534 ms mean / 1.576 ms p95, versus 1.022 / 1.030 ms with the timer. This is a polling-wait improvement, not a measured mouse-to-display latency reduction.

The service-context fixture runs as LocalSystem in the signed-in desktop session, uses a separate loopback profile and cancels if the phone connects to the installed host. It creates an actual 1968×2184 HDR virtual display and streams 120 FPS with an 80 Mbps requested bitrate. Desktop Duplication is explicitly logged. HEVC strict independent decoding passes with zero failures, including nonzero audio from a quiet 960 Hz tone rendered to Steam Streaming Speakers. WASAPI captures the tone; encrypted transport and independent Opus decoding produce a peak of 0.053 and RMS of 0.032. Teardown restores the audio journal. This covers real routing rather than merely successful decoding of silent packets.

| Actual virtual-display stream | Steady host FPS | Steady host mean / p95 | Steady maximum |
| --- | ---: | ---: | ---: |
| HEVC HDR 1968×2184 | 119.72 | 3.735 / 3.831 ms | 4.653 ms |
| HEVC HDR, next five-second sample | 119.72 | 3.768 / 3.940 ms | 6.738 ms |
| AV1 HDR 1968×2184, final five-second sample | 119.67 | 3.192 / 3.243 ms | 3.470 ms |

The HEVC client's whole-run mean is 5.733 ms and p95 is 3.900 ms: startup outliers raise the mean. Its 99.35 decoded FPS includes a two-second first-frame delay in the requested 12-second run, so that total is not a steady streaming rate. The AV1 run is **not an interoperability pass**: it decodes 1984×2186 rather than the requested 1968×2184, and every decoded frame fails the unchanged dimension check. Its first steady sample has 10.439 ms p95; the later sample above is lower. AMD's [AV1 alignment contract](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/AV1-Encoder#av1-specific-api) permits padded output for unaligned dimensions. The host does not silently change codec, resolution or quality to pass this check. Both privileged runs log a Windows layout-restoration warning; their separate recovery helpers subsequently clear the journals and restore the physical-only desktop. This is not a clean immediate-restoration acceptance pass.

The paced AV1 encoder probe, with actual DDX capture kept active, returns 120 FPS and 3.216 ms mean / 3.614 ms p95 from submission to observed output. Its source is the physical 2560×1440 SDR desktop converted to HDR, so it does not measure native virtual HDR capture age or full-stream processing. A repeat of the earlier WGC-backed probe also measures about 3.3 ms; these probes show no encoder-only advantage from changing capture backend. The full-stream changes still need a fresh test on the customer's phone and dynamic content. They do not establish a controlled C++ whole-host improvement.

Reproduce the active-capture component probe with `butterpollo-performance.exe --capture ddx --live-capture --encoder amf --codec av1 --hdr --width 1968 --height 2184 --fps 120 --bitrate 80000 --seconds 6 --paced`. The independent stream fixture accepts `BUTTERPOLLO_TEST_MATCH_DISPLAY=1` to request the matching display mode and `BUTTERPOLLO_TEST_AUDIO_TONE=1` to require a nonzero decoded tone. The latter requires a separate renderer such as `windows/examples/audio_probe.rs`; setting the variable alone fails a silent stream. Privileged virtual displays require the installed service's security context; administrator elevation alone is not sufficient for this driver's access policy. The optional `session_command` example reproduces that context from a test-owned LocalSystem task without changing driver permissions.

## Video packet processing after the first 2.0 candidate

Compared with Rust candidate `118d0ef133ae2d63898fb61512d25b6253d9c79b`, the revised packet path uses **11–16% less CPU time for encrypted frames** in these workloads. It reserves the encryption envelope in the final shard allocation, excludes that envelope from FEC, reuses AES-GCM setup within each frame and encrypts the shard in place. Source data goes directly into its shards, avoiding a full packed-frame copy. Windows UDP batches also use a bounded stack buffer for their descriptors.

| Data shards, 20% FEC | Encryption | Previous Rust median | Revised median | CPU time saved |
| --- | --- | ---: | ---: | ---: |
| 32 | Off | 8.87 µs | 7.57 µs | 15% |
| 32 | AES-GCM | 47.73 µs | 40.12 µs | 16% |
| 192 | Off | 160.61 µs | 151.44 µs | 6% |
| 192 | AES-GCM | 393.96 µs | 344.61 µs | 13% |
| 576 | Off | 492.40 µs | 457.43 µs | 7% |
| 576 | AES-GCM | 1185.92 µs | 1045.23 µs | 12% |

Each case uses seven 250 ms rounds on the same Ryzen/Rust release environment, with prebuilt identical payloads of 44,024, 264,184 and 792,568 bytes. Timing includes header construction, allocations/copies, FEC, optional encryption and packet release; it excludes capture, encoding and UDP sending. Both builds produce the same first-frame SHA-256, packet counts and wire byte totals in all six cases. Tests separately compare every encrypted shard against independent per-packet sealing through FEC, partial tails and sequence/frame/nonce boundaries. These results compare two Rust versions and do not establish a whole-host improvement over C++.

The package includes `butterpollo-video-packet-performance.exe`. Run it without arguments to print the workloads, timing samples and packet fingerprints. The source is `core/examples/video_packet_performance.rs`; use that same harness in the previous checkout when reproducing the comparison.

## October 2 latency work

The candidate removes synchronous display renewal and monitor enumeration from
the lock used by the capture and encoder workers. Those workers now read a
published output/generation pair; maintenance publishes a new identity after
native work finishes. A twenty-sample, read-only probe on the physical display
measured monitor enumeration at 1.558 ms mean, 2.605 ms p95 and 4.333 ms maximum.
This establishes a potentially expensive dependency in the old frame path,
not a measured full-stream improvement from removing it. HDR metadata reads
were negligible in this probe and remain on their existing polling schedule.

Same-size GPU color conversion now loads each source pixel once, preserving
the existing resize, HDR transfer, chroma and cursor behavior. At 1968×2184
FP16-to-P010, 64 D3D11 timestamp samples after 16 warmups measured 0.335 ms mean
and 0.337 ms p95 before, versus 0.170 ms mean and 0.175 ms p95 after. This is an
approximately 49% reduction in this GPU component's elapsed time. It excludes
capture, encoding, transport and decoding. The opt-in
`gpu_color::tests::gpu_conversion_timing` test reproduces the measurement.

Pointer-only Desktop Duplication updates reuse immutable owned desktop pixels
and update the detached cursor snapshot. A missed new desktop copy invalidates
the cache, preventing stale pixels after texture-pool exhaustion. Native
readback checks cover retained images, the full bounded pool and recovery.
Per-device GPU priority and maximum-frame-latency hints now match the previous
host, independently of whether privileged process scheduling is available.

Diagnostics add p99, an explicitly labelled capture-age estimate, and frame
send-completion intervals. The original source timestamps and Moonlight
processing durations remain intact. A bounded freshness-wait experiment is
disabled by default; it has not established a latency benefit.

A normal-user isolated HEVC Main10 stream passed encrypted pairing/permissions,
exact 1968×2184 output, BT.2020/PQ, and independent Opus decoding of a quiet
known tone. It requested 120 fps and 80 Mbps. After a five-second warmup, 900
frames delivered at 119.993 fps with no intervals above 1.5 frame periods.
Host processing mean/p95/p99/max was 5.517/5.9/6.3/6.4 ms. Arrival intervals had
9.198 ms p99 and 9.375 ms maximum. This test captured an unchanged 2560×1440
SDR desktop and converted/scaled it; it excludes native HDR motion acceptance
and is not a whole-host comparison with Vibepollo.

The independent AV1 geometry gate still fails on this RX 7900 XT. Requested
1920×1080, 1968×2184 and 2184×1968 decode as 1920×1082, 1984×2186 and 2240×1968,
respectively, in both SDR/HDR and both supported unrestricted alignment modes
tested. Requested component dimensions and alignment read back correctly, but
bitstream traces contain enlarged dimensions without a render-size correction.
The same customer-size failure occurs in the current C++ baseline. Do not
count these as exact-resolution passes or change the strict decoder gate.
[AMD's corresponding bug report](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423).

Dynamic loopback tests additionally render a changing barcode containing source
sequence/QPC values, then timestamp independent low-delay decoding. This
picture-age measurement includes rendering, DWM, capture, encoding, loopback
and software decode; it excludes remote display scanout. Source refresh must
be measured and matched, not inferred from a requested virtual-display mode.
The attempted 240 Hz C++ fixture actually presented at 120 Hz; comparisons with
the 240 Hz Rust fixture are therefore not accepted. The next elevated matched
benchmark launch was rejected by automatic approval review. Work continues
with non-elevated probes; native motion comparison remains pending.

## October 4: capture copies and conversion beside a game

On an RX 7900 XT (AMF 1.5.2), AV1 HDR at 1968×2184 already encodes at the
VCN's floor: 2.8-3.3 ms from submission to bitstream when idle. Tiles,
multi-VCN, pre-encode, CDEF, AQ, the lowest-latency mode and QueryTimeout 0
measured no gain; the driver already applies them under ultra-low latency.
The cost is elsewhere: `examples/gpu_load.rs` draws a game-like load
(≈5.6 ms GPU frames), and beside it every D3D11 step waits behind the game on
the graphics engine (conversion 7.2 ms instead of 0.9 ms, a plain copy 8.4 ms
instead of 0.75 ms). D3D11 GPU priorities, process scheduling classes and a
high-priority D3D12 direct queue did not change that. A D3D12 compute queue
did the same conversion in 0.22-0.29 ms under the load (`d3d12_probe`,
`copy_probe`).

Captured frames are now copied into shared textures on one compute queue and
converted on another, and AMF encodes from D3D12 (`windows/src/compute.rs`).
Two synchronisation details decided correctness:

- Desktop Duplication returns a frame while DWM's copy into it may still be
  running; the D3D11 device waits for it through the keyed mutex. A compute
  copy without that wait differed from the D3D11 copy in 159 of 788 frames
  idle and 316 of 325 beside the load (`ddx_sync_probe verify-nosync`). The
  D3D11 context signals a shared fence after acquiring, the copy waits for it
  on the GPU, and the D3D11 context waits for the copy before the frame is
  released: 0 of 1161 idle and 0 of 267 under load differed. A D3D11 fence
  signal holding no frame completes in 0.016 ms beside the load; holding a
  frame it completes when DWM's copy does (0.15 ms idle, 7-10 ms under load).
- AMF signals the fence it is given again after reading a D3D12 texture. With
  one fence for all conversions, that signal released the next texture before
  its conversion ran: a third of the streamed pictures repeated. Each output
  texture now has its own fence.

Synthetic moving frames, AV1 HDR 1968×2184 paced at 120 fps
(`performance --synthetic 16 --paced`, `load_matrix.py`):

| Case | Graphics queue | Compute queues |
|---|---|---|
| Idle | 3.23 ms mean, 3.55 p95 | 2.92 ms mean, 3.04 p95 |
| Beside the load | 14.32 ms mean, 32.55 p95, 90 fps | 2.79 ms mean, 2.85 p95, 120 fps |

Full encrypted streams from the isolated host, AV1 HDR 1968×2184 at 120 fps
and 80 Mbps from a 240 Hz virtual display, decoded by an independent client
that times a moving barcode from render to decoded picture (`run-motion.py`
virtual-motion; same binary, `gpu_compute_conversion` off and on):

| Case | Picture age mean / p95 | New pictures/s | Host mean |
|---|---|---|---|
| Idle, graphics queue | 12.98 / 13.89 ms | 120.6 | 3.45 ms |
| Idle, compute queues | 12.37 / 13.29 ms | 120.1 | 2.86 ms |
| Load, graphics queue | 52.87 / 71.02 ms | 44.6 | 21.2 ms |
| Load, compute queues | 45.45 / 62.26 ms | 50.1 | 10.6 ms |

Under the load DWM's own composition dominates (its copy into the
duplication surface finishes 7-10 ms after the frame is handed over), so the
host's share falls but the picture stays late. Host time under compute
includes that wait, because frames are published before DWM's copy finishes
and the encoder waits for it on the GPU. These runs exclude network transport
and a remote display.

### 1080p at 60 fps

The same comparisons at the most common stream setting: HEVC 10-bit HDR,
1920×1080 at 60 fps and 20 Mbps, from a 120 Hz virtual display, with the
same game-like load (the game ran at 174 fps in both paths). Two runs each.

| Synthetic encode | Graphics queue | Compute queues |
|---|---|---|
| Idle, mean | 2.26 / 2.40 ms | 2.14 / 2.03 ms |
| Beside the load, mean | 21.99 / 20.93 ms | 1.98 / 1.95 ms |
| Beside the load, p95 | 37.04 / 37.91 ms | 2.28 / 2.29 ms |
| Beside the load, encoded fps | 58.6 / 59.1 | 60.1 / 60.1 |

| Full stream | Graphics queue | Compute queues |
|---|---|---|
| Idle, picture age mean / p95 | 14.05 / 15.03, 13.98 / 14.72 ms | 14.05 / 14.63, 14.04 / 14.69 ms |
| Load, picture age mean / p95 | 40.92 / 54.04, 41.13 / 54.69 ms | 33.48 / 42.20, 33.46 / 42.46 ms |
| Load, new pictures per second | 56.8, 56.6 | 58.3, 57.8 |
| Load, present to send (host counter) | 16.39, 16.10 ms | 11.40, 11.07 ms |

Idle, both paths deliver the picture at the same time; the compute path's
fence handoffs cost about 0.15 ms of host time at this size, where the
D3D11 copy and conversion are small. Beside the game it delivers the
picture 7.5 ms sooner on average and 12 ms sooner at the 95th percentile.
Artifacts: `day-work-20261004\load-1080p60`, `day-work-20261002\p1080-*`.

### Against Vibepollo 2.0

Vibepollo 2.0 and Butterpollo 2.0.0-rc.2 alternated in one batch on the
same fixture. Vibepollo is a Release build of the 2.0.0 tag (`8a8c4b03a`)
with two startup-only patches for the isolated fixture (skip machine-wide
recovery, log the test display's name); no per-frame code changed. Both
hosts ran HEVC 10-bit HDR at 1080p60, 20 Mbps requested (14,988 kbps after
FEC and audio on both), native AMF at ultra-low latency and `speed` with
VBAQ and an input queue of 4, Desktop Duplication, realtime GPU priority,
and a 120 Hz virtual display (Vibepollo set to
`frame_limiter_auto_virtual_framegen = legacy`, its 2x mode; its default is
4x). Three runs each. The game-like load ran at 176.7-177.4 fps beside
Vibepollo and 173.8-174.9 fps beside Butterpollo, which streamed about
twice as many frames. "Host latency" is the per-frame value Moonlight
reports from the host; both hosts measure it from the moment Desktop
Duplication hands over the frame to the moment the packet is sent.

| Case | Vibepollo 2.0 | Butterpollo 2.0 |
|---|---|---|
| Idle, picture age mean / p95 | 16.03 / 16.62, 15.80 / 16.40, 16.05 / 16.93 ms | 13.88 / 14.58, 13.70 / 14.44, 13.73 / 14.52 ms |
| Idle, host latency | 2.73, 2.71, 2.80 ms | 1.96, 1.96, 1.97 ms |
| Load, picture age mean / p95 | 93.99 / 132.54, 92.04 / 129.45, 103.28 / 148.96 ms | 42.27 / 55.98, 42.31 / 56.81, 42.70 / 56.69 ms |
| Load, new pictures per second | 24.2, 25.0, 22.5 | 52.0, 51.0, 51.1 |
| Load, host latency | 59.09, 57.30, 65.28 ms | 9.11, 9.14, 8.80 ms |

Beside the load, Vibepollo handed AMF about 24 frames a second: its own
`encoder output has not caught up` lines count 60 submitted frames every
2.4-2.6 s. Butterpollo's graphics-queue path delivered about 57 new pictures
a second beside the same load in an earlier batch, so sharing the graphics
queue alone does not explain the gap; where Vibepollo loses the frames has
not been traced. The probe window rendered 50-59 fps beside the load with
Vibepollo and 59 fps with Butterpollo. Absolute load numbers move between
batches (Butterpollo measured 33.5 ms in an earlier batch without Vibepollo
runs), so only compare rows measured together. Artifacts:
`day-work-20261002\hh1080-*`.

Both hosts' virtual-display policy also turns on an RTSS 60 fps limit
during a stream (Vibepollo logs it). A second batch, beside the load only,
ran each host with the limit and with `frame_limiter_provider = none`:

| Beside the load | Vibepollo 2.0 | Butterpollo 2.0 |
|---|---|---|
| RTSS limit on, picture age mean / p95 | 124.84 / 156.05 ms | 42.68 / 57.60 ms |
| RTSS limit off, picture age mean / p95 | 125.81 / 158.30, 120.95 / 154.31 ms | 43.32 / 57.33 ms |
| New pictures per second, on / off | 18.6 / 18.5, 19.2 | 51.2 / 50.2 |
| Host latency, on / off | 79.7 / 79.8, 77.2 ms | 9.0 / 9.6 ms |

The limit changes neither host. One Butterpollo run with the limit off is
left out: its probe window did not start. Vibepollo was slower in this
batch than in the first (about 124 against 96 ms) and the probe rendered
only 40 fps beside it, while Butterpollo stayed at 42-43 ms. Artifacts:
`day-work-20261002\hh1080n-*`.

### HDR colour accuracy

`tests/colour_check.py` reads frame 900 of each stream above as decoded by
the client (`tests/moonlight_client.c` with `BUTTERPOLLO_TEST_FRAME_DUMP`),
recomputes the probe's scRGB picture for that frame, converts it as BT.2100
PQ with BT.2020 primaries in limited range, and compares. The
Vibepollo runs beside the load sent too few frames to reach frame 900, so
its column has the three idle runs; Butterpollo's has all six.

| | Vibepollo 2.0 | Butterpollo 2.0 |
|---|---|---|
| Black / 100-nit white patch (expected 64.0 / 509.08) | 64.0 / 509.0 | 64.0 / 509.0 |
| Luma error, mean absolute (10-bit codes) | 0.44 | 0.38-0.44 |
| Contrast slope (1 is exact) | 1.000 | 0.998-0.9995 |
| Saturation (decoded / expected chroma) | 97.9-98.0% | 100.2-100.6% |
| Chroma error, mean absolute | 0.96 | 0.30-0.59 |

Both streams also carry the same HDR10 metadata (BT.2020 primaries, D65,
the virtual display's peak luminance). Butterpollo's decoded pictures match
the expected values within half a 10-bit code on average, with no lifted
black and no lost saturation. A washed-out look therefore comes from before
capture (how Windows composes SDR content on an HDR display) or after
decoding (how the client shows HDR), not from the host's conversion or
encoding.

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

## October 5: WGC startup and notification experiment

The OpenCode investigation ended with a strict WGC smoke test succeeding as the
interactive user but failing as SYSTEM in that user's session:
`CreateForMonitor` returned `0x80070424`. The Rust service runs its host under
that SYSTEM token. Capture recovery already tried Desktop Duplication when WGC
could not open; initial stream startup did not. Startup and recovery now share
the fallback, log the actual backend and retain both Windows errors if neither
backend opens. Strict probes still fail rather than substitute DDX.

This is a fallback, not service-mode WGC support. A capture helper running as
the signed-in user is still needed. DDX fallback is not evidence of equivalent
VRR or game-frame-generation behavior.

The next experiment used WGC's
[frame-arrival callback on the pool's worker thread](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded)
to wake capture, replacing 500 us polling with notification waits bounded by a
100 ms housekeeping timer. Eight ten-second runs used the same release probe,
in poll/notify/notify/poll order for each GPU condition. The source was the
existing physical desktop, with approximately 35 changing pictures per second;
the first second was excluded from latency samples. GPU load was the existing
offscreen `gpu_load 55 1000 0 200` fixture. The installed host remained idle.

| Condition | Wake method | Frames sampled | Mean detection | Per-run p95 | Empty pool checks per ten seconds |
| --- | --- | ---: | ---: | ---: | ---: |
| Idle | 500 us polling | 632 | 0.566 ms | 1.247 / 1.487 ms | 18,884 / 18,898 |
| Idle | Notifications | 641 | 0.391 ms | 0.889 / 1.006 ms | 360 / 346 |
| GPU load | 500 us polling | 645 | 4.527 ms | 9.647 / 9.655 ms | 18,938 / 18,941 |
| GPU load | Notifications | 657 | 5.133 ms | 11.678 / 12.070 ms | 393 / 349 |

Detection means WGC's `SystemRelativeTime` to the host's snapshot acquisition,
including WGC's own delivery delay. The means above are weighted by frame
count; p95 values belong to individual runs. These are capture-component
measurements on changing desktop content, not a controlled game or an
end-to-end WGC/DDX comparison. They do not measure encoding, transport,
decoding or remote scanout.

Notifications reduced idle detection by 0.175 ms and almost eliminated empty
polls, but increased loaded detection by 0.606 ms in this batch. Therefore
**production capture keeps polling**. Callback registration and waits are
opt-in for `windows/examples/wgc_arrival_probe.rs`, not enabled by normal
streams. An exploratory hybrid run had zero updated frames after warmup and
overlapped part of a build; it is excluded and establishes no latency result.

The callback experiment also exposed a Windows teardown trap: revoking
`FrameArrived` after closing the pool aborts in `GraphicsCapture.dll` instead
of returning an error. The probe's registration is revoked before closure and
only once; the event remains owned until in-flight callbacks return. Sixteen
native reconnect/COM-teardown cycles pass with this order. Workspace tests
and warnings-as-errors checks pass too.

The final release build also passed an isolated encrypted user-mode WGC
stream: 1920x1080 HEVC SDR at 60 FPS, 20 Mbps requested, 12 seconds. The
independent client decoded all 715 received frames with zero failures and
decoded 2,222 audio packets with a nonzero test tone. The host log explicitly
reports `requested=wgc backend="wgc"`; capture was the existing 5120x1440
SDR physical desktop, with display changes disabled. This establishes
functional capture/encode/transport/audio interoperability, not a motion
latency improvement or service-mode WGC acceptance. The startup fallback's
failure cases are covered by regression tests; a new SYSTEM-context runtime
test was not run in this pass.

Local raw results, guards and validation logs are in
`C:\Users\ramaz\.codex\artifacts\butterpollo-wgc-20261005`.
The stream's full logs are in the earlier fixture's
`day-work-20261002/codex-20261005-wgc-startup` directory. The final host SHA-256
is `c10eeb65724d0598b7869dab6f6a6ace7715d2b46da06e0ab85640339eb3e0c2`.
The measured prototype's SHA-256 is
`b2b7e36667f7317864c0f8a31a01f2f5d4077691d02a69de398988a2121e68e9`.
For further investigation on a changing desktop, build the release
`wgc_arrival_probe` example and run `wgc_arrival_probe DISPLAY 10 poll`,
`wgc_arrival_probe DISPLAY 10 notify`, or `wgc_arrival_probe DISPLAY 10 hybrid`.
An empty display argument selects the primary output. A report with zero
post-warmup samples has no valid detection-latency comparison.

## October 5: LAN pacing and encoder follow-up

The reporter identifies an RX 9070 XT, the latest driver and Wi-Fi. Their
rc.2 log contains one HEVC hardware instance, repeated DDX access-loss
recovery, and transient UDP errors 10055 and 10035. The reported 4.7 versus
3.9 ms comparison remains open. This workstation has an RX 7900 XT with two
reported HEVC instances; disabling multi-instance encoding does not reproduce
the reporter's GPU. These tests do not establish the cause of that difference.

The independent receiver at `192.168.4.10` is an i5-8259U / Iris Plus 655 NUC
running Debian 13 on gigabit Ethernet. The Windows sender uses 2.5 Gb Ethernet.
The receiver uses Moonlight-common-c
`2600beaf13f18bfa43453609cf5e3b84a4227760`, FFmpeg and Opus in an isolated
Docker image. No host packages, network settings or installed service profile
were changed. Streaming profiles disable display mode/HDR changes; the later
motion-fixture refresh correction is documented below. A guard stops test-owned processes if an installed
stream or application becomes active.

### Packet pacing

Video pacing now measures its next wait from completion of the preceding
batch. A delayed socket call or wakeup can no longer accumulate credit for a
catch-up burst. Pacing includes Ethernet/IP/UDP wire overhead. Known physical
Ethernet routes cap the burst rate at 80% of link speed; unknown routes retain
the existing 800 Mbps default. This measures the sender's local Ethernet link,
not end-to-end capacity. It does not infer a Wi-Fi client's capacity through a
wired access point. Explicit pacing limits remain useful for that case, and
the stream bitrate and picture settings are unchanged.

The UDP probe sends 46,042 deterministic 1,400-byte datagrams: 600 frames at
60 FPS, 50 Mbps payload, with two keyframes eight times larger. The independent
Linux receiver records kernel timestamps, integrity and socket-overflow counts.
Its effective receive buffer is 425,984 bytes; Python processing and this buffer
are part of the stress fixture, not Moonlight's normal receiver.

| Pacing case | Actual receiver overflows | Complete frames | Mean frame send span |
| --- | ---: | ---: | ---: |
| Previous 800 Mbps, two runs | 344 / 400 | 598 / 599 | 0.693 / 0.693 ms |
| Revised 800 Mbps | 129 | 599 | 0.779 ms |
| Revised 80 Mbps | 0 | 600 | 10.728 ms |

The slower cap delivered every datagram without corruption, at the cost of a
longer send span. It is not a general latency win or proof of the Wi-Fi fix.
In a separate **modeled** 100 Mbps / 128 KiB bottleneck, adding a 6 ms sender
stall raised peak queued bytes to 93,357 with previous 80 Mbps pacing, versus
39,694 with completion-based pacing. Both corresponding real receiver runs
delivered every datagram. No sender-side 10055/10035 error was reproduced.
Raw results: `udp-results.json`; probe: `windows/examples/udp_pacing_probe.rs`.

### Encoder comparisons and rejected output-wait change

The controlled encoder fixture uses eight deterministic moving input textures,
HEVC 3840x2160 at 60 FPS / 50 Mbps, ULL/speed, VBAQ enabled and preanalysis
disabled. Each run encodes 480 pictures; comparisons use two runs per setting
in reverse order, separately idle and under the same offscreen GPU load.

| Existing conversion path | Idle mean | Loaded mean | Loaded per-run p95 |
| --- | ---: | ---: | ---: |
| Compute, current default | 6.339 ms | 5.955 ms | 6.209 / 6.203 ms |
| Graphics | 6.223 ms | 8.512 ms | 10.948 / 10.978 ms |
| Compute, multi-instance disabled | 6.320 ms | 5.948 ms | 6.178 / 6.183 ms |

This confirms the benefit of Opus's existing compute path under load on this
GPU; it is not a newly implemented speedup or a whole-stream comparison.

Removing an extra timer wait after AMF's own blocking output query reduced
loaded component mean latency from 5.958 to 5.831 ms. It also reduced idle
1080p60 LAN stream host mean from 2.279 to 2.201 ms and p95 from 2.6 to 2.3 ms.
However, the loaded LAN comparison regressed from 6.102 to 6.501 ms mean, with
per-run late-interval counts 89/76 before versus 83/97 after. All eight runs
decoded successfully and sustained approximately 60 FPS. The source was the
existing desktop scaled from 5120x1440, not controlled full-screen motion.
**The production output-wait change was reverted.** Component gains alone did
not justify the loaded stream regression. Raw results are in
`encoder-controlled`, `encoder-polling` and `lan-poll2-results.json`.

### Independent receiver limits and reproduction

A native DDX snapshot test failed twice with no initial image. The physical
display's existing idle timeout is 180 seconds. Holding a temporary display
power request made the same unchanged test pass once in 0.36 seconds, but
later repeats failed even with that request and a moving test window. The
standalone DDX check remains unresolved; the power request alone did not fix
it. Unlike the C++ host's capture loop, the Rust capture worker had no request
to prevent display sleep, which is a separate missing behavior. Capture
now holds `ES_DISPLAY_REQUIRED | ES_CONTINUOUS` for its lifetime, preserving
prior thread requirements and restoring them at teardown. The guard cannot
move between threads. The snapshot fixture uses the same guard.
[Windows documents the request and restoration semantics here](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-setthreadexecutionstate).
This is a capture-liveness correction, not evidence that display sleep caused
the reporter's access-loss events or remaining encoder-latency difference.

Two 230-second 1080p60 HEVC LAN runs crossed the existing 180-second display
timeout. Input activity was recorded; the last input occurred at connection
startup, with more than 230 seconds idle by each run's end. In the control,
all ten five-second samples after 180 seconds had no fresh capture claims;
the revised worker continued recording fresh claims in all ten. Sending
repeated pictures can hide this problem behind an apparently healthy FPS.

| Display request | Received / decoded pictures | Steady FPS | Host mean / p95 | Late arrival intervals |
| --- | ---: | ---: | ---: | ---: |
| Previous behavior | 13,743 / 13,743 | 60.139 | 2.290 / 2.6 ms | 51 |
| Held during capture | 13,838 / 13,838 | 60.561 | 2.270 / 2.6 ms | 44 |

Both runs decoded nonzero audio with zero codec failures. Both still needed
two DDX restarts during startup and initially received some blank pictures;
the awake request does not resolve that startup issue. This single long pair
validates continued fresh claims in this idle environment, not a general
latency improvement. Reports: `lan-power-results.json` and each run's
`capture-freshness.json`. The native power-state test separately checks prior
requirements, nested guards and restoration.

Every hardware-decoded picture is read back before checking exact geometry,
bit depth, HDR signalling and pixel contrast. Audio validation requires a
decoded test tone. A 1080p60 HEVC run decoded 1,203/1,203 pictures, sustained
60.597 FPS after warmup and had zero intervals above 1.5 frame periods.
The 4K60 HEVC run decoded 538/538 received pictures without errors, but the
NUC's decoder plus readback averaged 35.023 ms and delivered only 27.588 FPS.
That run is **not a performance pass**, despite correct pictures and audio.
The receiver now supports `BUTTERPOLLO_TEST_MIN_FPS` to fail such runs directly.
The initial separately named comparison binary was unreachable from the NUC;
it produced no stream and is excluded. Subsequent comparisons used the same
test executable path and saved each binary's hash.

Final 18-second runs of the retained changes passed the minimum-rate gate,
exact dimensions, visible pixel contrast on every decoded picture, and the
test tone. The source remained the existing SDR desktop; the HDR row checks
SDR-to-HDR conversion and HDR signalling, not native HDR capture.

| Backend / codec | Stream size | Received and decoded | Steady FPS | Decode failures |
| --- | --- | ---: | ---: | ---: |
| DDX / H.264 | 1920x1080 | 1,082 | 60.591 | 0 |
| DDX / HEVC Main10 HDR | 1280x720 | 1,082 | 60.582 | 0 |
| DDX / AV1 | 1280x720 | 1,083 | 60.614 | 0 |
| WGC / HEVC | 1920x1080 | 1,088 | 60.601 | 0 |

AV1 used software decoding; the other rows used Intel VAAPI plus readback.
The WGC row checks the actual opened backend, so fallback cannot pass it as
a WGC result. Reports are in `lan-final-results.json`. That earlier workspace
run passed 198 tests, including 18 native checks, with the desktop active.
The earlier inactive-desktop DDX failures remain recorded; a later pass does
not close that condition. The excluded AV1 geometry check independently
fails all twelve requested SDR/HDR/alignment combinations. NVIDIA execution
still needs NVIDIA hardware.

The Windows independent receiver also passed 1280x720 HEVC (718/718 pictures,
60.649 steady FPS), and correctly rejected an intentionally impossible
1,000 FPS requirement while still decoding 718 pictures without codec errors.
The final rebuilt host passed a local 3840x2160 HEVC / 60 FPS / 50 Mbps stream:
985/985 received pictures decoded, 60.585 steady FPS, 6.269 ms mean / 6.6 ms
p95 host time, and nonzero audio. The whole-run rate was only 54.56 FPS because
startup recovery consumed part of the 18-second run; 200 initial pictures had
no sampled luma contrast. Keep that startup defect visible. This source was
the existing 5120x1440 SDR desktop scaled to 4K, not native 4K motion capture.
Reports: `local-final-hevc`, `local-rate-gate-negative`, `local-final-4k-hevc`.

Build `tests/build-moonlight-client.sh ARTIFACT_DIRECTORY MOONLIGHT_SOURCE`
on Linux with CMake, C/C++ compilers and OpenSSL, FFmpeg and Opus development
packages. Use `BUTTERPOLLO_TEST_HOST` and `BUTTERPOLLO_TEST_PORT` with
`tests/interop.py` against a test-owned host profile. Optional variables are
`BUTTERPOLLO_TEST_HW_DECODER=vaapi`,
`BUTTERPOLLO_TEST_HW_DEVICE=/dev/dri/renderD128`,
`BUTTERPOLLO_TEST_REQUIRE_PICTURE=1`, `BUTTERPOLLO_TEST_AUDIO_TONE=1`,
`BUTTERPOLLO_TEST_WARMUP_SECONDS=5` and `BUTTERPOLLO_TEST_MIN_FPS=58.2`
for a 60 FPS run. The profile must repeat static pictures at the requested
rate, or a static desktop's deliberately reduced rate will fail this gate.
Remote clocks are independent, so remote measurements do not subtract the
sender's QPC timestamps or claim source-to-display latency.

The remaining RX 9070 XT acceptance comparison needs that GPU and client:
keep the same 3840x2160 / 60 FPS HEVC profile, bitrate, range and game scene
on both hosts; record the actual AMD driver version, not just "latest".
Compare idle and loaded runs in alternating order after warmup. On Rust,
compare compute conversion enabled and disabled without changing quality.
Repeat the same client once on Ethernet, then on Wi-Fi, to separate encoding
from delivery. Preserve capture-restart and UDP-drop logs alongside host
processing, frame age and delivery intervals. No setting from a faster local
GPU or a wired fixture closes that acceptance check by itself.

## October 5 WGC compute copy and capture startup follow-up

WGC can use the same fenced D3D12 copy and AMF conversion as DDX on supported
AMD devices with `wgc_compute_copy=true`. It remains opt-in after the production
repeat-rate comparison below found a loaded cadence tradeoff.
`gpu_compute_conversion=false` disables compute globally. WGC's graphics-copy
and polling defaults remain unchanged. Failure to initialize
compute preserves WGC on D3D11, and a texture-sharing failure now copies the
**same frame** on D3D11 instead of waiting for another desktop update.
A native regression test failed before that fallback fix and passes after it.

Before measuring latency, 120 WGC frames at idle and 120 under GPU load were
compared byte for byte with D3D11 readback of the same source frame. Every
comparison passed; each run contained 119 actual content changes. The
candidate was read first so reading the reference could not hide a missing
synchronization fence. A retained snapshot also remained unchanged after
capture teardown. These were SDR desktop captures on the local RX 7900 XT.

Eight initial 22-second HEVC streams used graphics/compute/compute/graphics
order both at idle and beside `gpu_load 45 1000 0 200`. The motion fixture
temporarily requested 60 Hz on the 5120×1440 SDR display, with a deterministic
128-pixel moving strip; the stream was 2560×720 at 60 FPS and 20 Mbps. These
initial runs forced `minimum_fps_target=60`. Scaling by exactly one
half preserved the timestamp barcode. The first five seconds were excluded.
All received pictures decoded correctly, with 100% barcode coverage, nonzero
test-tone audio, and no capture restart. Picture age uses the same PC's QPC
clock and includes decoding; it excludes scanout and input latency.

| GPU condition | WGC copy/conversion | Host mean | Decoded picture-age mean | Distinct pictures/second |
| --- | --- | ---: | ---: | ---: |
| Idle | Graphics | 2.361 ms | 31.432 ms | 46.758 |
| Idle | Compute | 2.210 ms | 31.835 ms | 50.601 |
| Loaded | Graphics | 8.557 ms | 49.052 ms | 49.704 |
| Loaded | Compute | 1.953 ms | 40.630 ms | 50.357 |

Values are the mean of two runs per cell. Under load the host mean fell by
77% and decoded picture age by 17%; both reversed-order runs agreed. Idle
picture age did not improve. Transport FPS was 60.425–60.645, which must not
be confused with distinct-picture FPS. The producer itself slowed to about
56.7 FPS with the graphics path and 58.8 FPS with compute under this load.
This is a controlled strip plus offscreen load, not a full game or native 4K.

An initial two-run pilot found fewer distinct frames with compute (50.386
versus 45.726 FPS). The full alternating comparison above did not reproduce
that ordering. The deliberately strict 50-distinct-FPS gate still failed on
both idle graphics runs and one idle compute run, and on both loaded graphics
runs. Those failures remain recorded; this does not establish perfect 60-FPS
freshness. Artifacts: `wgc-compute-verification`, `wgc-compute-abba`, and
`wgc-compute-abba2` under the October 5 artifact directory.

### Fixture correction and production repeat rate

The original motion probe changed physical refresh when passed a numeric
rate. It now paces animation with a timer and never changes display mode.
Read-only checks before, during and after its 60-FPS animation confirmed
5120×1440 at the saved 240 Hz; the three-second probe produced 60.322 FPS.
The longer fixture produced 7,501 pictures over 125.000 seconds. The Windows
QPC frequency was independently confirmed as 10 MHz. No restoration was
needed: current and saved display modes already agreed at the audit.

Four 18-second WGC compute runs, in 60/20/20/60 repeat-rate order, isolated
the difference between the fixture's forced repeats and production's existing
`minimum_fps_target=20`. With 60-FPS motion on the unchanged 240-Hz display,
forced repeats delivered 44.085/45.608 distinct FPS and 21.333/21.094 ms mean
picture age. Production's repeat rate delivered 59.925/60.002 distinct FPS
and 11.997/11.805 ms. All pictures decoded, barcode coverage was 100%, and
there were no capture restarts or late intervals. Thus the earlier repeated
pictures are partly a fixture artifact. The production repeat default was
already correct and remains unchanged. Report: `repeat-cadence`.

Final workspace/native validation passes 200 checks, including 20 native
checks, with the corrected moving fixture on an active desktop. The two
excluded checks are unavailable NVIDIA execution and the separately reproduced
AMD AV1 geometry failure. The new same-frame fallback regression and
WGC compute synchronization test both pass. Report: `capture-native-final`.

Eight further 18-second streams repeated graphics/compute/compute/graphics at
idle and under the same GPU load, using production's `minimum_fps_target=20`
and the corrected 60-FPS animation on the unchanged 240-Hz monitor. Each cell
below averages two runs; five warmup seconds are excluded. Every picture
decoded, with 100% barcode coverage, nonzero test-tone audio, no capture
restart and no compute fallback.

| GPU condition | WGC copy/conversion | Host mean | Decoded picture-age mean | Distinct FPS | Transport FPS |
| --- | --- | ---: | ---: | ---: | ---: |
| Idle | Graphics | 2.533 ms | 14.789 ms | 59.965 | 59.965 |
| Idle | Compute | 2.247 ms | 11.455 ms | 60.001 | 60.001 |
| Loaded | Graphics | 15.326 ms | 52.859 ms | 51.423 | 56.331 |
| Loaded | Compute | 1.986 ms | 34.603 ms | 49.955 | 55.714 |

The idle runs all pass. All four loaded runs fail the unchanged 58.2-FPS
transport and freshness gates; decoded-picture correctness alone does not
make them performance passes. Compute reduces loaded picture age by 35%,
but distinct delivery also falls by about 3%. Its default activation was
therefore reverted. The implementation remains available for explicit testing;
the final default preserves the previous graphics-copy path. This is the third
rejected default, alongside notifications and the shorter encoder-output wait.
Do not choose only the favorable host-latency counter. Report:
`wgc-compute-abba3/results.json`, host SHA-256
`405f29d01c6ded76877ac8e4c2a3b0917a3172cf2c761201e3d1dbfd414259a9`.
The final build differs by disabling the default and making the native test
opt in explicitly. The report's per-run configuration identifies each path.

The retained release build has SHA-256
`c8341cefcb1cdfc50f8038e735c412175571222a743da5f0ba6ec6a388366959`.
All workspace binaries build; all-target Clippy with warnings denied,
formatting and diff checks pass. Its final workspace/native run again passes
200 checks, including 20 native checks, with the same two exclusions above.
Report: `retained-native-final`.

Final wired Intel VAAPI/readback runs use 60-FPS motion, the production repeat
floor and 2560×720 at 60 FPS / 20 Mbps. Every received picture decodes with
exact geometry, 100% barcode coverage, nonzero audio and no capture restart.
HDR remains conversion from an SDR desktop; it is not native HDR capture.

| WGC setting / codec | Decoded pictures | Transport FPS | Distinct FPS | Decode failures |
| --- | ---: | ---: | ---: | ---: |
| Default / HEVC | 1,077 | 60.000 | 59.922 | 0 |
| Compute opt-in / HEVC | 1,069 | 60.011 | 60.011 | 0 |
| Compute opt-in / HEVC Main10 HDR | 1,070 | 60.000 | 60.000 | 0 |

Removing the motion fixture deliberately fails motion validation despite
724/724 decoded pictures, zero codec errors and 60.606 transport FPS. The
receiver therefore cannot count a successful static decode as this motion
test's success. No remote picture-age result is emitted because clocks are
independent. Report: `retained-lan-final`. Test-owned processes and receiver
containers are stopped; the installed rc.2 service and its profile are intact.

### Startup diagnosis

The old `ddx_arrival_probe` inserted a zero latency sample when it captured
nothing, incorrectly printing one frame. It now reports actual acquisitions
and separate presentation samples. `ddx_startup_probe` compares raw BGRA-first,
FP16-first and legacy duplication with plain snapshots, configured graphics
and compute capture, and WGC. It records metadata and sparse pixel ranges,
without saving desktop pictures or changing display modes.

With Windows explicitly reporting the display off, all six DDX paths returned
zero frames; WGC returned one cached image. Continuous display requests,
including a separate system-plus-display request, did not wake this already
off output. The active-desktop DDX check passes. This narrows the earlier
standalone failure to an off-display condition on this machine; the request
still prevents sleep during an already active stream, as measured above.

During a cold stream, a separate raw DDX observer saw the output switch from
5120×1440 BGRA to 3840×2160 FP16 and back, with an access-loss event at each
switch. The configuration had display mode/HDR changes disabled. The trace
identifies the transitions, but not what initiated them. A subsequent warm
stream had no restart. DDX now logs its actual dimensions, format and API at
every open, and preserves the modern-API failure when legacy fallback occurs.
No fixed startup delay or black-pixel heuristic was added. This observation
does not establish the cause of the RX 9070 XT reporter's restarts.

The Linux receiver also verifies the barcode's increasing frame sequence.
It deliberately omits absolute picture age because the remote clock is not
synchronized with Windows QPC.

## October 5 rc.3 selection and capture recovery

The user selected AMD WGC compute as the rc.3 default after reviewing the
production-repeat comparison above: about 35% lower loaded picture age with
about 3% fewer fresh pictures. The historical default-reversion record above
is retained. rc.3 enables `wgc_compute_copy` by default, retains the independent
off-switch and same-frame graphics fallback, and separates shared captures
when their compute settings differ. This does not change the backend selection
policy or make WGC available under SYSTEM.

Capture recovery previously slept 150 ms before every reopen, assuming all
streams had released the old GPU device. It now publishes a reset generation,
wakes consumers, and waits for acknowledgements after encoder, image and
filter teardown. Departing subscriptions stop blocking recovery; new
subscriptions own no old resources. Reopening starts once all owners release,
with 150 ms backoff only on failed open attempts and a 30-second deadline.
Tests cover multiple consumers, a departing/joining consumer, successive
resets and reset notifications arriving before or during a wait.

The tester can run a candidate when available, but the RX 9070 XT itself is
not remotely accessible. Keep the 4.7 versus 3.9 ms report open until matched
measurements arrive. The local fixture is RX 7900 XT and the LAN receiver is
a wired Intel NUC, not the reported Wi-Fi system. rc.3 artifacts are under
`C:\Users\ramaz\.codex\artifacts\butterpollo-rc3-20261005`.

## October 5 AV1 idle recheck

The rc.5 follow-up initially recorded 56.6–57.5 fresh AV1 pictures per second
at a 60 FPS target. A subsequent GPU check found Warhammer 3 using 92–99%
of the graphics engine. After the user closed it, the same released rc.5
executable (`9c3bb28832e68cada56d637142b2e8b2527b745c6e9c6224db29cdc5500806a8`)
passed the idle motion check: 59.843 fresh FPS, 60.004 steady received FPS,
and all 1,047 received frames decoded without errors. Steady decoded picture
age averaged 10.562 ms (p95 11.512 ms, p99 12.620 ms); host time averaged
1.907 ms. The earlier loaded measurements remain valid observations, but
are not evidence of an idle AV1 regression or an RTSS cap failure.

This local check used an RX 7900 XT, WGC's user helper with both compute
paths enabled, a 2560×1440 physical desktop at 120 Hz, a 60 FPS moving strip,
and a 1280×720 AV1 stream at 20 Mbps. Overlay process lifetime was not recorded
throughout this check. A low-priority, two-job build was also running. The updated harness
lets the source renderer finish and retains its frame timestamps alongside
the receiver's barcode and timing records. Picture age stops at decode,
excluding client scanout and input latency. Evidence is under
`C:\Users\ramaz\.codex\artifacts\butterpollo-monitor-av1-20261005\rc5-idle-av1-strip`.

The rc.6 runtime delivered 59.925 fresh FPS in the same idle strip check,
decoding all 1,116 received frames. A final process inventory found RTSS
stopped, so the earlier checks do not establish continuous overlay coverage.
With RTSS explicitly started and verified alive through a separate rc.6
check, all 1,054 received frames decoded; the steady window contained 752
distinct frames with no repeats or skips, at 60.001 fresh FPS. Decoded picture
age averaged 13.795 ms (p95 14.383 ms), and host time averaged 1.796 ms.
The existing RTSS global profile remained at 120/1 FPS with SyncLimiter=1;
the test requested no limiter changes. RTSS was stopped afterward to restore
its prior process state. Optional codec detection also completed with the
same capability flags as rc.5. These checks do not claim an rc.6 latency gain.

## October 5 rc.8 capture polling and RTSS audit

The WGC freshness predictor now caches its median and polling window when a
new frame updates the history. Polling no longer sorts two identical arrays
between frames. A regression test compares the cached decisions against the
previous calculation across stable and changing cadence, short intervals,
duplicate timestamps and capture resets.

Seven alternating local release-mode microbenchmarks each ran two million
polls, with one observation every eight polls and identical output checksums.
Median time fell from 84.79 ms to 14.32 ms (5.92 times faster for this small
calculation). This is a small CPU saving; it does not establish a whole-stream
latency or FPS improvement. Evidence and both implementations are under
`C:\Users\ramaz\.codex\artifacts\butterpollo-rtss-autostart-20261005\qa`.

The first rc.8 candidate was installed through the normal update transaction.
Settings, pairings and app-library hashes remained unchanged. With RTSS fully
closed, the service retried Windows error 740 using the signed-in administrator,
applied 2997/50 FPS, and restored 120/1 FPS on disconnect while Desktop remained
retained. A 120 FPS renderer measured 59.941 FPS during the cap and 119.996 FPS
afterward. WGC compute remained active and all 831 AV1 frames decoded. Warhammer
3 was running, so these are functional checks with a background game, not an
idle comparison. A preceding run recovered from two display-resolution changes;
its disrupted timing result was rejected.

Fault injection also reproduced a reconnect bug: after failed SDK restoration,
replacing a retained process owner with `None` killed RTSS during the next lease.
The owner now survives a successful recovery and reacquisition. A separate
fixture reproduced missed process detection for a directory ending in `\.`;
directory identity is now normalized before comparison.

## October 5 rc.8 idle physical-display investigation

After Warhammer closed, the desktop was 5120×1440 SDR at a configured 240 Hz.
These checks stream 2560×720 at 20 Mbps through WGC's user helper and compute,
with independent local Moonlight/FFmpeg decoding and source frame barcodes.
They are not directly comparable to the earlier 1280×720 checks on a smaller
desktop. No compiler runs alongside the motion checks. The source logs and
DXGI presentation statistics distinguish requested refresh, actual presented
frames and distinct pictures reaching the decoder.

An initial alternating AV1 strip comparison gave 59.978 and 60.002 fresh FPS
for the first rc.8 build, versus 60.005 and 54.415 for the audited build. Three
subsequent audited runs delivered 59.912–59.938 fresh FPS; HEVC and H.264 checks
gave 59.920 and 59.866. The failed run remains part of the evidence. A
full-screen pattern reproduced the shortfall in both builds: 50.092 and
49.437 fresh FPS, respectively. Direct WGC and graphics-copy comparisons also
failed the freshness gate, so this does not identify the timing-cache change
or compute copies as the cause. Source submission and displayed-present
counters confirmed approximately 60 FPS in the separate presentation probe.
The frame gaps appeared before encoding. Configured 240 Hz alone is not
evidence that every image was scanned out at 240 Hz or that VRR caused the gaps.

The C++ host sets WGC's minimum update interval to 1 ms; the Rust port had
left it at Windows' default. A [firsthand Windows capture report](https://github.com/robmikh/Win32CaptureSample/issues/82)
describes a similar ceiling near 60 FPS with values below 1 ms. That report
does not establish the untouched property's value on this PC; the native
measurement below later found 16 ms. The
[MinUpdateInterval property](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.minupdateinterval)
is available on newer Windows builds. Local Windows 11 build 26200 accepted
the setting. An unconditional experiment measured:

| WGC configuration and source | Received FPS | Fresh FPS | Host processing mean | Decoded picture age mean |
| --- | ---: | ---: | ---: | ---: |
| Windows default, full-screen 120 FPS | 59.758 | 59.758 | 1.884 ms | 17.805 ms |
| 1 ms interval, full-screen 120 FPS | 100.326 | 94.180 | 1.853 ms | 10.171 ms |
| 1 ms, compute disabled, full-screen 120 FPS | 98.832 | 91.705 | 2.161 ms | 11.167 ms |
| 1 ms interval, 60 FPS strip | 60.516 | 54.120 | 1.874 ms | 18.920 ms |

Every received frame decoded without errors in these cases, but none passed
the strict fresh-picture target. The 1 ms experiment also failed a full-screen
60 FPS check (43.951 fresh FPS). It is therefore not enabled globally. The
first rate-aware candidate requested it only when the negotiated stream rate
exceeded 60 FPS, preserved the previous 59.94/60 FPS behavior, and kept compute enabled. The effective
policy follows the user helper and capture-sharing key. An explicit
`wgc_high_rate_capture=true` or `false` overrides the automatic decision;
unsupported Windows versions retain their default. The improvement at 120
FPS removes an observed ceiling, not all capture loss.

The final rate-aware executable was then compared with its own override
disabled, avoiding a binary-version confound. At a 120 FPS request it delivered
90.598 fresh FPS with the automatic 1 ms request, versus 59.959 with the
override disabled. Decoded picture age averaged 14.640 versus 10.247 ms;
this pair establishes higher delivery rate, not a uniform latency gain.
At 60 FPS the unchanged default delivered 57.178 and 47.846 fresh FPS, and
the preceding audited executable then delivered 47.567 under the same strip
test. Forcing 1 ms at 60 FPS gave 55.417 fresh FPS; combining it with fixed
grid pacing gave 56.521, with higher picture age (25.701 and 26.533 ms).
Neither diagnostic passed the 58.2 fresh FPS acceptance gate, and neither
became a 60 FPS default. No failed run was removed to claim success.

The source's displayed-present counter remained near 60 FPS in the strip
checks, while its reported refresh-counter rate differed (approximately 240
in the first run and 100 in the later revised/old pair). These observations
motivate further Windows/display-timing investigation; they do not establish
actual panel scanout rate or a confirmed VRR cause. The physical display mode,
driver settings and the user's applications were not changed by these tests.

H.264 and HEVC HDR output from the SDR source reproduced the shortfall
(48.893 and 46.342 fresh FPS). Both decoded every received frame with correct
geometry and nonzero decoded audio. Disabling the optional DXGI statistics
in the source renderer still gave 48.528 fresh AV1 FPS, so removing that
instrumentation did not resolve this occurrence. The ordinary workspace
suite passed 206 tests (23 hardware/network tests ignored), all-target Clippy
passed with warnings denied, and the release workspace built successfully.
These code checks and successful decoding do not override the failed
fresh-picture acceptance results.

During diagnosis, an allocator-reused image address exposed a stale
`first_seen` trace entry. Clearing it after submission fixes the diagnostic;
it does not change pacing. `motion_probe` now optionally records DXGI
presentation statistics with `BUTTERPOLLO_TEST_PRESENT_STATS=1`.
`wgc_arrival_probe` adds an `unregistered` polling mode for comparison with
registered notifications; no production notification policy was changed.

All original runs, including failed comparisons, are preserved in
`C:\Users\ramaz\.codex\artifacts\butterpollo-monitor-av1-20261005`.
The consolidated `CAPTURE_QA.json` is under
`C:\Users\ramaz\.codex\artifacts\butterpollo-rtss-autostart-20261005\qa`.
Full-screen 120 fresh FPS and the reporter's RX 9070 XT/Wi-Fi comparison
remain open. The final candidate also still needs an installed-service
check; a prior installer launch was rejected before execution by tool policy.

## October 5 rc.8 installer firewall repair

The two manual installation attempts at 22:19–22:20 Berlin time copied the
rate-aware host successfully, then failed when `netsh` rejected the
`\\?\C:\Program Files\ButterpolloRust\butterpollo.exe` application path.
The preceding automatic updater had persisted its canonical filesystem path
in the Windows installation entry; the manual installer reused that path.
This failure happened after the motion benchmarks and cannot explain their
loopback fresh-frame loss. An existing private/domain, local-subnet allowance
remained, and the wired receiver at 192.168.4.10 could still reach serverinfo.

Setup now converts conventional drive/UNC paths at Windows command and
registration boundaries while retaining canonical filesystem identity checks
inside the updater. Unsupported namespaces and names that would change meaning
are rejected before firewall operations. Existing Butterpollo rules are updated
in place; a missing rule is added. No existing rule is deleted on a failed
replacement, and legacy-rule cleanup follows a successful Butterpollo rule.
Regression tests cover canonical paths with spaces and Unicode, UNC paths,
real-file identity, existing/fresh rules, and failure without deletion.

## October 5 rc.8 explicit WGC interval correction

A native background probe isolated WGC from the encoder, texture copies and
network. Each case opened a new capture session, left the first second out,
and read `MinUpdateInterval` plus frame timestamps. Warhammer remained open;
the probe created no visible window, changed no display/input/RTSS settings,
and saved no pictures. On Windows build 26200, the untouched property returned
160,000 100-ns units: **16 ms**, not zero.

| Fresh session setting | Property value | Native capture updates/sec |
| --- | ---: | ---: |
| Untouched, first control | 16 ms | 55.384 |
| Explicit zero | 0 ms | 216.968 |
| Explicit 1 ms | 1 ms | 216.569 |
| Untouched, final control | 16 ms | 57.493 |

A separate comparison rebuilt the previous capture implementation from
`ba784a5dba6b063605f22dc1691028ae2e76307e` and the corrected source with the
same build command. Two alternating three-second checks of direct WGC plus
the production GPU-copy path measured 59.489/58.000 capture updates/sec before
and 246.923/234.999 after, excluding each first second. These short checks ran
against the existing game picture without a test window or encoder. They
confirm removal of the capture ceiling, not a whole-stream FPS improvement.
The prior saved probe lacked source provenance and was excluded as a baseline;
its measurements remain in the artifacts. Exact-source results and binary
hashes are in `qa\explicit-zero-exact-source-comparison\results.json`.

This identifies a throttle before encoding and explains why simply omitting
the API call can miss a 60 FPS target. The low-rate path now explicitly sets
zero; the higher-rate path retains its 1 ms request. Both direct capture and
the user helper use the same constructor. Unsupported API versions keep the
existing nonfatal fallback. Capture sharing and compute-copy policy are
unchanged. `wgc_high_rate_capture=false` now selects explicit zero, rather than
leaving the property untouched.

The native counts do not establish game render FPS, distinct decoded picture
FPS, input latency, or zero performance impact on the game. Earlier failed
barcode checks remain failed historical evidence. A controlled motion check
of this revised executable is still required before claiming the full
smoothness acceptance gate passes. The probe and timestamp records are under
`C:\Users\ramaz\.codex\artifacts\butterpollo-rtss-autostart-20261005\qa\native-cadence-fresh-session.json`.

Two further eight-second background AV1 checks used the real user-helper and
compute path at 2560x720, 60 FPS and 20 Mbps, with an independent local decoder.
They captured the existing game picture without a test window, audio tone,
input, display changes or frame limiter. After the five-second warmup, the
short measured windows were 2.102/2.146 seconds. The previous build delivered
58.039 FPS and missed the unchanged 58.2 FPS delivery gate; the corrected build
delivered 60.585 FPS and passed. All 412/435 received frames decoded, with zero
errors. Intervals over 1.5 frame periods fell from one to zero in those windows.
The final host samples reported mean source-frame ages of 0.681/3.419 ms and
mean present-to-send times of 2.526/5.252 ms; this is not evidence of reduced
latency. The game was uncontrolled and the sample was brief. Distinct-picture
cadence and sustained latency still require the controlled motion check.
The raw cases are `butterpollo-monitor-av1-20261005\wgc-zero-background-before`
and `wgc-zero-background-after`; the comparison is preserved separately from
the failed historical motion checks.

Verification of this revision: 208 ordinary tests passed, 24 environment
tests were skipped by default, the new native WGC interval regression passed
when selected explicitly, Clippy passed with warnings denied, and the release
workspace built successfully. The running game's process/start time and all
three installed host profile hashes were unchanged after the background tests.
The full smoothness acceptance gate remains pending. After these measurements,
the user installed the final candidate. Read-only checks confirmed the tested
host hash, service version 2.0.0-rc.8, successful setup completion, a conventional
installation path and an enabled inbound firewall allowance for that executable.
This confirms installation and service startup; it does not substitute for a
complete limiter lifecycle, controlled motion check or automatic-update handoff.

## October 6 rc.9 WGC-first test candidate

An unset, blank or Automatic capture setting now resolves to WGC before the
stream opens capture. This uses the existing signed-in user helper when running
as a service, together with the existing Desktop Duplication fallback at startup
and recovery. Explicit capture choices retain their previous normalized value.
The initial candidate preserved compute-copy defaults, capture intervals,
stream rates, display refresh policy and frame limiting. The follow-up below
removes its automatic high-rate WGC interval limit.

The selection regression matrix covers physical/virtual displays, fractional
and integer frame rates, VRR, frame generation and legacy capture aliases.
The fallback regression starts from the default policy and injects a WGC open
failure, verifying that Desktop Duplication is attempted next. These are policy
and error-path checks, not new hardware or performance measurements.

Initial-candidate verification: 210 ordinary tests passed; the default run skipped 23 hardware
checks and one network check. Formatting, Clippy with warnings denied, the
release workspace build, and the web build passed. The web type check reported
zero errors and warnings. These checks ran without starting capture or changing
the installed service.

After the game closed and the user made the screen available, eight controlled
full-screen motion cases compared Automatic WGC/helper/compute with explicit
DDX on the unchanged 5120x1440, 240 Hz display. Each independently decoded
AV1 stream requested 2560x720 at 20 Mbps for 20 seconds, with a five-second
warmup and 14.3-14.8 measured seconds. The fixture rendered at the requested
stream rate; its recorded cadence was approximately 60 or 120 FPS. The test
used isolated loopback profiles, without display/RTSS changes or an audio tone.

| Stream target | Capture | Delivered FPS, two runs | Distinct-picture FPS, two runs |
| --- | --- | --- | --- |
| 60 | Automatic WGC | 60.602 / 60.586 | 56.203 / 57.495 |
| 60 | DDX | 60.594 / 60.591 | 54.454 / 53.844 |
| 120 | Automatic WGC, 1 ms | 107.272 / 109.162 | 100.344 / 101.478 |
| 120 | DDX | 121.196 / 121.206 | 113.908 / 113.850 |

All frames decoded without errors, all barcodes were readable, and the expected
backend opened without capture recovery or compute fallback. None of these
eight cases met the unchanged 97% distinct-picture gate. Fixed-grid WGC pacing
made 120 FPS delivery worse (99.271 FPS, 93.097 distinct); direct WGC without
the helper also failed (106.636 delivered, 100.157 distinct). These results do
not justify changing the default pacing mode or blaming the user helper.

Two follow-ups changed only the isolated profile's WGC interval override to
explicit zero. The helper/compute path then delivered 121.213/121.190 FPS,
with no intervals above 1.5 frame periods, versus 232/227 long intervals in
the default 1 ms cases. Distinct-picture rates rose to 116.274/116.663 FPS:
one narrowly missed the unchanged 116.4 FPS gate, and one passed. Mean decoded
picture age was 9.301/9.255 ms versus 8.983/9.230 ms with 1 ms, so this is a
delivery improvement, not evidence of reduced picture age. Mean host processing
remained about 1.85 ms. The revised default uses explicit zero at every rate;
an explicit wgc_high_rate_capture=true retains the 1 ms diagnostic option.

The candidate remains unpublished and is not installed over the running service.
The rebuilt executable was then checked twice without an interval override:
121.186/121.193 delivered FPS, no long intervals and no decoding errors.
Distinct-picture rates were 116.289/116.595 FPS, so one still narrowly missed
116.4 while the other passed. Mean decoded picture age was 9.084/9.339 ms;
mean host processing was 1.864/1.865 ms. The test binary's SHA-256 is
f4a04b4634a690a86727b5b9c9a6da27ff558e275251fae2fc2a23400ef4c82b.
The revised source passed all 210 ordinary tests, with the same 24 environment
checks skipped, plus formatting, Clippy with warnings denied and the release
workspace build. Frontend sources did not change after their successful checks.
Distinct-picture acceptance is not yet consistent; the 60 FPS gate, longer
runs, reconnect/secure-desktop recovery, HDR and the reporter's RX 9070 XT/Wi-Fi
case remain open. No failed case is relabelled as a pass. Logs, renderer reports,
independent per-frame CSVs and comparison summaries are retained under
D:\\CodexArtifacts\\butterpollo-wgc-default-20261006 alongside the initial
candidate package and its exact-source hashes.

## Limits

This machine validates AMD AMF. Native NVENC now calls the installed NVIDIA driver directly, supports reviewed API versions 11.0–13.0, reference frame invalidation, D3D11 4:2:0/8-bit 4:4:4 and GPU-only CUDA interop for ten-bit 4:4:4. Seven mock-driver tests exercise compatibility, asynchronous ownership, timeout teardown, metadata lifetime, loss recovery and bitrate changes; NVIDIA execution/performance still needs NVIDIA hardware. QSV has native D3D11 imports, and TrueHDR has a shared-device GPU path; these need Intel/NVIDIA hardware respectively. Unsupported native formats, PyroWave and software encoding use CPU compatibility paths. The wired LAN checks above do not establish Wi-Fi performance, multiple concurrent 4K sessions, dynamic game content, native 4K capture or end-to-end input/display latency. The GPU texture pools and native encoder queues are bounded to eight retained frames; capacity runs may intentionally keep those queues occupied. [PARITY.md](PARITY.md) separates implemented features from native validation.

The initial C++ comparison was blocked by an older `butter.2` executable whose startup performed global virtual-display recovery despite the isolated configuration; it was stopped before streaming tests. The October 4 comparison against pinned Vibepollo 2.0 above supersedes that initial limitation. The controlled FEC comparison uses the exact baseline sources without starting the C++ host.
