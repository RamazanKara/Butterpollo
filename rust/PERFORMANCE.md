# Rust performance evidence

[Documentation](../docs/README.md) · [Performance overview](../docs/performance.md) · [Compatibility](PARITY.md)

This is the dated measurement record. For a guided comparison, start with the [performance overview](../docs/performance.md). Each section below describes the source, version and fixture used at that time.

| Find a result | Recorded evidence |
| --- | --- |
| Radeon compute off/on | [1080p60 comparison](#1080p-at-60-fps) |
| Other Sunshine hosts | [Vibepollo 2.0 baseline](#against-vibepollo-20) |
| WGC capture and pacing | [Guarded comparisons](#guarded-ab-stress-and-compatibility-checks) |
| Native HEVC / AV1 HDR | [Final rc.10 pixels and repeat runs](#final-native-virtual-hdr-pixels-excluding-physical-panel-calibration) |
| PyroWave HDR 4:4:4 | [Transport and decoding](#vibepollo-20-pyrowave-transport) |
| CPU error correction | [C++ FEC comparison](#controlled-comparison-with-the-original-c-fec) |
| Reproduction and scope | [Probes](#reproduce-on-another-machine) · [Hardware limits](#limits) |

## Initial measurement environment

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

These October 2 session API and five-second host log samples reported capture-to-packetization host processing separately from encoder latency. The current implementation instead writes encoder-claim-to-pre-packetization processing into Moonlight's frame header and reports capture age separately. Neither definition includes client scanout or input latency; compare only measurements using the same definition. Full-stream idle-desktop comparisons are recorded below; a dynamic game and the customer's client remain separate acceptance checks.

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
respectively, in both SDR/HDR with tested alignment modes 3 (`NO_RESTRICTIONS`)
and 4 (`8X2_ONLY`). Requested component dimensions and alignment read back correctly, but
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
black and no lost saturation in these reference frames. This validates the
tested host conversion and encoding path for this content and setup. A
washed-out report on another stream still requires checking that stream's
captured pixels, encoded colours and client presentation; these fixtures
do not isolate the cause of an unmeasured case.

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

## October 7: PyroWave recovery feedback

The rc.19 report describes about 3.3 `idr_requests` per second at several
PyroWave bitrates. That counter alone cannot identify the incoming message:
it included explicit IDR requests, reference invalidations that fell back to
IDR, and host requests such as encoder startup. The exact reporting client
build and its control trace were not available for this investigation.

The pinned [Nonary control implementation](https://github.com/Nonary/moonlight-common-c/blob/d6a11bc685b41037b352a96f29d08276fe5359ba/src/ControlStream.c)
uses `0x0301` for reference invalidation and `0x0302` for an explicit IDR
request. Its older `0x0201` loss report runs every 50 ms; modern Sunshine
connections instead send `0x0200` pings every 100 ms and queued `0x5502` FEC
status reports. Butterpollo ignores those statistics messages for recovery.
This client's decoder-capability check enables RFI only for H.264, HEVC and
AV1; PyroWave transport loss requests an IDR instead. Neither this client
transport nor the host has a 300 ms recovery timer.
The [Qt PyroWave decoder](https://github.com/Nonary/moonlight-qt/blob/43225b52c934174736123f894580c0decbe4bec2/app/streaming/video/ffmpeg.cpp)
also returns `DR_OK` after rejecting a PyroWave picture: the next independent
picture replaces it. The reported rate is therefore not established as
expected behavior or as network loss; its precise trigger remains open.

There was a host-side feedback bug. PyroWave reference invalidation reached
an unsupported encoder method, requested an IDR and increased `idr_requests`.
Both pending invalidation and the IDR latch could bypass the capture cadence,
including encoding a repeated picture early. The PyroWave encoder itself
ignores the IDR argument, so it never produces a larger recovery keyframe.
Critical FEC uses the record layout and configured percentage; detail FEC uses
frame cadence, record stability and the available wire budget. Neither reads
these control messages. Extra early pictures could indirectly change cadence
and bandwidth, but feedback does not directly increase parity.

PyroWave feedback now leaves the cadence latches alone. Reference invalidations
have their own `reference_invalidations` counter in session statistics and
the periodic timing log, and cannot become PyroWave IDR requests. Explicit
`0x0302` requests remain visible in `idr_requests`, including for PyroWave;
the existing host startup/fallback accounting is retained. This follows
[Vibepollo 2.0's handlers](https://github.com/Nonary/vibepollo/blob/8a8c4b03a/src/stream.cpp),
which separately count IDR/RFI messages and ignore both for PyroWave encoding.
Unit tests cover valid and malformed PyroWave invalidations, explicit requests,
and unchanged H.264/HEVC/AV1 range merging and IDR fallback.

The framing comparison found the same eight-byte short header, IDR frame type
2, exact final payload length, critical-prefix packet count, `0x80` record-start
flag, RTP/24-bit stream sequence progression and up-to-four-block FEC layout.
The first FEC block protects the critical records at 20% by default, with at
least two parity packets, when it fits the 255-shard Reed-Solomon limit;
otherwise the planner uses unprotected blocks. The critical-packet count is not a required ratio of
the whole frame; low-bitrate pictures can devote a larger share to coarse
records. No framing or FEC changes were justified by the comparison.

Before changing the host, the independent pinned receiver streamed record-framed
PyroWave over loopback at 2560×720/60, with 10/200/800 Mbps requested and
9,308/198,988/798,988 kbps negotiated. It decoded all 682/683/715 received
pictures, with zero partial frames and zero decode failures. Each run kept
`idr_requests=1` from startup through teardown; the 3.3-per-second symptom did
not reproduce. These are transport/vendor-decoder checks, not validation of
the reporting client's renderer. The release e2e wrapper correctly reports
missing motion/audio-continuity measurements for this PyroWave receiver; its
interoperability check passes, but it is not a full release-e2e pass.

### Radeon LTR recommendation (evaluation only)

Keep `amd_ltr_frames=0` as the default for now. AMF supports explicit LTR
selection for [HEVC](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_Encode_HEVC_API.md#228-ltr-properties)
and [AV1](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_Encode_AV1_API.md#227-ltr-properties).
Butterpollo can already recover a valid reference-invalidation range from an
older retained anchor, reducing the need for an IDR. The existing strict
FFmpeg omission fixtures above decoded all 56 retained pictures and exercised
two LTR recoveries each for HEVC and AV1. That is correctness evidence, not a
measured Wi-Fi or RDNA4 improvement.

LTR cannot replace explicit decoder-reset IDRs, a lost initial anchor, invalid
ranges or recovery after all usable anchors are gone. It also needs a client
decoder that supports reference invalidation and enough negotiated references
for the anchors plus the rolling reference. The current policy respects that
budget, disables LTR with intra refresh, checks driver capabilities/readback,
and falls back on rejected surface properties. Anchor selection relies on the
reported loss range; it is not proof that every hardware client retained it.

For a supported Radeon/client pair, opt-in LTR is worth testing with controlled
loss, delayed feedback and decoder resets while measuring recovery bytes,
latency and sustained decoding. Test RDNA4 separately: the documented
[RX 9000 H.264/HEVC freeze report](https://github.com/AlkaidLab/foundation-sunshine/issues/666)
concerns forced low-latency/input-queue settings, not proof of an LTR defect,
but it makes extrapolating RX 7900 XT results to all Radeon drivers unsafe.
Do not enable LTR globally to mask congestion. No encoder policy or AMF code
was changed by this investigation.

## October 7: Wi-Fi and unknown-route pacing

H.264, HEVC and AV1 now default to twice the negotiated encoder bitrate on a
wireless or unknown host route, bounded to 1–800 Mbps. Physical Ethernet keeps
800 Mbps, capped at 80% of its reported link speed. Loopback keeps 800 Mbps.
The first frame resolves the route before selecting its rate; subsequent
lookups retain the two-second refresh. A wired host cannot infer the capacity
of a wireless client behind an access point, so that case still needs a
bitrate reduction or an explicit `pacing_max_bitrate_kbps` override.

Positive overrides retain their existing 110% stream-bitrate floor and 80%
physical-link cap. PyroWave's automatic sender is unchanged: 95% of a known
Ethernet link, or per-frame wire demand and its bitrate floor otherwise.
Its independent-frame bandwidth must not inherit an 800 Mbps ceiling or a
conventional-codec multiplier. The rc.19 two-frame encoder gate is unchanged.

### Measured loopback cost

These runs used the release `rust/release/e2e.py` fixture, WGC and an RX 7900 XT
with driver `32.0.31041.1004`. The installed service log was checked before
each run and showed no active stream. The portable host used its own package,
configuration and ports. Other agents could still use the GPU, so settings
and builds were alternated, with two runs per case; ranges below are the two
observations, not confidence intervals. Baseline was commit `4ea3b058`.
Raw logs, receiver results and sampled statistics are in this worktree's
`target/network/measure`; `target/network/measurements.json` contains the
summary. No physical Wi-Fi path was measured.

The first six runs compared pacing settings on the baseline executable at
2560×720/60, with 60,000 kbps requested and 46,988 kbps at the encoder.
Overrides of 93,976 and 70,482 kbps modelled 2× and 1.5× on loopback.
The moving-strip scene actually sent only 9.8–11.2 Mbps of UDP video, including
FEC; these timings are not a sustained 60 Mbps traffic test. Send cost below
is the mean of sampled `present_to_send_mean_ms - frame_age_mean_ms -
host_processing_mean_ms` after three seconds, including packetization and
send waits. It is not encode time or client latency.

| Baseline pacing | Send cost per frame | Receiver steady FPS | Release e2e results |
| --- | ---: | ---: | --- |
| 800 Mbps | 0.16–0.17 ms | 60.57–60.70 | Both passed |
| 2× encoder bitrate | 0.65–0.81 ms | 60.58–60.77 | One passed; one audio-continuity failure |
| 1.5× encoder bitrate | 1.84–1.90 ms | 60.57–60.60 | One passed; one audio-continuity failure |

All six decoded every delivered picture without a decode failure. The two
audio failures are retained here; their cause was not established. The lower
send cost and larger FEC/scheduling margin favour 2× over 1.5×.

A separate alternating baseline batch at 5120×1440/240, 150,000 kbps requested
and 118,988 kbps encoded, exposed the cost of applying 2× unnecessarily to a
fast route. Host throughput fell from 215.61–216.01 FPS at 800 Mbps to
206.43–207.63 FPS at 237,976 kbps, about 4%. This is why loopback retains its
old default. The software receiver could not sustain the requested rate, so
these runs failed the receiver cadence/motion gates; they are host throughput
measurements, not successful 240 FPS playback.

The final baseline/candidate/baseline/candidate batches gave:

| Workload | Baseline host FPS | Candidate host FPS | Baseline / candidate send cost |
| --- | ---: | ---: | --- |
| HEVC 2560×720/60, 800 Mbps versus 2× override | 60.54–60.60 | 60.59–60.61 | 0.12–0.14 / 0.52–0.61 ms |
| HEVC 5120×1440/240, automatic loopback | 216.82–218.14 | 217.73–218.05 | 0.32–0.43 / 0.32–0.33 ms |
| PyroWave 2560×720/120, 800 Mbps requested, automatic | 87.97–88.02 | 88.10–91.50 | 11.26–11.36 / 10.87–11.26 ms |

All four normal HEVC runs passed the complete release fixture, decoded all
927–938 pictures, and retained continuous audio. Receiver steady FPS was
60.58–60.61; arrival-interval p99 was 18.47–22.95 ms. Candidate mean
presentation-to-send time was 3.43–3.60 ms versus baseline 3.10–3.73 ms.
The saturated HEVC runs again failed receiver cadence/motion gates: the
software receiver achieved only 36.05–55.97 FPS and requested recovery frames.
All had continuous audio and zero decode failures among delivered pictures.
There was no host throughput regression in these loopback observations.

The PyroWave receiver decoded all 4,369 pictures across the four runs, with
zero partial frames, zero decode failures and `idr_requests=1` throughout;
candidate `reference_invalidations` stayed zero. Actual UDP video was
581–611 Mbps on the candidate versus 586–592 Mbps on baseline. These runs
passed the independent receiver's interoperability check, but the release
wrapper reports its missing motion/audio metrics, as described above. They
show no high-bitrate slowdown in this workload; they do not validate 120 FPS
playback or explain the reporting client's periodic feedback.

### Estimated burst size

For a hypothetical 60 Mbps encoded stream at 60 FPS, each average picture is
125,000 bytes. With a 1,392-byte packet setting, encryption, IPv4 and 20% FEC,
that is 91 data packets plus 19 parity packets: 165,660 modelled wire bytes.
Serialization takes about 11.04 ms at 120 Mbps (2×), 14.73 ms at 90 Mbps
(1.5×), or 1.66 ms at 800 Mbps, within a 16.67 ms frame period. This explains
why 1.5× leaves little allowance for larger pictures or scheduling delays.

With the default 64 KiB batch limit, the sender's two-millisecond budget
reduces an initial encrypted UDP burst from 45 packets / 64,800 bytes at
800 Mbps to 20 packets / 28,800 bytes at 120 Mbps. These are calculations
using the existing packet and Ethernet-overhead model, not measured Wi-Fi
airtime or evidence that a particular access point will avoid audio loss.

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

## October 6 offline latency audit

Analysis of the same final two 120 FPS WGC runs, without new capture or changes
to the installed host, separates the latency counter from picture age:

| Stage or outcome | Run A / Run B |
| --- | --- |
| Host processing (claim to before packetization) | 1.864 / 1.865 ms mean |
| Capture timestamp to encoder claim | 1.378 / 1.548 ms mean; 4.616 / 5.119 ms p95 |
| Independent software decoder callback | 3.254 / 3.371 ms mean |
| Render timestamp to decoded picture | 9.084 / 9.339 ms mean |
| Distinct-picture rate | 116.289 / 116.595 FPS |

The software decoder cost is specific to the test fixture, not Moonlight Qt's
hardware decoder. These values exclude client display scanout and input latency.
At 60 FPS the WGC frame-claim wait averages roughly 4 ms, making frame selection
and pacing a stronger next investigation than further encoder preset changes.
Shortening that wait is not automatically beneficial: selecting an older picture
can worsen picture age even while lowering a host counter.

All fourteen source fixtures recorded render submissions, but none enabled the
existing optional DXGI actual-presentation statistics. The next comparison should
enable those statistics and trace picture identity through capture and encoding
before attributing the remaining repeated pictures to either stage. Compute
versus graphics copies also need a comparison using the corrected zero interval.
The exact workload and distinct-picture acceptance threshold should stay fixed.

Static review found two additional hypotheses, with no measured gain yet: drain
the two-buffer native WGC pool to the newest frame before copying after a stall;
and compare the helper's process scheduling settings with the host's streaming
scope under load. The helper already uses MMCSS and a high-resolution timer.
No capture, pacing, encoder or scheduling defaults were changed by this audit.

## October 6 capture phase and actual-presentation investigation

The follow-up measurements use a different desktop configuration: 2560x1440 at
120 Hz, streaming 1280x720 AV1 at 20 Mbps on the local RX 7900 XT. The display
was already in this mode when testing resumed. These ages must not be compared
directly with the earlier 5120x1440/240 Hz, 2560x720 results. Each controlled case
requested 20 seconds with a five-second warmup, WGC through the signed-in user
helper, compute conversion and the explicit zero capture interval. The receiver
independently decodes pictures in software on loopback; this is not a Moonlight
hardware-decoder or input-to-display measurement.

The fixture now records actual DXGI presentation statistics as well as render
submissions. Healthy source windows maintained the intended presentation rate
and a constant one-frame in-flight count. An additional presentation-to-decoder
estimate uses
`SyncQPCTime + (PresentRefreshCount - SyncRefreshCount) * QPCfrequency / refreshHz`
to estimate display presentation, then subtracts render-to-presentation delay
from the original decoded picture age. The fixture's present-call and picture
IDs are checked against the actual presentation counters before using that
mapping. The [DXGI statistics definitions](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/ns-dxgi-dxgi_frame_statistics)
and [present-call counter](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-getlastpresentcount)
are distinct. This estimate supplements the original render-age metric; it does
not replace raw acceptance or include scanout, network Wi-Fi delay or input.

Trace-level capture records identify each publication, including publications
replaced before an encoder claim. Current instrumentation scopes IDs by source
and records generation, capture/publication timestamps and matching claims.
Weak identity records do not keep image textures alive, and tracing disabled
avoids populating those records. The saved single-source experiments correlate
claims with decoded wire frames using frame order and a checked timing fit;
they do not identify the pixel contents of unclaimed publications.

Actual presentation was healthy in the initial four compute/graphics cases,
while the later 60 FPS diagnostic cases published roughly 70-78 capture updates
per second. Every decoded repeat in those cases had a new capture publication
ID. Thus a new captured image could contain the same source picture and consume
pacing credit immediately before the next useful update. In the baseline trace,
252 publications were replaced before claim, 85 claimed pictures repeated and
76 source picture IDs were absent. Disabling prediction reduced repeats to eight
and omitted source IDs to zero, but increased estimated presentation-to-decoder
age from 12.334 to 15.438 ms. A fixed-grid comparison likewise reached 60 distinct
FPS but increased that age to 19.275 ms. These alternatives do not justify a
global pacing change.

The opt-in `frame_pacing_source_phase=true` diagnostic learns a dominant phase
from capture timestamps observed by the streaming thread. It does not consume
fixture barcodes, render IDs or DXGI statistics. It requires a concentrated
rolling history and a recent source interval close to the stream interval;
irregular, faster and slower sources fall back to ordinary arrival pacing.
Recovery, timestamp discontinuities and persistent phase shifts reset history.
Anticipation remains bounded rather than adding a full source period. The
ordinary arrival policy and `frame_pacing_source_phase=false` remained defaults
during the initial diagnostics below.

Nine cases used the same runtime SHA-256
`1551d2f33a927023c5319f45d770cb07c3962c0ebed5374685f1066be6306c4a`.
All decoded without errors. Source and stream rates are independent in the last
four cases. Same-rate gates remain 58.2 distinct FPS at 60 and 116.4 at 120.
The explicitly planned 120-to-60 and 30-to-60 workloads use
`min(source FPS, stream FPS) * 97%`, recorded as receiver thresholds 58.2 and
29.1 respectively. A 30 FPS source cannot supply 60 distinct pictures. These
new mixed-rate cases have their own declared workload; historical failures and
same-rate gates remain unchanged.

| Case | Source to stream FPS | Delivered FPS | Distinct FPS | Original render age, mean ms | Estimated presentation age, mean / p95 ms | Wire gaps | Fresh-picture gaps |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| phase-60-a | 60 to 60 | 60.392 | 59.906 | 12.007 | 9.281 / 17.259 | 0 | 7 |
| base-60-b | 60 to 60 | 60.479 | 54.232 | 17.533 | 13.067 / 25.075 | 13 | 104 |
| phase-120-a | 120 to 120 | 120.133 | 119.927 | 14.315 | 6.043 / 7.795 | 0 | 3 |
| base-120-b | 120 to 120 | 120.823 | 119.930 | 14.904 | 6.633 / 9.954 | 0 | 13 |
| phase-60-b | 60 to 60 | 60.137 | 59.931 | 16.082 | 7.875 / 11.700 | 0 | 3 |
| phase-fast-source | 120 to 60 | 60.478 | 60.478 | 13.408 | 5.135 / 7.527 | 0 | 0 |
| base-fast-source | 120 to 60 | 60.577 | 60.577 | 13.308 | 5.034 / 7.237 | 0 | 0 |
| phase-slow-source | 30 to 60 | 49.109 | 29.973 | 17.652 | 15.269 / 32.405 | 233 | 0 |
| base-slow-source | 30 to 60 | 45.803 | 30.009 | 21.152 | 13.520 / 31.483 | 260 | 0 |

Wire gaps exceed 1.5 stream periods; fresh-picture gaps exceed 1.5 periods at
the available distinct-picture rate. The 30-to-60 cases have zero fresh-picture
gaps above 50 ms despite many wire gaps above 25 ms. Omitted source IDs for
120-to-60 can represent expected downsampling and are not alone a loss diagnosis.

Both 60 FPS phase runs passed their freshness gate and reduced estimated
presentation age by 3.785/5.192 ms relative to the intervening baseline, which
still failed freshness. The 120 FPS pair retained about 119.93 distinct FPS and
improved estimated age by 0.590 ms. The faster-source pair was effectively
unchanged, with phase 0.100 ms slower in this sample. The slower-source pair was
1.749 ms slower after presentation with phase, despite a lower original render
age: the latter was influenced by different render-to-vsync phase. Both retained
all 438 distinct source pictures. Exact-estimator offline replays of each saved
slow trace selected identical capture IDs and times with phase on/off across
three encoder-busy assumptions, and observed claim/deadline analysis found no
changed phase deadline in those slow windows. Phase had more surplus capture
publications and repeated pictures. This points toward run variation, but does
not establish equivalence under all real schedules; slow-source ABBA repeats
remain necessary.

Initial synthetic-load and DDX follow-ups used runtime SHA-256
`12bf588d1517a3ce4545366567bdcd11c0a55af4710cc0f43fd4becd572a1294`.
The four loaded cases kept the same source/stream rates and freshness gates;
a separate GPU workload rendered at roughly 120 FPS with about 5.1 ms median
and 5.9 ms p95 render work. The DDX pair used the controlled source without that
extra load. All six decoded without errors and maintained the intended average
source presentation rate. The loaded 60 FPS phase source occasionally alternated
one/three display refreshes around its intended two-refresh cadence; its baseline
source was uniform. Both loaded 120 FPS sources were uniform. These are synthetic
stress samples, not gameplay validation.

| Case | Distinct FPS | Original render age, mean ms | Estimated presentation age, mean / p95 ms | Wire gaps | Fresh-picture gaps |
| --- | ---: | ---: | ---: | ---: | ---: |
| load-60-base | 57.089 | 18.278 | 13.959 / 21.480 | 0 | 51 |
| load-60-phase | 59.310 | 22.708 | 14.390 / 23.263 | 1 | 18 |
| load-120-phase | 118.751 | 19.930 | 11.667 / 15.832 | 13 | 45 |
| load-120-base | 119.178 | 16.626 | 8.354 / 12.111 | 0 | 28 |
| ddx-60-phase | 58.284 | 17.155 | 12.600 / 21.747 | 0 | 34 |
| ddx-60-base | 55.083 | 20.413 | 12.304 / 21.415 | 0 | 81 |

Phase improved 60 FPS freshness under synthetic load and with DDX, but their
estimated presentation ages increased by 0.432 and 0.296 ms respectively. The
loaded 120 FPS phase run was 3.313 ms slower and had more long intervals than
baseline, although both passed freshness. Its source render-to-presentation
phase matched baseline, so that difference is not removed by presentation
normalization. That phase run had 103 capture publication intervals above 12.5 ms
versus zero in baseline, even though actual source presentation was uniform.
Five of its 13 long wire intervals claimed a late publication immediately; eight
used the ordinary prediction deadline. None directly matched a phase override
on the claimed frame, but 76 other first deadlines did. The optional capture
publication alignment to the claim grid was disabled in these profiles; shared
GPU scheduling can still couple capture and encoder work. Attribution and repeat
runs are pending. These results do not establish a universal latency win or
justify enabling the diagnostic by default.

A subsequent guard restricts phase overrides to sustained surplus:
at least 32 observed timestamps, using up to 64, must average at least 110% of
the requested stream rate. It counts timestamp intervals, resets with phase
history and falls back when the surplus ends. Fixed-trace replay preserved the
phase choices of the successful 60 FPS runs while selecting ordinary pacing on
the idle 120 FPS traces and loaded 120 FPS baseline trace. It reduced, but did
not eliminate, activation on the prior loaded 120 FPS phase trace. This replay
uses modeled newest-seen observations and several encoder-busy assumptions;
it does not reproduce shared GPU scheduling or optional claim-grid feedback to
capture. That replay alone did not establish runtime acceptance; the following
group measures the guarded implementation with explicit on/off settings.

### Guarded A/B, stress and compatibility checks

All thirteen cases in `guard-plan.json` used runtime SHA-256
`f0974b404dcc613cbeab0a19857bab5e8328c4c7aaeb1a919954c5fae63fabab`,
the same 2560x1440/120 Hz desktop and 1280x720/20 Mbps AV1 workload. The loaded
120 FPS group and 30-to-60 group use baseline/phase/phase/baseline order. The
final pair deliberately removes the synthetic GPU workload's rate cap; it
renders about 183.6-183.7 FPS with 5.4 ms median GPU work, leaving very little
headroom. The freshness gates remain 58.2, 116.4 and the explicitly planned
29.1 for the 30 FPS source. All thirteen cases decoded without errors.

| Case | Phase | Source / stream FPS | Distinct FPS | Original render age, mean ms | Estimated presentation age, mean / p95 ms | Wire / fresh / publication gaps |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| guard-idle60-1 | on | 60 / 60 | 60.000 | 12.628 | 7.902 / 12.358 | 0 / 3 / 0 |
| guard-idle60-2 | off | 60 / 60 | 57.650 | 19.269 | 11.847 / 19.194 | 0 / 43 / 0 |
| guard-idle60-3 | on | 60 / 60 | 59.862 | 16.671 | 8.221 / 15.647 | 0 / 4 / 0 |
| guard-load120-1 | off | 120 / 120 | 118.986 | 19.223 | 10.952 / 14.779 | 0 / 32 / 0 |
| guard-load120-2 | on | 120 / 120 | 119.118 | 16.813 | 8.541 / 12.392 | 0 / 29 / 0 |
| guard-load120-3 | on | 120 / 120 | 119.789 | 14.934 | 6.660 / 9.963 | 0 / 16 / 0 |
| guard-load120-4 | off | 120 / 120 | 118.593 | 19.455 | 11.186 / 15.003 | 0 / 38 / 0 |
| guard-slow-1 | off | 30 / 60 | 30.057 | 19.705 | 14.350 / 31.642 | 223 / 0 / 292 |
| guard-slow-2 | on | 30 / 60 | 29.992 | 19.548 | 14.655 / 30.423 | 172 / 0 / 245 |
| guard-slow-3 | on | 30 / 60 | 30.038 | 19.535 | 13.158 / 31.095 | 237 / 0 / 315 |
| guard-slow-4 | off | 30 / 60 | 30.025 | 19.266 | 14.187 / 31.573 | 270 / 0 / 329 |
| guard-saturated60-1 | on | 60 / 60 | 53.955 | 31.880 | unavailable | 27 / 110 / 64 |
| guard-saturated60-2 | off | 60 / 60 | 53.019 | 31.676 | unavailable | 28 / 120 / 61 |

Wire and publication gaps exceed 1.5 stream periods; fresh gaps use 1.5 periods
of available distinct content. Thus slow-source wire/publication intervals above
25 ms are kept in the table, while fresh content has no gaps above 50 ms. The
saturated cases no longer pass the presentation estimator's steady-source-rate
validation, so their adjusted ages are withheld. Their raw render ages and failed
freshness gates are retained. Actual source presentation averaged 59.691/59.140
FPS and varied between one and four display refreshes, rather than a uniform two.

The two guarded idle 60 FPS runs pass freshness and reduce estimated presentation
age by 3.945/3.625 ms versus the intervening baseline, or 3.785 ms averaging those
two run differences. The baseline still fails freshness. First-deadline trace
reconstruction confirms phase-specific decisions in both guarded runs. The loaded
120 FPS group reproduces neither the earlier long wire intervals nor long capture
publication intervals. However, neither enabled loaded run has a reconstructed
surplus activation in its steady window, so their lower measured ages must not be
credited as a new 120 FPS algorithmic latency gain. These are descriptive short
runs, not a statistical guarantee against all scheduling variation.

All four slow-source runs retain every available source picture and use ordinary
pacing in the reconstructed steady windows. Their mean estimated presentation
ages average 14.269 ms baseline and 13.907 ms guarded. The earlier 1.749 ms slow
regression is not reproduced; this does not prove an improvement where the phase
override is inactive. Both saturated runs fail the unchanged 58.2 FPS gate.
Phase has no learned stable center in those reconstructed windows, so it falls
back, while host processing remains about 1.80 ms. Pacing cannot be claimed to
restore 60 distinct FPS under this uncapped workload, and no game performance or
remote RX 9070 XT/Wi-Fi conclusion follows from it.

The current candidate now enables guarded source-phase pacing for WGC-selected
streams; `frame_pacing_source_phase=false` preserves the previous behavior.
Explicit DDX keeps its previous default, since this guarded runtime group did not
validate DDX. This selects the measured 60 FPS improvement while retaining the
rate, concentration and recovery fallbacks. The default-path and recovery checks
below follow this decision; packaging is separate. These measurements do not
assert installation over the service or publication.

Runtime-4 verification passed 242 ordinary tests with 26 environment-dependent
tests ignored by default, and Clippy with warnings denied. Four selected native
checks separately passed across `native-3` and `native-4`: WGC reconnect/COM
teardown, H.264's negotiated one-reference budget, native H.264/HEVC/AV1 reference
recovery after dropped packets, and exact localized GPU texture comparison.
The one-reference H.264 check decoded all 64 frames with SPS and VUI reference
budgets of one and no LTR recovery; the unrestricted recovery test retained its
separate LTR coverage. The first default-selection follow-up repeated all 242
ordinary passes, 26 default skips, Clippy and the release build successfully.

Official Windows Moonlight Qt 6.2.0 also passed ten local hardware-decoder and
D3D11-renderer connections against runtime-3, SHA-256
`12bf588d1517a3ce4545366567bdcd11c0a55af4710cc0f43fd4becd572a1294`:
two each for H.264, HEVC, AV1, HEVC HDR negotiation and AV1 HDR negotiation at
1280x720/60 FPS. Requested formats matched negotiation, client logs reported about
60 decoded/rendered FPS and no decoder errors, and each connection and reconnect
closed cleanly. Host cancellation returned the host to its free state. The
separate quit command's GUI needed an owned-window close after cancellation, so
natural CLI quit exit is not established; the earlier standalone list timeout
also remains unresolved. HDR requests used an SDR desktop and do not validate
native HDR color or HDR display output. These loopback compatibility checks are
separate from the software-decoder latency fixture and do not validate every
Moonlight platform, Wi-Fi behavior or final-package installation.

### Repeat deadlines and current candidate

The first default-enabled executable, SHA-256
`cbc837071d1b27a02050a3db1f16ec18aa444e54b4ecacd49c33b24534491ca6`,
passed the no-override AV1 60/120 FPS cases with 59.977/119.858 distinct FPS,
zero long wire intervals and zero decode errors. Original render ages were
15.407/14.791 ms mean, and estimated presentation ages were 8.185/6.520 ms.

A deliberately terminated WGC helper reopened in 309 ms after releasing encoder
resources in 31 ms. The HEVC stream resumed without decoder errors or a new
client connection. This is a functional recovery pass, not a smoothness pass:
its whole steady window produced 51.551 distinct FPS and failed the normal 58.2
gate. The recovery runner's explicitly recorded 50 FPS delivery threshold did
not replace that freshness gate. The shortfall already existed before failure
(51.189 distinct FPS in the preceding steady interval) and remained after the
first two recovery seconds were excluded (53.804). The outage itself skipped
19 source pictures, rather than explaining the entire deficit.

That profile requested `minimum_fps_target=60`. Frequent source presentation
intervals of one/three refreshes around the intended two caused old images to
reach their 16.667 ms repeat deadline before the useful update arrived. Repeating
them spent pacing credit. The old trace logged only new-image submissions, so
its 1,062 claim records could not be joined ordinally to 1,167 received frames.
Clock-aligned matching associated 81 of 111 steady repeated pictures with outputs
without a fresh-image claim record. Separate HEVC on/off/on cases then achieved
60.004/53.533/59.963 distinct FPS and estimated presentation ages of
7.312/11.840/7.417 ms, establishing that this was not a general HEVC decoding or
phase-pacing regression. The failed recovery smoothness result remains retained.

The follow-up gives a repeat a bounded opportunity to yield to an imminent new
capture when source-phase pacing is enabled and the observed source interval is
near the stream period. Extra wait cannot exceed one quarter of the stream
period plus 0.5 ms. Slower sources, disabled phase policy and non-arrival pacing
retain their existing repeat policy. Tracing now includes every submission with
an explicit `fresh` flag, allowing same-image repeats to be separated from a
newly captured image containing the same pixels.

Eight cases in `repeat-plan.json` compare the revised executable, SHA-256
`d22e446dd103592c09cd9bf11da3a76d355ab1783a18862d5d3b9f8b31a3237a`,
with the preceding executable at the same HEVC settings. All seven revised cases
pass their declared freshness gates and decode without errors; the old HEVC case
still fails 58.2. Source presentation is uniform within each measured window.
The normal-minimum cases use the actual default `minimum_fps_target=20`; the
high-minimum and slow-source cases are explicitly different workloads.

| Case | Codec | Source / stream / minimum FPS | Distinct FPS | Original render age, mean ms | Estimated presentation age, mean / p95 ms | Same-image repeats | Wire / fresh gaps |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| repeat-hevc-on1 | HEVC | 60 / 60 / 60 | 59.793 | 11.292 | 7.343 / 11.288 | 0 | 0 / 6 |
| repeat-hevc-old | HEVC | 60 / 60 / 60 | 57.700 | 16.713 | 13.269 / 22.959 | 42 inferred | 0 / 42 |
| repeat-hevc-on2 | HEVC | 60 / 60 / 60 | 59.860 | 10.317 | 7.210 / 11.157 | 0 | 0 / 5 |
| repeat-av1-60 | AV1 | 60 / 60 / 60 | 60.000 | 13.428 | 7.321 / 9.092 | 0 | 0 / 0 |
| repeat-av1-120 | AV1 | 120 / 120 / 120 | 119.603 | 15.608 | 7.334 / 11.421 | 1 | 0 / 23 |
| repeat-slow | HEVC | 30 / 60 / 60 | 30.025 | 29.907 | 22.907 / 37.708 | 305 | 1 / 5 |
| repeat-normal60 | AV1 | 60 / 60 / 20 | 59.976 | 12.522 | 8.027 / 12.570 | 0 | 0 / 4 |
| repeat-normal120 | AV1 | 120 / 120 / 20 | 120.006 | 14.906 | 6.633 / 10.219 | 0 | 0 / 10 |

The revised HEVC runs remove the same-image repeats observed in the matching
old case and lower estimated presentation age by 5.926/6.059 ms. This overlaps
with the pacing improvements above and must not be added to them as a separate
end-to-end gain. Repeated pixels in new capture publications still occur. The
30 FPS source with a forced 60 FPS repeat minimum delivers 60.600 encoded FPS,
retains every source picture, and has five fresh-picture intervals above 50 ms;
it is not a zero-jitter pass. Its age cannot be compared directly with the earlier
30 FPS tests using minimum 20. No same-image submission in the new traces decoded
as a different picture, supporting the trace's identity distinction.

The revised source passed 246 ordinary tests, with 26 environment checks ignored
by default, plus Clippy with warnings denied and the release build. New tests
cover bounded repeat waits and fallback conditions. These measured uniform-source
runs do not establish that every irregular presentation cadence is fixed.
Subsequent direct-WGC and helper-failure checks on this same executable completed
as follows; installation and publication remain separate steps.

Direct WGC without the user helper passed 59.930/120.001 distinct FPS at requested
60/120 FPS, with zero decoder errors, long wire intervals, capture restarts or
compute fallbacks. Original render ages averaged 14.310/14.315 ms and estimated
presentation ages 7.738/6.041 ms. These cases retain the normal minimum of 20 FPS.

The final HEVC helper-failure check repeated the original explicit 60 FPS minimum.
Resource release took 31 ms and WGC reopened in 317 ms; the client remained
connected and decoded all 1,157 received frames without errors. Including the
forced outage, the whole steady window delivered 58.691 FPS and 58.348 distinct
FPS, passing the unchanged normal 58.2 freshness gate. It contains one 379.469 ms
wire gap, five repeated pictures and 24 skipped source pictures. Nineteen skipped
pictures span the outage itself; five occur during immediate encoder recovery.
The preceding steady segment produced 59.343 distinct FPS; after the first
recovery second, 59.996 distinct FPS resumed with no omitted source pictures.
Whole-window render age averaged 9.781 ms and estimated presentation age 7.224 ms.
Actual source presentation was uniform in this run. The earlier irregular-source
failure remains separate evidence, so this does not establish universal recovery
smoothness or broader display/device-loss handling. Exact records are
`direct-results.json`, `final-helper-recovery2/recovery-result.json` and
`final-recovery-analysis-20261006/RECOVERY2_SEGMENTS.json`.

Three further official Qt 6.2.0 checks against the preceding
`cbc837071d1b27a02050a3db1f16ec18aa444e54b4ecacd49c33b24534491ca6`
executable verified the client's AV1 hardware-decoder, cropping and D3D11-renderer
metadata path: 1920x1080 at 8-bit and 10-bit, and 1968x2184 at 8-bit. The client
reported cropping coded 1920x1082 to 1920x1080 and 1984x2186 to 1968x2184, with
no decoder errors and decoded/rendered rates near the requested 60 FPS. This
shows the official client's handling of padding; it does not change the failed
raw elementary-bitstream geometry gate. Pixel readback, edge correctness, native
HDR color and display accuracy were not checked. Exact summaries are
`qt62/cases/official-av1-crop-1080/summary.json` and
`qt62/cases/official-av1-crop-portrait/summary.json`; the untested transposed
2184x1968 output must not be inferred from them.

Raw plans, per-frame receiver CSVs, source presentation reports, acceptance
results and runtime provenance are under
`D:\CodexArtifacts\butterpollo-latency-compat-20261006`. The
`phase-case-analysis-20261006/RESULTS.json` and `COMPARISON.json` retain all nine
cases and raw gates; `stress-phase-review-20261006/RESULTS.json` retains the six
follow-ups. `guard-case-analysis-20261006` retains the complete thirteen-case
comparison and deadline reconstruction; `qt62/cases/official-fixed-3/summary.json`
retains the exact-client compatibility matrix. `repeat-case-analysis-20261006`
and `final-recovery-analysis-20261006` preserve repeat and recovery attribution.
`trace-review-20261006` and `slow-phase-review-20261006` preserve earlier trace
attribution and replay limitations. No result here validates the remote
RX 9070 XT/Wi-Fi report, sustained gameplay, native HDR accuracy or client
scanout. Recovery coverage is limited to the specific checks described above;
later codec fixes require their own exact-binary validation.

## October 6 follow-up: saturation, HDR state and compatibility

These follow-ups use evidence under
D:\CodexArtifacts\butterpollo-remaining-20261006. The first rc.10 test host is
SHA-256
93a681c26f8e767212e3580260d641658e1d871257b64aadd97b511e0e31bcbf.
Its ordinary suite passed 257 tests with 27 environment-dependent tests ignored
by default; Clippy with warnings denied and the release build passed. These
checks and the scoped native tests below do not establish installation or
publication of the candidate.

The final hotplug-protection revision passed 263 ordinary tests with 27 excluded
by default, including six new hotplug tests. Formatting, Clippy with warnings
denied and release builds passed. Its host SHA-256 is
3b6d6e3299cc57383c4ca5aa373a0bfbb721218afb44114c81dbbdd61a9bd89c.
The earlier executable hash and SDR result below identify their own tested
build; final package and installation provenance are recorded separately.

### Saturation diagnosis and queue-drain rejection

Four serial cases ran the unchanged rc.9 executable
d22e446dd103592c09cd9bf11da3a76d355ab1783a18862d5d3b9f8b31a3237a
in baseline/drain/drain/baseline order. The only case setting changed was
wgc_drain_to_newest=false/true/true/false. All used the WGC helper and compute
copy, implicit default source-phase pacing, AV1 1280x720 at 60 FPS and 20 Mbps,
the 2560x1440 physical desktop at 120 Hz, minimum_fps_target=20, a 20-second
stream with five-second warmup, and the same bounded uncapped synthetic GPU
workload. The workload rendered at 184.5–184.6 FPS in all four cases.
The freshness floor remained 58.2 FPS.

| Case | Actual source presentation FPS | Distinct FPS | Original render age, mean ms | Estimated presentation age, mean ms | Wire gaps above 25 ms | Host processing, mean ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| rc9-saturated-base-a | 58.727 | 54.063 | 31.034 | 17.251 | 52 | 1.760 |
| rc9-saturated-drain-a | 59.518 | 52.632 | 28.355 | 15.252 | 79 | 1.757 |
| rc9-saturated-drain-b | 59.689 | 54.101 | 27.212 | 15.405 | 58 | 1.761 |
| rc9-saturated-base-b | 59.691 | 54.808 | 29.457 | 16.888 | 34 | 1.760 |

All four decode without errors, capture restart or compute fallback, but all
fail the unchanged distinct-picture floor. Drain-a also fails the delivery
floor at 57.304 FPS. Draining reduces picture age by roughly 1.5–2.0 ms while
losing more fresh pictures and increasing long wire intervals. It remains
disabled by default. The result must not be represented as a saturation fix.

The newer timing analysis checks DXGI presentation identity separately from
uniform requested cadence. Source frame IDs match Present call IDs, and actual
presentation remains one frame behind the calls throughout each measured
window. Those checks permit the same presentation-QPC estimate described above
even though the source's frame intervals are irregular. The older table's
withheld saturated estimates are retained as the original analysis; a separate
reanalysis estimates 17.860/17.757 ms for guard-saturated60-1/2. Neither the new
estimate nor this reanalysis changes a raw age or acceptance gate. This
approximation excludes input and display scanout and has roughly 0.1 ms
claim-clock alignment precision.

The measured encoder/host processing stays near 1.8 ms. Under saturation, both
capture publication delay and frame-selection delay rise. In the current
baseline cases, all 68/72 omitted source pictures have an estimated matching
capture publication. With drainage, 73 of 100 and 51 of 80 omitted pictures no
longer have an estimated matching publication. This is inferred identity for
unclaimed images: selecting the most recent actual presentation at least
0.25–1 ms before the capture timestamp agrees with every known claimed
picture in these four runs, but unclaimed pixels were not read back.

Fixed-publication replay does not support disabling prediction globally or
shortening the minimum claim spacing as a default fix. An ideal content
deduplication oracle improves modeled baseline freshness from 54.057/55.066
to 56.646/58.287 FPS at the existing spacing, but assumes zero GPU comparison
cost and uses picture identity unavailable to production. It retains source
irregularity and does not pass every case. Exact GPU comparison therefore
remains a diagnostic candidate, not a default capture optimization.

Raw cases and unchanged acceptance are in saturation/saturation-results.json.
RC9_ABBA_REPORT.md and RC9_ABBA_DIAGNOSIS.json in that directory retain stage
timing and provenance; RC9_ORACLE_AND_LIFETIMES.json retains the model's
assumptions. No game, display mode, installed profile or service was changed by
these four tests; the installed host was checked idle and Warhammer absent
before each run.

### HDR-state correction and current SDR validation

The candidate reads the explicit modern HDR capability/request bits and active
color mode, using the dedicated HDR setter rather than treating any Windows
Advanced Color state as HDR. Its ABI is checked against
[Microsoft's SDK definitions](https://github.com/microsoft/win32metadata/blob/main/generation/WinSDK/RecompiledIdlHeaders/um/wingdi.h).
Legacy query/set behavior is retained for operating systems without the modern
query; a failed modern setter does not fall back to changing WCG. A successful
request must settle in both requested and active state. Temporary query errors
are retried; a timed-out owned request is rolled back while preserving an
observed competing request. Unit checks include pending-state cancellation,
failed disable rollback and explicit rollback failure.

The actual physical display path exposed modern flags 69 with HDR-support bit 4
clear and active mode SDR, while the legacy value 5 reported general Advanced
Color/WCG capability. That describes this current Windows display path; it
does not establish that the panel model lacks HDR hardware. The corrected
probe refuses to claim native HDR from this state. Earlier attempted physical
HDR color results that used BGRA8 capture are not native HDR validation.

One rc.10 AV1 SDR check, rc10-sdr-av1-720p60-1, passed on the unchanged physical
desktop using default WGC/helper/source-phase behavior and minimum 20 FPS.
All 1,172 received frames decoded without errors. Its steady window delivered
60.002 FPS and 871 consecutive distinct source pictures, with no repeats,
omitted IDs or wire gaps above 25 ms. Host processing averaged 1.815 ms and
original render-start-to-decoder age averaged 9.508 ms. DXGI presentation
counters remained zero in this smoke, so actual presentation timing is
unavailable; the raw age must not be relabeled as presentation-to-decoder
latency. The decoder barcode gate is independent of those missing counters.
Physical settings and 120 Hz refresh were unchanged after cleanup.

The final host repeated this default-path SDR check in
rc10-sdr-av1-720p60-2. All 1,169 received frames decoded; the steady window
delivered 59.999 distinct FPS with 868 consecutive unique picture IDs, no repeats,
omitted IDs or long wire gaps. The physical mode, 120 Hz refresh and SDR state
were unchanged. DXGI statistics again supplied no usable presentation-ID/QPC
pairs, so this is freshness and compatibility evidence without an actual
presentation-to-decoder latency claim. The original 58.2 FPS floor was retained.

The initial native virtual HDR attempts below failed before pixel validation.
The reviewed isolated harness uses one
owned extended HDR display, full-window native 1280x720 motion, independent
frame readback and the original color thresholds: luma/chroma mean absolute
error at most three 10-bit code values, contrast 0.98–1.02 and saturation
0.95–1.05. The first normally elevated attempt failed the VDD access preflight
with AccessDenied before starting its private host, creating a display,
pairing, capture or encoding. Its failure is not a codec or HDR pixel failure.
No native HDR pass or relaxed threshold follows from it. A later native HDR result
requires its own recorded capture and pixel evidence.

Subsequent isolated service-context attempts did open WGC with RgbaF16 source
pixels. One stopped because ANSI log formatting obscured the harness's readiness
check; the corrected reader preserves all acceptance tokens and thresholds.
The next attempt detected a second newly active display, the previously dormant
HISENSE, alongside the owned HDR virtual display. The exact physical/topology
gate rejected it before motion began. The original Odyssey settings were
unchanged, and teardown restored the original topology in 2.327 seconds. Neither
attempt provides a native HDR color-roundtrip pass.

The source fix captures available inactive targets immediately before owned VDD
creation/recreation and protects those same identities through HDR startup. It
prunes only reactivated dormant targets from a fresh active topology, keeping
the supplied modes, path priority and clone relationships. Strict temporary
SetDisplayConfig flags neither save the topology nor allow Windows to retime
the supplied modes. The owned display is followed by driver target and monitor
path, so GDI display-name reuse cannot redirect its HDR setup or recovery.
A second semantic snapshot check rejects an observed concurrent layout change.
The final settle phase requires 500 ms of quiet within one 1.5-second deadline,
retained across retries; expiry prevents further mutation. Protection ends
after startup. Windows has no atomic topology compare-and-set, so a narrow
last-call race remains, and intentional activation of the same dormant target
during this short window cannot be distinguished from automatic topology recall.
Six pure tests cover the selection, clone/mode, identity, race and deadline
rules. The reporter's phone behavior has not been reproduced by these local
tests; final native pixel-validation evidence follows separately.

### Final native virtual HDR pixels, excluding physical-panel calibration

Two isolated service-context runs used the final host above, each with one owned
extended 1280x720 HDR virtual display, full-window 60 FPS motion, WGC helper
capture and AMF at 40 Mbps. The stream ran for 22 seconds with a five-second
warmup and minimum repeat target 20 FPS. Both reported active modern HDR and
RgbaF16 capture; independent software decoding supplied a ten-bit frame dump
at received frame 480. No capture fallback or restart occurred. These are
native HDR capture/conversion/codec pixel checks, not physical-panel calibration,
client HDR rendering, scanout or game-content validation.

| Codec | Received/decoded frames | Steady distinct FPS | Unique pictures in steady window | Repeats / skipped IDs | Wire intervals above 25 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| HEVC Main10 HDR | 1,281 / 1,281 | 60.001 | 985 / 985 | 0 / 0 | 0 |
| AV1 Main10 HDR | 1,302 / 1,302 | 60.026 | 1,000 / 1,000 | 0 / 0 | 31 |

| Codec | Luma MAE, 10-bit codes | Chroma MAE, 10-bit codes | Contrast slope | Saturation ratio |
| --- | ---: | ---: | ---: | ---: |
| HEVC Main10 HDR | 0.393 | 0.404 | 0.9995 | 1.0059 |
| AV1 Main10 HDR | 0.387 | 0.428 | 0.9983 | 0.9976 |

Both black patches decoded to code 64 and white patches to 509 against 509.08
expected. The original limits were retained: black error at most two codes,
white error at most three, luma/chroma MAE at most three, contrast 0.98–1.02 and
saturation 0.95–1.05. Both passed the unchanged 58.2 distinct-FPS floor and
decoded without errors. AV1's 31 long arrival intervals remain an unresolved
cadence finding; its complete picture coverage and correct pixels do not make
it a gap-free smoothness pass. No latency improvement is claimed from these
two HDR runs.

Each run passed 11 during-stream topology samples and a mandatory check before
cancellation, with the original physical settings intact and only the owned
virtual display added. Logs in both runs show the guard restoring the dormant
HISENSE target after creation and again after HDR settings. This validates
correction of that reproduced startup recall, not prevention of every transient
activation or the remote phone report. Final teardown removed the virtual
display and restored the original topology; the installed service stayed
running and its configuration hash was unchanged. The temporary dispatcher
tasks were removed. HEVC/AV1 complete fixture durations were 35.493/35.280 seconds,
including 0.401/0.439 seconds of cleanup.

Full results are system-context/hdr-9dae55fb791f48f2b23414231da970dc/
coordinator-result.json (HEVC) and
system-context/hdr-4edd39d9a9d441bb8f179768b3ee1d8f/coordinator-result.json (AV1).
Their matching hdr-virtual case directories retain raw host/receiver logs,
pixel dumps, color results, topology samples and before/after snapshots.

#### Repeat HDR runs and four-run totals

Two further runs repeated AV1 and then HEVC with the same final executable,
1280x720/60 native FP16 virtual display, 22-second duration, five-second warmup
and unchanged freshness/color gates. Together with the original pair above,
the recorded order is HEVC/AV1/AV1/HEVC.

| Repeat codec | Received/decoded frames | Steady distinct FPS | Unique pictures in steady window | Repeats / skipped IDs | Wire intervals above 25 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| AV1 Main10 HDR | 1,288 / 1,288 | 59.999 | 989 / 989 | 0 / 0 | 0 |
| HEVC Main10 HDR | 1,302 / 1,302 | 60.001 | 999 / 999 | 0 / 0 | 0 |

| Repeat codec | Luma MAE, 10-bit codes | Chroma MAE, 10-bit codes | Contrast slope | Saturation ratio |
| --- | ---: | ---: | ---: | ---: |
| AV1 Main10 HDR | 0.401 | 0.503 | 0.9984 | 0.9976 |
| HEVC Main10 HDR | 0.374 | 0.434 | 0.9995 | 1.0057 |

The AV1 repeat's decoded reference frame also verifies black and a 100-nit
white patch. The source renders white as 1.25 in linear scRGB, where 1.0
represents 80 nits. These are mean limited-range ten-bit PQ luma codes from
received frame 480 (source frame 468):

| Source patch | Expected Y′ | Decoded Y′ |
| --- | ---: | ---: |
| Black | 64.00 | 64.00 |
| 100-nit white | 509.08 | 509.00 |

The values come from the AV1 repeat's `colour-result.json` and describe one
reference frame. The four-run frame total below counts successful decoding.

Both repeats passed their independent pixel checks, all 11 during-stream
topology samples, the pre-cancel topology check and complete restoration.
The installed service remained running and its configuration hash was unchanged.
Across all four HDR runs, **5,173 received frames decoded successfully** with
zero decoder errors, and **44 during-stream topology samples passed**, plus
four pre-cancel checks. Adding the final SDR check's 1,169 decoded frames gives
**6,342 clean decodes across five rc.10 runs**. These are whole-run decode totals;
the fresh-picture counts in the tables cover only the steady windows.

The four independently decoded reference frames have luma MAE 0.374–0.401
and chroma MAE 0.404–0.503 in 10-bit code values. Both recorded mean-error
metrics stay below 0.51 code values; this describes those four pixel dumps,
not a per-pixel maximum or a color analysis of all 5,173 frames.

The first AV1 run's 31 long arrival intervals remain in the original table.
The repeat adds an observation with zero intervals above 25 ms without replacing
that uneven run or establishing universally even cadence. All source, scope
and physical-panel limitations above remain unchanged.

The rc.10 validation record (VALIDATION.json of the rc.10 release, since removed)
contains all four cases in current_validation.native_hdr.fixtures. Repeat raw
results are system-context/hdr-88d67d5ef35c450da7e4badb41d069d5/
coordinator-result.json (AV1) and
system-context/hdr-f87e34f8d65c44cfa13e2c8d4b7d5d52/coordinator-result.json (HEVC),
with full evidence in their matching hdr-virtual case directories.

### Native controller and Moonlight 6.2 command checks

One explicitly selected native HID test passed with the installed signed
libvirtualgamepad 0.1.0.39 driver. It sequentially created an owned neutral
DualShock 4 and DualSense and read their newly enumerated HID reports through
the production packet decoder and Gamepads::apply path. It verified two
independent primary-pad contacts, stable movement identity, release/reuse,
cancel-all, rejection of a third occupied contact, and removal on drop.
Secondary-surface events with matching pointer IDs did not alter either
primary contact. The first probe's VID/PID pathname assumption was corrected
to inspect HID attributes; production input behavior did not change.

This validates the existing fallback and primary-pad behavior, not a second
native touch surface. The signed driver's protocol has a contact index but
no independent surface index. A genuine second surface needs a compatible
device profile, versioned driver protocol and signed package; it cannot be
represented by the second finger of the primary pad. No live physical
two-pad controller, network-delivered touch event or game-specific mapping
was tested. Evidence is touchpad/INVESTIGATION.json and
touchpad/native-test-attempt2.log; the selected test executable SHA-256 is
5b01008db084c9aad9ed1cfa886b2b6c2a41772230b7187d4403420be9b031b0.

Further checks of the unchanged official Windows Moonlight Qt 6.2.0 executable
resolve part of the earlier command-line uncertainty. Against a private rc.9
software-only host, plain listing and cached CSV listing exited naturally
with code zero in 0.204/0.207 seconds. Three running-app quit cases exited
naturally in 3.847–3.849 seconds, and idle quit in 0.811 seconds. The fixture
used a verified private Windows desktop with normal GUI startup behavior:
the earlier hidden-startup flag suppressed the client's GUI polling, and its
half-second post-cancel wait was too short. No physical desktop switch or
user input was needed.

An unmodified official client still hangs on a cold-cache CSV listing in the
controlled mock case; the 65.143-second observation was stopped and is retained
as a failure. Source/debugger evidence associates it with asynchronous artwork
workers during Qt thread-pool shutdown. Plain listing or a normally populated
artwork cache avoids that tested path. No host protocol workaround or official
client patch was shipped. These command checks contain no RTSP stream and do
not replace the earlier codec matrix or validate the new candidate's codecs.
cli/SUMMARY.json preserves exact client/host hashes, mock controls and six
passing pure lifecycle tests.

An optional [Moonlight 6.2.0 source patch](compatibility/moonlight-6.2.0/README.md)
now makes CSV listing read existing artwork without starting background
artwork downloads. Eight focused regression cases pass against the actual
patched BoxArtManager translation unit with real Qt 6.4.2 on Linux; three
negative controls expose the original background-worker behavior. This is
source-only validation with stubbed HTTP/computer and cache boundaries, not a
full Windows client build or an end-to-end patched-client result. It does not
replace the official executable or erase its recorded cold-cache failure.

The read-only environment collector now records OS, GPU/driver, physical
adapter/link counters, host version/hash and service state without changing
settings or uploading data. Its local output is environmental evidence only.
Neither it nor these local tests reproduces the remote RX 9070 XT over Wi-Fi
report, establishes NVIDIA execution, or supplies physical-panel calibration
and client-scanout validation.

## October 7: rc.17 against rc.2 on the October 4 fixture

The [1080p at 60 fps](#1080p-at-60-fps) fixture again (`run-motion.py`
from `day-work-20261002`, copied with only its tool paths changed): RX 7900
XT, HEVC 10-bit HDR at 1920×1080 and 60 fps, 20 Mbps requested, native AMF
at ultra-low latency with `speed`, compute conversion, a 120 Hz virtual
display, and the same game-like load (`gpu_load 45 1000 0 200`) on the load
rows. The rc.2 binary is the one measured on October 4 (`5747318fae0a4581`),
rc.17 the installed release (`a8f491c3257431fe`). Each run started as SYSTEM
in the signed-in session, as the service starts the host: the virtual
display driver now refuses an administrator. Rows alternated in one batch,
three runs each. The DDX rows capture with Desktop Duplication as on
October 4; the WGC row is rc.17's default capture.

| Picture age, mean / p95 (ms) | rc.2, DDX | rc.17, DDX | rc.17, WGC |
|---|---|---|---|
| Idle | 14.69 / 15.77, 14.91 / 17.16, 14.62 / 15.47 | 14.78 / 16.00, 14.66 / 16.04, 14.69 / 15.93 | 14.93 / 16.22, 14.80 / 15.98, 14.84 / 15.91 |
| Load | 35.64 / 44.80, 35.19 / 44.59, 36.10 / 46.53 | 35.72 / 46.31, 35.01 / 43.63, 35.34 / 44.38 | 33.15 / 43.06, 33.34 / 43.70, 33.77 / 44.82 |
| Load, new pictures per second | 57.6, 58.8, 57.6 | 58.2, 59.7, 59.2 | 58.3, 58.1, 58.4 |
| Load, host latency | 5.79, 5.49, 5.68 ms | 5.20, 5.70, 5.53 ms | 1.87, 1.85, 1.86 ms |

On the same capture path rc.17 delivers the picture when rc.2 did, idle
(14.7 ms both) and beside the load (35.4 against 35.7 ms, within the
spread of the runs). With its default WGC capture it delivers it 2.2 ms
sooner beside the load than rc.2 (33.4 against 35.7 ms; 43.9 against
45.3 ms at the 95th percentile), and the host latency Moonlight reports
falls from 5.7 to 1.9 ms. Idle, all three are equal; every idle run
delivered 60 new pictures a second without a repeat.

rc.2 measured 35.7 ms beside the load here against 42.3 ms on October 4,
more than the difference between the versions: absolute values move
between batches, so only compare rows measured together. Beside the load
the probe rendered at 60-66 Hz instead of 120, as on October 4, so the
fixture's source-rate check fails on load rows as it did then; their
picture-age statistics are complete. One rc.2 idle run's probe exited
with an error after the stream had ended. Artifacts: `bench-rc17\cmp-*`.

rc.19 (`88e8fd322e8b1a6a`), three idle runs with WGC later the same day on
the same fixture: 14.95 / 16.49, 14.96 / 16.14 and 14.74 / 15.94 ms, 60.4
new pictures a second without a repeat, as rc.17. Artifacts:
`bench-rc17\rc19wgc-*`.
## October 7: PyroWave conversion on the compute queue

PyroWave's colour conversion (RGB to its Y, Cb and Cr planes) ran as three
D3D11 draws on the graphics queue. Beside the game-like load
(`gpu_load 30 1000 0 200`), `examples/performance.rs --codec pyrowave --hdr
--yuv444 --records` at 1080p, 120 fps and 400 Mbps measured 5.6-5.8 ms per
frame against 0.47 ms idle. PyroWave's own GPU work, from
`pyrowave_device_report_performance_stats`, was 0.11 ms idle and 0.19 ms
under the load; a CPU wait on the conversion's fence showed where the rest
went: 5.1 ms under the load against 0.15 ms idle.

The planes are now written by one compute shader pass on the D3D12 compute
queue the AMF path uses (`pyro_cs`, high priority), into the same textures
Vulkan imports, with the shared fence ordering the two queues. The planes
are byte-identical to the graphics pass for HDR and SDR, 4:4:4 and 4:2:0,
three output sizes and a composited pointer
(`compute_planes_match_the_pyrowave_graphics_planes`).

| Per frame, RX 7900 XT | Graphics queue | Compute queue |
|---|---|---|
| Idle, repeat-frame throughput | 0.47 ms | 0.47 ms |
| Beside the load, repeat-frame throughput | 5.62, 5.75 ms | 0.57, 0.54 ms |
| Beside the load, paced at 120 fps | 4.62 ms | 0.71 ms |
| Paced, bytes per frame idle / beside the load | 155,780 / 155,960 | 155,802 / 155,834 |

The fixture client does not decode PyroWave, so no PyroWave stream was
played end to end; equal planes and equal frame sizes show the encoder
reads the same pictures.
## October 7: two frames in the encoder

The stream claimed a picture whenever the pacer allowed one, and AMF only
pushed back at eight frames in flight. When one encode takes longer than a
frame interval, up to eight frames queued and each was that much older when
it came out, without any more frames per second for it. Since rc.19 a claim
waits while two frames are in the encoder, enough to keep both of a Radeon's
encoder instances busy, and the host takes the encoder's output first.

5120×1440 HEVC at 240 fps and 150 Mbps on the RX 7900 XT, whose encoder
manages 220 fps there; two alternating runs each:

| Encoder saturated | rc.18 | rc.19 |
|---|---|---|
| Present to send, mean | 42.7 ms | 11.1 ms |
| Present to send, p99 | 45.9 ms | 13.3 ms |
| Host latency | 39.6 ms | 8.9 ms |
| Frames per second | 220 | 220 |

Streams the encoder keeps up with are unchanged: H.264, HEVC, AV1 and HEVC
VRR at 2560×720 and 60 fps pass the release e2e.
## October 7: AMF low-latency mode and AV1 latency mode

`amd_lowlatency_mode` (AMF's `LowLatencyInternal`, H.264 and HEVC) and
`amd_av1_latency_mode` (`Av1EncodingLatencyMode`) default to `auto`, which
leaves the property unset. Whether forcing them on is worth a default was
measured with `examples/performance.rs --synthetic 16 --paced` at 2560×1440
and 120 fps, 20 Mbps, `speed`, `vbr_latency`, on the RX 7900 XT with driver
32.0.31041.1004. The probe reports submission to output; the settings line
the host logs shows what the driver applied.

With the default usage, ultra-low latency, the driver already applies both:
`LowLatencyInternal=1` and `Av1EncodingLatencyMode=3` (lowest) with the
properties unset. Forcing them changed nothing. Three alternating runs each:

| Ultra-low latency, mean / p95 (ms) | `auto` | forced on |
|---|---|---|
| H.264 | 3.12 / 3.58, 3.13 / 3.50, 3.08 / 3.40 | 3.19 / 3.64, 3.10 / 3.37, 3.09 / 3.46 |
| HEVC | 3.13 / 3.62, 3.17 / 3.64, 3.17 / 3.64 | 3.12 / 3.48, 3.16 / 3.62, 3.31 / 3.69 |
| AV1 | 2.61 / 3.11, 2.62 / 3.11, 2.62 / 3.12 | 2.66 / 3.11, 2.64 / 3.12, 2.65 / 3.13 |

Every forced run produced the same number of bytes as its `auto` pair, to
the byte. The other usages, one run each:

| Usage | Codec | Driver's choice | `auto`, mean / p95 | Forced on |
|---|---|---|---|---|
| Low latency | HEVC | `LowLatencyInternal=0` | 3.59 / 3.92 ms | 3.16 / 3.63 ms |
| Low latency | AV1 | mode 0 (none) | 3.89 / 4.28 ms | 2.64 / 3.13 ms |
| Low latency, high quality | HEVC | `LowLatencyInternal=1` | 3.50 / 3.87 ms | 3.58 / 3.83 ms |
| Low latency, high quality | AV1 | mode 3 (lowest) | 3.48 / 3.76 ms | 3.51 / 3.83 ms |
| Transcoding | HEVC | `LowLatencyInternal=0` | 3.43 / 3.76 ms | 3.04 / 3.34 ms |
| Transcoding | AV1 | mode 0 (none) | 3.92 / 4.22 ms | 2.60 / 2.95 ms |

Forcing them helps only with a usage that leaves them off: about 0.4 ms per
HEVC frame and 1.3 ms per AV1 frame here, at the same output size. Both stay
`auto`. A default of on would change nothing for the default usage and
override a user's deliberate choice of another one, and writing
`LowLatencyInternal` explicitly froze HEVC encoding on RX 9000 cards with
Adrenalin 26.5 (video stalls while audio plays), the reason the native AMF
encoder stopped forcing it. The setting descriptions now say which usages
already have them on. The fallback console page saved the low-latency switch
as `amd_low_latency_internal`, a key nothing reads; it now saves
`amd_lowlatency_mode` and offers the AV1 latency mode too.
## October 7: how much bitrate PyroWave needs

A user streamed PyroWave at 720p60 with Moonlight's bitrate at 10 Mbps and
saw blurred grey blocks: a fifth of a bit per pixel per frame leaves room for
only the coarsest brightness layers. To see what the encoder takes when the
bitrate does not limit it, `examples/performance.rs --codec pyrowave
--records --paced --bitrate 1000000` at 1920×1080 and 60 fps, four seconds
each, on the RX 7900 XT:

| Picture | Per frame | Bits per pixel | Mbps at 60 fps |
|---|---|---|---|
| Desktop, SDR 4:2:0 | 624 KB | 2.41 | 300 |
| Desktop, HDR 4:4:4 | 478 KB | 1.84 | 229 |
| Moving test picture (`--synthetic 16`), SDR 4:2:0 | 1,822 KB | 7.03 | 875 |
| Moving test picture, HDR 4:4:4 | 1,344 KB | 5.18 | 645 |

The PyroWave README targets "~200+ mbit/s" on a wired LAN. A 720p60 HDR
stream to Nonary's client at 149 Mbps (2.7 bits per pixel) worked, at 60 fps
and 0.68-0.78 ms from present to encoded frame.

Below one bit per pixel per frame, less than half of what a desktop takes,
the host now warns: once when the stream starts and when the client lowers
the bitrate (`PyroWave has too little bitrate`), and on the console's stream
card. That is about 55 Mbps at 720p60, 125 Mbps at 1080p60 and 500 Mbps at
4K60 (`pyrowave::minimum_kbps`). The setting's description and the
configuration guide give the same numbers. The host does not raise the
bitrate itself: the client's choice may reflect its network.
## October 7: PyroWave decoded end to end

The ignored `pyrowave::decode_test::pyrowave_decoded_end_to_end` test sends
known pictures through `Encoder::new_gpu_options` and `encode_gpu`, codec
3, then decodes with the pinned PyroWave 0.6 SDK, bitstream `186f0393`.
The decoder has its own Vulkan device on the same AMD adapter. It writes
three imported D3D11 textures through `pyrowave_decoder_decode_gpu_buffer`;
a shared fence completes before D3D11 staging readback. SDR8 uses R8 UNORM,
and both ten-bit formats use R16 UNORM, retaining the decoder's precision.

The sender's `VideoPacketizer::encode_pyrowave` produces the datagrams,
including its normal 20% critical FEC for records, with optional detail FEC
disabled. A separate receive parser orders the data shards, removes the
transport header and final padding, then
feeds either the length-prefixed codec packets or the individual records
to `pyrowave_decoder_push_packet`. Record padding is skipped. The test
requires complete-frame readiness and rejects early readiness. This covers
complete, unencrypted delivery; it does not exercise network loss recovery,
socket pacing or client display.

Two deterministic charts contain colour bars, smooth gradients, one- and
two-pixel text strokes, and independent RGB noise between 25% and 75%.
An independent double-precision CPU reference converts the actual uploaded
pixels to full-range BT.709 SDR or BT.2020 PQ HDR, including the centred
2x2 chroma average for 4:2:0. SDR10 starts from FP16 scRGB, rather than
eight-bit pixels in a ten-bit container. HDR tests both FP16 scRGB with
1000-nit white and packed ten-bit PQ input. PSNR uses peaks of 255 or 1023;
maximum errors are in those code values, including fractional codes from
R16 readback. HDR PSNR is in PQ code space, not linear light.

Each case alternates graphics/compute, compute/graphics, graphics/compute.
It asserts that compute actually ran, and that every decoded sample is
identical across both conversions, both framings and all three batches.
The test resets only the encoder's previous timestamp before each sample,
using its normal first-frame budget of 1/60 second: CPU reference and
readback time cannot inflate the plain-packet budget. These are picture
quality measurements, not sustained throughput measurements.

The 400, 125 and 30 Mbps cases run at both sizes, along with a 1000 Mbps
control. At 1000 Mbps the test requires at least 40 dB on every plane and
35 dB on every chart panel; at 400 Mbps it requires 24 dB on every plane.
The control is needed because 400 Mbps still loses fine text and noise at
1080p, especially in 4:4:4. These are regression limits for this chart,
not recommended quality targets or assertions that the codec is lossless.

On an AMD machine, use the usual Rust SDK environment, then run from the
workspace root. On this machine the environment script is
`C:\Users\ramaz\.codex\artifacts\butterpollo-rust-20260930\performance-probe\rust-env.ps1`.
`BUTTERPOLLO_PYROWAVE_ROOT\share\pyrowave-shared\build-info.txt` must name
commit `186f0393b77f7755953b5ecde994bb1cec2e4155`.

```powershell
. 'C:\Users\ramaz\.codex\artifacts\butterpollo-rust-20260930\performance-probe\rust-env.ps1'
cargo test -p butterpollo-windows --lib --no-run --target-dir target\qa
if ($LASTEXITCODE) { throw 'Test build failed' }
Copy-Item "$env:BUTTERPOLLO_PYROWAVE_ROOT\bin\libpyrowave-shared-0.dll" target\qa\debug\deps\
cargo test -p butterpollo-windows --lib pyrowave::decode_test::pyrowave_decoded_end_to_end --target-dir target\qa -- --ignored --exact --nocapture --test-threads=1 2>&1 | Tee-Object target\qa\pyrowave-decode.log
if ($LASTEXITCODE) { throw 'PyroWave decode test failed' }
```

`PYROWAVE_RESULT` lines contain JSON with per-plane PSNR and maximum error,
frame bytes and datagram counts. `panels` lists bars, gradient, text and
noise for each of Y/Cb/Cr; a null PSNR with zero maximum error means an
exact match (infinite PSNR). Before each batch, `PYROWAVE_BATCH` records the
installed host log's last connect/disconnect event, or notes that its log
is absent. Check those lines when comparing a machine shared with streams
or other GPU work. The test only reads that log and does not start a host
or change its settings. NVIDIA execution is unverified here; NVIDIA users
should use Vibepollo.

On October 7, 2026, the AMD Radeon RX 7900 XT with driver 32.0.31041.1004
passed all 1,152 decodes in 620.90 seconds in the debug test build. All 288
batch checks found the last stream event was a disconnect at 12:57:29 UTC.
Other GPU work was not controlled. The maximum compute/graphics difference
was zero storage units, including all 16 bits of the ten-bit output planes.
Both framings and all three repeats also matched exactly: repeat spread
was 0.00 dB PSNR and zero maximum-error difference for every picture.

Each row below gives the lower PSNR and larger maximum error of the two
pictures, separately for each plane. Repeated measurements do not change
those values. The maximum error uses 8-bit codes for SDR8 and 10-bit codes
for SDR10/HDR10; it is not a count of R16 storage units. The rates are the
requested budgets at 60 fps, not measured network throughput.

| Size | Format | Chroma | Mbps | PSNR Y/Cb/Cr (dB) | Max error Y/Cb/Cr (codes) |
|---|---|---|---:|---|---|
| 1920x1080 | SDR8 | 4:2:0 | 1000 | 80.62/81.16/80.35 | 1.0/1.0/1.0 |
| 1920x1080 | SDR8 | 4:2:0 | 400 | 33.34/43.05/42.06 | 64.0/36.0/27.0 |
| 1920x1080 | SDR8 | 4:2:0 | 125 | 22.34/28.37/29.01 | 132.0/93.0/124.0 |
| 1920x1080 | SDR8 | 4:2:0 | 30 | 18.64/23.33/26.13 | 186.0/114.0/129.0 |
| 1920x1080 | SDR8 | 4:4:4 | 1000 | 44.78/41.20/40.87 | 23.0/24.0/27.0 |
| 1920x1080 | SDR8 | 4:4:4 | 400 | 28.83/25.90/27.26 | 96.0/97.0/92.0 |
| 1920x1080 | SDR8 | 4:4:4 | 125 | 21.89/23.28/24.87 | 141.0/103.0/116.0 |
| 1920x1080 | SDR8 | 4:4:4 | 30 | 18.63/21.22/23.40 | 186.0/127.0/135.0 |
| 1920x1080 | SDR10 | 4:2:0 | 1000 | 67.65/68.04/67.97 | 3.1/3.0/3.0 |
| 1920x1080 | SDR10 | 4:2:0 | 400 | 33.32/43.35/42.19 | 258.8/145.5/97.4 |
| 1920x1080 | SDR10 | 4:2:0 | 125 | 22.32/28.42/29.00 | 528.8/374.7/492.3 |
| 1920x1080 | SDR10 | 4:2:0 | 30 | 18.63/23.36/26.14 | 745.0/456.2/517.5 |
| 1920x1080 | SDR10 | 4:4:4 | 1000 | 45.22/41.33/40.84 | 108.6/93.6/108.1 |
| 1920x1080 | SDR10 | 4:4:4 | 400 | 28.88/25.41/27.31 | 382.8/387.1/364.5 |
| 1920x1080 | SDR10 | 4:4:4 | 125 | 21.89/23.32/24.91 | 572.2/412.0/480.2 |
| 1920x1080 | SDR10 | 4:4:4 | 30 | 18.61/21.24/23.42 | 745.0/512.3/541.3 |
| 1920x1080 | HDR10 | 4:2:0 | 1000 | 68.12/67.87/66.92 | 3.1/2.7/2.9 |
| 1920x1080 | HDR10 | 4:2:0 | 400 | 39.49/49.67/49.24 | 155.1/79.7/34.4 |
| 1920x1080 | HDR10 | 4:2:0 | 125 | 28.93/35.42/39.77 | 294.4/144.5/83.2 |
| 1920x1080 | HDR10 | 4:2:0 | 30 | 23.49/31.17/37.98 | 466.2/176.4/101.9 |
| 1920x1080 | HDR10 | 4:4:4 | 1000 | 53.65/47.58/51.44 | 43.0/38.5/29.2 |
| 1920x1080 | HDR10 | 4:4:4 | 400 | 35.29/34.12/38.01 | 176.5/133.7/85.5 |
| 1920x1080 | HDR10 | 4:4:4 | 125 | 27.69/31.15/36.34 | 305.7/152.7/85.1 |
| 1920x1080 | HDR10 | 4:4:4 | 30 | 23.48/29.10/35.32 | 476.7/202.0/103.4 |
| 1280x720 | SDR8 | 4:2:0 | 1000 | 77.53/84.60/80.58 | 1.0/1.0/1.0 |
| 1280x720 | SDR8 | 4:2:0 | 400 | 65.69/79.82/76.16 | 2.0/2.0/1.0 |
| 1280x720 | SDR8 | 4:2:0 | 125 | 27.27/33.94/33.93 | 105.0/50.0/62.0 |
| 1280x720 | SDR8 | 4:2:0 | 30 | 19.92/24.09/26.26 | 186.0/103.0/116.0 |
| 1280x720 | SDR8 | 4:4:4 | 1000 | 77.53/62.48/62.88 | 1.0/2.0/2.0 |
| 1280x720 | SDR8 | 4:4:4 | 400 | 42.55/37.25/36.81 | 24.0/42.0/46.0 |
| 1280x720 | SDR8 | 4:4:4 | 125 | 26.04/23.86/25.89 | 109.0/97.0/105.0 |
| 1280x720 | SDR8 | 4:4:4 | 30 | 19.24/22.09/23.81 | 186.0/134.0/122.0 |
| 1280x720 | SDR10 | 4:2:0 | 1000 | 67.49/68.05/67.76 | 4.0/2.6/2.9 |
| 1280x720 | SDR10 | 4:2:0 | 400 | 65.96/68.01/67.74 | 6.5/7.2/4.8 |
| 1280x720 | SDR10 | 4:2:0 | 125 | 27.24/33.93/34.05 | 422.7/199.3/251.2 |
| 1280x720 | SDR10 | 4:2:0 | 30 | 19.91/24.20/26.28 | 745.0/410.3/470.4 |
| 1280x720 | SDR10 | 4:4:4 | 1000 | 67.49/62.36/62.34 | 4.0/5.5/5.2 |
| 1280x720 | SDR10 | 4:4:4 | 400 | 42.62/37.27/36.83 | 109.1/129.2/184.8 |
| 1280x720 | SDR10 | 4:4:4 | 125 | 26.04/23.89/25.92 | 434.2/387.2/408.1 |
| 1280x720 | SDR10 | 4:4:4 | 30 | 19.19/22.13/23.84 | 745.0/498.0/491.5 |
| 1280x720 | HDR10 | 4:2:0 | 1000 | 67.99/67.83/66.85 | 2.6/2.7/3.4 |
| 1280x720 | HDR10 | 4:2:0 | 400 | 67.99/67.83/66.85 | 2.6/2.7/3.4 |
| 1280x720 | HDR10 | 4:2:0 | 125 | 34.40/41.98/42.13 | 163.5/81.9/76.0 |
| 1280x720 | HDR10 | 4:2:0 | 30 | 25.07/31.56/38.60 | 458.5/191.8/87.4 |
| 1280x720 | HDR10 | 4:4:4 | 1000 | 67.99/61.99/61.41 | 2.6/5.6/6.8 |
| 1280x720 | HDR10 | 4:4:4 | 400 | 50.15/44.48/45.80 | 89.0/64.9/55.6 |
| 1280x720 | HDR10 | 4:4:4 | 125 | 33.18/31.73/37.17 | 288.2/144.2/86.9 |
| 1280x720 | HDR10 | 4:4:4 | 30 | 25.05/29.58/35.52 | 459.5/195.7/91.6 |

At 1080p60, `minimum_kbps` is 124,416 kbps (one bit per pixel per frame).
The 400, 125 and 30 Mbps budgets are 3.215, 1.005 and 0.241 bits per pixel
per frame. At 720p60 the warning is 55,296 kbps, and the same budgets are
7.234, 2.261 and 0.543 bits per pixel per frame. These are nominal encoder
budgets; framing, FEC and network headers add bytes on the wire.

There is substantial detail loss below the warning, but these pictures do
not show a sharp quality cliff at one bit per pixel. In the 1080p SDR8
4:2:0 chart, the worst text-panel luma PSNR is 30.08 dB at 400 Mbps,
18.03 dB at 125 Mbps and 13.75 dB at 30 Mbps. The corresponding whole-plane
luma scores are 33.34, 22.34 and 18.64 dB. Quality is already poor at the
warning's boundary; the larger drop is between 400 and 125 Mbps.

The current threshold remains a conservative warning about very low
bitrate, not a promise of good quality above it. If it is intended to
protect fine SDR text, about 3.2 bits per pixel per frame is a candidate
for 4:2:0: 400 Mbps is the lowest tested 1080p rate to keep both text and
whole-plane luma above 30 dB. That would be about 178 Mbps at 720p60 by
pixel-count scaling, an estimate rather than a measured cutoff. It does
not cover 4:4:4: at 400 Mbps the 1080p SDR8 plane scores are only
28.83/25.90/27.26 dB. Only the 1000 Mbps control clears 40 dB on all planes
throughout this matrix. More rates near a proposed boundary and real
desktop/game captures are needed before choosing a replacement that
applies across content and chroma formats. `minimum_kbps` is unchanged.

## October 7: 4:4:4 from AMF

Whether the native AMF encoder could offer HDR 4:4:4 HEVC or AV1 was
checked on the RX 7900 XT, driver 32.0.31041.1004, AMF runtime 1.5.2.0, with
a standalone probe outside Butterpollo. It cannot; the hardware encodes 4:2:0
only.

- The encoder caps report `HevcMaxProfile` 2 (Main10) and `Av1MaxProfile` 1
  (Main). Input formats are YUV420P, YV12, BGRA, RGBA, ARGB, NV12 and P010;
  output formats NV12 and P010. The runtime's own property table lists
  `HevcProfile` as {Main, Main10} and `Av1Profile` as {Main}, and has no
  chroma-format property. H.264 stops at High.
- `Init` with AYUV, Y410 or Y416 input returns `AMF_INVALID_FORMAT` for both
  codecs. Setting `HevcProfile` to 3, 4, 5 or 16, or `Av1Profile` to 2 or
  higher, returns `AMF_INVALID_ARG` and the profile stays Main10 or Main.
- RGBA, BGRA and R10G10B10A2 input is accepted, and FFprobe reads the output
  as `yuv420p` or `yuv420p10le`: the encoder converts RGB to 4:2:0 itself.
- The driver's D3D12 video encoding API, queried separately on the same GPU,
  supports HEVC Main and Main10 only (not Main12, the 4:2:2 profiles or any of
  the four 4:4:4 profiles) and AV1 Main only (not High or Professional), with
  NV12 and P010 input.
- The AMF SDK on GitHub, 1.5.3 of September 29, has the same profile enums.
  Its 4:4:4 note (1.5.0) is about the video converter, not the encoders.
  AMD's AMF maintainer answered requests for it with "4:4:4 and 4:2:2 codecs
  are currently not supported by hardware" (AMF issue 483, September 2025)
  and said RX 9000 encodes HEVC "4:2:0, 8 and 10 bit" (issue 539). The AMF
  wiki's hardware table says "All codecs are 4:2:0" through VCN 5.0. RDNA 4
  could not be tested here.

Packing 4:4:4 into a larger 4:2:0 picture, or into two 4:2:0 streams as RDP's
AVC444 does, needs a client that reassembles the planes. Moonlight decodes a
single stream at the negotiated size and shows it as is, so a stock client
would display the packed layout. The native AMF encoder keeps refusing 4:4:4,
which the startup probe already treats as unavailable, so an AMD host never
offers it; Moonlight PC then warns "Your host PC doesn't support YUV 4:4:4
streaming" and uses 4:2:0. PyroWave remains the full-chroma path on AMD,
including 10-bit HDR 4:4:4.

## October 7: where WGC loses a millisecond

On the RX 7900 XT, an 8×1-pixel window in the display's bottom-right corner
encoded a changing picture ID. `windows/examples/capture_phase_probe.rs`
matched captured IDs to QPC samples immediately before the renderer's
`Present(1)` call, recorded host acquisition, and waited for a D3D11 event
query after the host-owned texture copy. Only then did it read the eight
pixels. Thus Present-to-copied includes composition, capture delivery and
the completed host copy, but excludes encoder claim, conversion, encoding,
networking and decoding. It is not a stream or input-to-display measurement.

The main batch used a 1920×1080 virtual display reporting 120 Hz, kept alive
by a test-owned SYSTEM task. The probes themselves ran as the signed-in
user. `helper` forces the production WGC helper process and its three shared
textures; `wgc` captures in-process; `ddx` uses Desktop Duplication. Each
condition ran WGC, helper, DDX, DDX, helper, WGC, nine seconds per run,
excluding the first second and final 250 ms. The loaded runs used
`gpu_load SECONDS 1000 0 200`. The fixture checked the installed host log for
a connected client before runs; the JSON does not record other agents' GPU
activity. The range below retains that batch's spread.

Arithmetic means of two per-run means, with their range in parentheses:

| Present to completed host copy, ms | DDX | WGC in-process | WGC helper |
| --- | ---: | ---: | ---: |
| Idle, 59.3 Hz source | 5.05 (5.03–5.08) | 5.88 (5.62–6.15) | 6.05 (5.95–6.16) |
| Idle, vsync-paced source | 6.38 (6.31–6.46) | 6.78 (6.70–6.86) | 7.08 (7.02–7.13) |
| Heavy GPU load, 59.3 Hz requested | 35.62 (35.36–35.89) | 35.23 (34.92–35.55) | 34.28 (31.77–36.79) |

The second row requested 500 Hz and used vsync. Although the virtual display
reported 120 Hz, the recorded Present intervals were about 4.167 ms
(240 calls/s), not 8.333 ms. It should not be described as a verified
120 Hz source. The slow idle source measured 16.863 ms between Presents.
Idle picture coverage was 100%, except one fast DDX run at 99.57%. Under
load it was 86.1–88.7% for DDX, 95.6–96.5% for direct WGC and 89.8–92.6%
for the helper; the table measures pictures actually captured.

Subtracting these batch means, the helper was 1.00 ms behind DDX at 59.3 Hz
and 0.69 ms behind it in the vsync-paced case. The helper-minus-direct WGC
differences were only 0.17 and 0.29 ms. These alternating-run differences
estimate the broker hop, rather than measuring the hop on the same frame.
Most of the idle gap therefore lies in WGC delivery itself. Loaded per-run
means were roughly 32–37 ms; the helper alone varied by 5.02 ms, so there is
no established latency saving from bypassing it under this load.

The earlier SYSTEM-host batch corroborated the idle gap: at 59.3 Hz,
helper/DDX means were 5.85/4.89 ms, and in the vsync-paced case 7.43/6.38 ms.
Its loaded batch ended with a helper first-frame timeout, so it is not a
complete loaded comparison. On the physical display, the initial 59.3 Hz
batch recorded 6.35 ms for direct WGC, 6.21/6.61 ms for the helper and
4.76/4.70 ms for DDX. The final direct-WGC run captured only 36.1% of IDs;
do not pool it into that comparison. Capturing two paths simultaneously
also changed their timings, so the side-by-side runs are excluded above.

The intermediate `stamp_qpc` in these files is the host's legacy capture
stamp: DDX's last presentation/cursor update or WGC's **clamped**
`SystemRelativeTime`. It is not an independent DWM-present observation on
WGC. The separate `wgc_stamp_probe` found 92% of physical-display stamps
in the future at arrival, with mean arrival-minus-stamp about -0.79 ms.
That explains why `detect_mean_ms` could say about 0.12 ms while the
picture-ID probe measured a real delivery gap. New signed
`wgc_stamp_to_host_*` fields and valid/future stamp counts expose this
limitation; the legacy age fields and pacing clock remain unchanged.
[Metric definitions](../docs/performance.md#what-the-numbers-mean) distinguish
these offsets from actual Present-to-copied measurements.

An unbuilt direct IddCx frame ring is estimated to save about 1 ms at idle
if it avoids this WGC delivery gap, and nothing measurable under the tested
load. Only about 0.2–0.3 ms is the helper hop. It is not worth building for
latency now. This is an estimate, not a measurement of an IddCx ring.
The [rc.17 end-to-end fixture](#october-7-rc17-against-rc2-on-the-october-4-fixture)
also puts the component result in context: DDX and WGC picture age were
only 0.15 ms apart idle (14.71 versus 14.86 ms), while WGC was about 2 ms
better under load (33.42 versus 35.36 ms). Component differences do not
predict that entire journey.

Raw artifacts are under
`C:\Users\ramaz\AppData\Local\Temp\claude\C--src-butterpollo\7e5651cf-4ad6-4e53-8a70-43a4eede758f\scratchpad\`:
`vphase/u-01` through `u-18` supply the main table, `vphase/v-*` the earlier
SYSTEM batch, and `phase/idle59-*`, `phase/t-*` and `phase/stampsrc-*` the
physical checks. The raw stamp probe's aggregate above was reported by the
original study; those JSON files do not retain its raw stamp samples. Reproduce
with the usage headers in `capture_phase_probe.rs`, `wgc_stamp_probe.rs`
and `session_command_long.rs`. Virtual runs require a test-owned SYSTEM
scheduled task (the `bench-rc17` harness), no active stream, and task/display
cleanup afterward. No capture wait or pool-size optimization was made here.

## October 7: AMF split-frame encoding

AMF's `HevcMultiHwInstanceEncode` and `Av1MultiHwInstanceEncode` let the
driver split one frame across a GPU's video encode engines (AMD's samples
call it "split frame encode", default on); H.264 has no such property. The
original C++ host asked for it before Init when the caps reported more than
one engine and the driver had it off. The Rust host never wrote it. A real
split would show as roughly half the time per frame at large sizes;
alternating whole frames between engines would show as more frames per
second at the same time per frame.

Method: `examples/performance.rs --synthetic 16 --paced --seconds 3` on the
RX 7900 XT, driver and AMF runtime 32.0.31041.1004, with the defaults
(ultra-low latency, `speed`, `vbr_latency`, input converted on the D3D12
compute queue) at 20, 40, 80, 80 and 150 Mbps for 1920×1080, 2560×1440,
3840×2160, 5120×1440 and 7680×2160. Each case ran with the property
untouched (a measurement build that skipped the write), forced on and forced
off, twice, in the order untouched, on, off, off, on, untouched. The
settings line confirmed the value after Init. Before every run the host log
was checked for a stream; a first batch that overlapped a client's stream was
discarded. `--slices N` and `--intra-refresh` were added to the probe for the
interaction runs. Every run's JSON, settings line and decision line are in
`C:\Users\ramaz\.codex\artifacts\butterpollo-amf-split-frame-20261007`
(`probe.jsonl` is the discarded batch).

The driver reports two engines for both codecs (`HevcNumOfHwInstances=2`,
`Av1CapNumOfHwInstances=2`) and has the property on before Init with nothing
written, in all 630 runs, with every usage tried. On, off and untouched gave
the same time per frame, the same frames per second and the same bytes per
frame (within 0.2%, the difference of a frame in a three-second run) in all
60 combinations of HEVC and AV1, the five sizes, 60, 120 and 240 fps, SDR
and HDR. At 60 fps, SDR, mean of each run in ms:

| Submission to output | Untouched | Forced on | Forced off |
|---|---|---|---|
| HEVC 1920×1080 | 2.21, 2.20 | 2.29, 2.29 | 2.25, 2.30 |
| HEVC 2560×1440 | 3.29, 3.27 | 3.35, 3.27 | 3.31, 3.34 |
| HEVC 3840×2160 | 6.19, 6.10 | 6.13, 6.13 | 6.18, 6.15 |
| HEVC 5120×1440 | 5.59, 5.65 | 5.68, 5.60 | 5.69, 5.64 |
| HEVC 7680×2160 | 11.34, 11.38 | 11.42, 11.44 | 11.38, 11.24 |
| AV1 1920×1080 | 1.79, 2.02 | 1.86, 1.92 | 1.85, 1.94 |
| AV1 2560×1440 | 3.02, 2.99 | 2.98, 2.91 | 2.96, 3.00 |
| AV1 3840×2160 | 7.10¹, 5.08 | 5.04, 5.08 | 5.08, 5.04 |
| AV1 5120×1440 | 4.88, 4.77 | 4.76, failed¹ | 4.74, 4.79 |
| AV1 7680×2160 | 9.34, 9.41 | 9.44, 9.38 | 9.35, 9.47 |

¹ A stall; see below. HDR and the 120 and 240 fps runs agree the same way:
no median differs by more than 0.2 ms between the three, in no consistent
direction.

The time per frame grows with the picture as one engine's would: 11.3 ms
at 7680×2160 against 6.1 ms at 3840×2160 for HEVC. Where one engine cannot
keep up, a split would have helped most, and nothing changed. Frames per
second without pacing (`--seconds 4`), two runs each:

| Unpaced | Untouched | Forced on | Forced off |
|---|---|---|---|
| HEVC 1920×1080 | 660, 660 | 659, 660 | 660, 660 |
| HEVC 3840×2160 | 196, 196 | 195, 196 | 196, 196 |
| HEVC 5120×1440 | 216, 216 | 216, 216 | 216, 216 |
| HEVC 7680×2160 | 100, 100 | 100, 100 | 100, 100 |
| AV1 1920×1080 | 835, 835 | 836, 835 | 835, 836 |
| AV1 3840×2160 | 248, 248 | 248, 248 | 248, 249 |
| AV1 5120×1440 | 270, 270 | 270, 270 | 270, 269 |
| AV1 7680×2160 | 126, 126 | 126, 126 | 126, 126 |

HEVC at 7680×2160 and 120 fps settles at 100 fps with 82 ms from submission
to output in all three. Frames do overlap in the encoder (196 frames per
second at 3840×2160 against 164 from the time per frame), but a second
engine would nearly double that. So on this GPU and driver one stream uses
one engine, whatever the property says, and the rc.19 note that two frames
in the encoder keep both of a Radeon's instances busy was wrong; its comment
now says so.

Nothing else made the driver split. At 3840×2160 and 60 fps, mean of two
runs each in ms, untouched / on / off:

| Also set | HEVC | AV1 |
|---|---|---|
| 2 slices (AV1: tiles) | 6.30 / 6.33 / 6.34 | 5.22 / 5.23 / 5.23 |
| 4 slices (AV1: tiles) | 6.34 / 6.28 / 6.23 | 5.17 / 5.19 / 5.19 |
| Intra refresh | 6.29 / 6.22 / 6.21 | 5.16 / 5.16 / 5.14 |
| 2 LTR frames | 6.41 / 6.35 / 6.34 | 5.06 / 9.02¹ / 5.01 |
| Quality preset balanced | 6.46 / 6.50 / 6.45 | 5.11 / 5.05 / 5.01 |
| Quality preset quality | 7.47 / 7.46 / 7.48 | 17.48 / 17.43 / 17.44 |
| Pre-analysis | 25.85 / 26.01 / 26.02 | 25.22 / 25.08 / 25.08 |
| CBR | 6.39 / 6.40 / 6.33 | 5.26 / 5.21 / 5.20 |
| Usage transcoding | 25.11 / 24.80 / 25.14 | 12.37 / 12.33 / 12.28 |
| Usage low latency | 23.81 / 25.06 / 25.52 | 12.24 / 12.29 / 12.34 |
| Usage high quality | 20.32 / 20.32 / 20.42 | 19.48 / 19.52 / 19.56 |
| SmartAccess Video on | 6.48 / 6.45 / 6.41 | 5.19 / 5.23 / 5.15 |
| D3D11 input (`gpu_compute_conversion=false`) | 6.45 / 6.42 / 6.55 | 5.35 / 5.23 / 5.24 |

The usages other than ultra-low latency queue frames (p95 of 19-70 ms), so
their means vary more. AV1 needs two tiles above 4096 pixels wide and the driver chose them by
itself at 5120 and 7680 wide; two or four slices or tiles, SmartAccess
Video, the transcoding usage and D3D11 input at 7680×2160 also left the
three alike. Bytes per frame matched in every pair.

Six of these 630 runs stalled once: two gave up (`AMF GPU queue failed to
drain`), one stopped submitting for 35 seconds, and three lost up to 0.55 s
of frames or raised the mean by 2-8 ms while the median stayed put. Four
were forced on, two untouched, none forced off. No stream was running and
the system log has no driver reset, but other builds and an installer ran on
the machine during the matrix. The six cases then ran five more times each
for eight seconds in all three variants (90 runs, twelve minutes of
encoding): no stall, the means within 0.05 ms of each other, and the driver
again had the property on before Init. The stalls do not follow the
property.

`amd_split_frame` now does what the original host did: `auto` (the default)
asks for split-frame encoding only when the caps report more than one engine
and the driver has it off, `enabled` asks whenever there is more than one
engine, and `disabled` turns it off. With one engine, as the RX 9070 XT's
HEVC encoder, nothing is written in any mode, and a driver that rejects the
property keeps the stream. The host logs the setting, the engine count and
the driver's value (`AMF split-frame encoding left to the driver` or
`requested`), and the settings line shows the value after Init. On the RX
7900 XT `auto` writes nothing. Whether a driver that ships with it off then
splits frames, and whether RDNA2 cards with two engines do, was not
measured here.
## 2026-10-07: send PyroWave FEC blocks sooner

These measurements use an AMD RX 7900 XT (driver 32.0.31041.1004) and Ryzen
7 5800X3D. The baseline is `523aa283794690a3cd3beb3ca3ed2b172ecd87bb`.
NVIDIA users should use [Vibepollo](https://github.com/Nonary/Vibepollo);
this experiment does not measure NVIDIA hardware.

The sender now prepares and sends one FEC block at a time. The coarse block
and its parity leave before later blocks are copied into RTP shards and
encrypted; preparing the next block can use part of the previous batch's
pacing interval. Record packing sizes its fit tree to the records present
and stops updating ancestors once their earliest record is unchanged.
`DetailFec` uses a pre-sized hash map for record lookup; it never depends on
map iteration order. Its stability thresholds, wire budget and FEC plans
are unchanged. An abandoned iterator advances the frame number once a
block has escaped and keeps the consumed sequence numbers and nonces.

Temporary probes timed mapped-bitstream access and SDK packetizing,
`record_frame`, `DetailFec::observe`, layout/FEC planning, shard allocation
and copying, parity, encryption, pacing waits and Winsock calls. They also
recorded the first send batch and final send relative to capture claim and
presentation. These logs are removed from production; the existing
`stream timings` counters remain. "Sent" here means the socket call
returned, not a hardware timestamp on a physical link. The first-batch
measurement includes sending every datagram in that batch.

The encoder probe used `windows/examples/performance.rs --codec pyrowave
--records --hdr --yuv444 --paced --synthetic 8 --seconds 6`, with
`--width 1920 --height 1080 --fps 120 --bitrate 400000` and
`--width 3840 --height 2160 --fps 60 --bitrate 800000`. All eight probes in
the two complete A/B pairs reached the requested cadence. Mean encoder
submission-to-output time, including packetizing and record packing, was
0.969–0.972 ms before / 0.963–0.969 ms after at 1080p, and 2.467–2.565 ms
before / 2.612–2.850 ms after at 4K. These are not GPU-only encode times.

The 12-second encrypted host runs used the isolated `release/e2e.py`
fixtures and the pinned Nonary transport referenced above, with GPU decode
removed from a temporary receiver. The receiver still decrypts and
assembles the stream; its inherited "decoded" counter counts delivered
frames in this mode. The source was a 5120×1440 SDR desktop with the motion
strip, scaled to the requested HDR 4:4:4 output. This does not validate
native HDR capture or client display latency. `pacing_max_bitrate_kbps=0`
was unchanged: loopback has no reported physical link speed, so the
measured pacing rates were about 442 Mbps and 880 Mbps, including the
existing headroom/traffic-demand rule. The requested codec rates were
400 and 800 Mbps; negotiation reported 398,988 and 798,988 kbps.

The installed service log ended in `CLIENT DISCONNECTED` before every
GPU batch. Other worktrees were compiling and running GPU tests. Runs
alternated A/B/A/B; a port collision interrupted an intermediate pair,
which is excluded. The repeat used unused fixture ports 51618–51644.
Complete transport runs are `target/pw-pipeline/transport-{before,after}-{1,3}`;
per-stage summaries are in `target/pw-pipeline/transport-summary.json`.
These local artifacts are not shipped.

Mean time per frame in the first complete pair, milliseconds (before → after):

| Stage | 1080p120 / 400 Mbps | 4K60 / 800 Mbps |
|---|---:|---:|
| Mapped access and SDK packetizing | 0.554 → 0.538 | 1.168 → 1.162 |
| Record packing | 0.239 → 0.231 | 0.968 → 0.877 |
| Adaptive detail FEC observation | 0.299 → 0.212 | 0.929 → 0.124 |
| Layout and FEC planning | 0.035 → 0.042 | 0.174 → 0.266 |
| RTP shard allocation/copying | 0.037 → 0.046 | 0.128 → 0.102 |
| Parity computation | 0.001 → 0.002 | 0.007 → 0.007 |
| Headers and encryption | 0.296 → 0.299 | 0.871 → 0.804 |
| Pacing waits | 7.439 → 7.598 | 12.120 → 11.438 |
| Socket sends | 1.264 → 1.455 | 3.393 → 2.744 |

Stage means use each stage's completed frames; frames replaced while
waiting for the sender mean these columns are not one additive timeline.
The large 4K detail-observation change also reflects more frames arriving
within the nominal interval, where the existing controller skips hashing.
It is not a measurement of the map replacement alone. In the second pair,
4K record packing was 1.198 → 1.345 ms and socket sends 4.281 → 4.705 ms,
showing the background-load variation.

Ranges of the two run means, milliseconds:

| Measurement | 1080p before | 1080p after | 4K before | 4K after |
|---|---:|---:|---:|---:|
| Encoder return → first send batch | 5.18–5.23 | 4.35–4.47 | 9.96–10.77 | 2.06–8.84 |
| Encoder return → last send | 13.60–13.61 | 13.38–13.71 | 25.33–26.99 | 17.03–26.05 |
| Present → first send batch | 6.50–8.07 | 5.68–5.80 | 13.02–13.97 | 6.93–14.23 |
| Present → last send | 14.87–16.49 | 14.72–15.05 | 28.39–30.20 | 21.90–31.44 |

Present-to-last-send p95 ranged from 18.77–23.23 ms before to
18.71–19.10 ms after at 1080p, and 36.42–37.96 ms before to
30.91–41.09 ms after at 4K. The stream's own rolling `stream timings`
reported mean present-to-send of 14.85–19.41 → 14.60–14.85 ms and
28.49–30.56 → 20.48–31.34 ms respectively. This supports earlier first
sends after encoder return, but not a consistent 4K present-latency or
1080p completion improvement under this shared load.

A CPU replay separates preparation from that background GPU/capture load.
It used identical synthetic, structurally valid records (not encoded
pictures), 419,536 / 1,679,240 raw bytes,
and verified both packers produced identical framed bytes. It alternated
the old/new packer and controller and the eager/incremental packet APIs
150 times per size after ten warmups, with encrypted 1360-byte packetizer
settings and 20% critical FEC. Detail observation used unchanged frames
20 ms apart. This measures CPU work, not socket or presentation latency:

| Replay stage, mean / p95 ms | 1080p before | 1080p after | 4K before | 4K after |
|---|---:|---:|---:|---:|
| Record packing | 0.403 / 0.542 | 0.307 / 0.405 | 1.177 / 1.481 | 1.067 / 1.380 |
| Detail observation | 0.275 / 0.364 | 0.200 / 0.268 | 1.164 / 1.418 | 0.614 / 0.822 |
| First block ready, including planning | 0.489 / 0.583 | 0.064 / 0.099 | 1.749 / 2.051 | 0.214 / 0.279 |

The eager API must finish every block before returning the first. Building
and releasing all blocks still took 0.531 → 0.467 ms and 1.828 → 1.791 ms;
most of the first-block saving moves later work out of its way. The replay
and its log remain in `target/pw-pipeline/pyrowave_pipeline_probe.rs` and
`target/pw-pipeline/cpu-replay.log`.

Pacing remains the largest cost. The existing sender already uses
`WSASendMsg` scatter/gather with UDP segmentation and `Timer::until_precise`;
there is no additional payload concatenation in the socket path. Sending
at FEC boundaries increased mean socket calls from about 7 to 9 at 1080p
and 21 to 22 at 4K. Critical parity cost only microseconds in these runs,
so no parity worker or timer change was added. The SDK still packetizes
into its reusable CPU buffer before record packing; skipping that copy
would require replacing SDK packetization, not simply borrowing its raw
mapped buffer. Encryption remains in place in the final shards.

Compatibility checks preserve the ordering required by
[Nonary's pinned FEC queue](https://github.com/Nonary/moonlight-common-c/blob/d6a11bc685b41037b352a96f29d08276fe5359ba/src/RtpVideoQueue.c)
and the critical-packet count and restart markers used by its
[depacketizer](https://github.com/Nonary/moonlight-common-c/blob/d6a11bc685b41037b352a96f29d08276fe5359ba/src/VideoDepacketizer.c).
Twelve golden packet fingerprints were regenerated from the baseline
source and match, including framing, critical/detail parity, encryption,
unaligned payload fallback, minimum parity and sequence/frame wraps over
two consecutive frames. Separate tests cover abandoning a partial frame
and the old controller's exact decisions through record reordering,
sequence changes, motion, a nominal-cadence reset and a pause.

The final release build generated all 24 native record/container transport
fixtures. `tests/pyrowave_transport.py` matched parity against the original
C++ nanors DLL, recovered 12 deliberately lost coarse shards across the
protected fixtures, and decoded every recovered frame with the independent
vendor decoder. The final encrypted live decoder runs also passed the
receiver's interoperability checks: 1,180/1,180 frames at 1080p and 537/537
at 4K, with zero partial frames or decoding failures. They do not establish
120/60 fps playback. Earlier shared-GPU decoder runs logged queue overflows
and failures in both versions; the latency comparison therefore uses the
transport-only receiver described above.

The generic e2e evaluator still returns failure because this receiver
does not emit its picture/motion/tone timing fields. These are transport
and PyroWave decode checks, not a strict e2e evaluator pass. Final checks:
`cargo fmt --all`; clippy for core, host and Windows with all targets and
warnings denied; 158 core tests passed; 46 host tests passed with two
pre-existing ignored tests; and the release host/examples build passed.
No web files changed. The installed service and display configuration
were not changed.

## October 7: input on the control thread

Every client's input passes through one control thread: ENet receives the
datagram, the host decrypts and decodes it, and the injector hands it to
Windows or the virtual gamepad driver. Five things on that thread made
input wait; each is now changed and measured against the code before it
(`72b8b0cc`).

Ryzen 7 5800X3D (16 threads), Windows 11 26200, release builds of
`rust/windows/examples/input_path_probe.rs` and
`rust/host/examples/enet_input_probe.rs`, with the stream's process setup
(HIGH priority class, 1 ms timer resolution, power throttling off). Load is
a child process of normal priority class spinning one thread per logical
CPU at the named thread priority, standing in for a CPU-bound game. Mouse
input is always a zero-distance relative move. The pad sections plug one
neutral VHF pad and remove it, and ran while no client streamed. Network
rows are loopback: they include the host's socket calls and scheduling,
but no NIC, interrupt moderation or wire.

### Virtual gamepad calls

Each pad call is a blocking DeviceIoControl into the VHF driver, which
runs in WUDFHost at normal priority. The raw calls (`vhf`, 1,000 states per
round at 1 kHz, 500 feedback polls):

| Load | State p50 | State max | Feedback poll max |
|---|---|---|---|
| None | 136 µs | 0.25, 0.28 ms | 0.12 ms |
| Normal | 53-55 µs | 6.8, 21.9 ms | 11.1 ms |
| Highest | 54-56 µs | 279, 229 ms | 0.34 ms |

They ran on the control thread: every controller state, the feedback poll
every 8 ms and the driver's first open (2.9-6.2 ms) and plug. A pad now
has its own thread; the control thread queues its events and collects
feedback. `pad-contention` sends one controller state and one mouse move
per millisecond through the injector for 6 s, with the 8 ms refresh, and
records when the mouse move is done after its pass began:

| Load | Before p50 / p99 / max | After p50 / p99 / max |
|---|---|---|
| None | 230 µs / 1.37 ms / 6.2 ms | 45 µs / 0.39 ms / 1.8 ms |
| Normal | 107 µs / 2.11 ms / 27.6 ms | 41 µs / 0.12 ms / 2.1 ms |
| Highest | 92 µs / 2.12 ms / 287 ms | 39 µs / 0.13 ms / 2.0 ms |

Queueing a state takes 3-6 µs (max 125 µs); the rest is SendInput itself
(p50 35-39 µs, max 1.8-2.1 ms). A rerun with `--trace` showed the pad
plugged and no driver call failing on its thread. The driver's own
stalls remain and now delay only that pad's input. Raising WUDFHost's
priority would shorten them but changes another process; it is left as
it is. One earlier run of the new code, before the probe timed controller
and mouse separately, recorded a single 344 ms pass with no load; four
later runs without load did not repeat it, and its cause was not found.

### Input thread priority

The control thread registered with MMCSS and then set its own thread
priority, which cancels the boost. `priority` reads the thread's effective
priority (13 without either); `contention` sends loopback datagrams to a
thread waiting as the control loop does, with TIME_CRITICAL spinners on
every CPU, 1,500 each:

| Thread setup | Priority | p50 | p99 | max |
|---|---|---|---|---|
| `Priority::new` (control thread before) | 14 | 3,541 µs | 20,138 µs | 26,062 µs |
| `Priority::input` (control thread now) | 18 | 17 µs | 30 µs | 287 µs |

The run before the change gave 3,535 / 23,274 / 26,983 µs for
`Priority::new`. The gamepad thread uses `Priority::input` too. Capture,
encode, audio and pacing threads still use `Priority::new`; whether they
should keep their boost needs its own measurement beside a game.

### ENet acknowledgement

rusty_enet's `service()` sends a packet's acknowledgement (one sendto,
22-24 µs here) before it returns the packet. The control socket now holds
what ENet sends until the pass has applied its input, then sends it in
order before `host.flush()`. `enet_input_probe`, 3,000 reliable 36-byte
packets on loopback, two alternating runs per mode, median:

| | Acknowledged inside `service()` | Held until after apply |
|---|---|---|
| Client send to server apply step | 69, 71 µs | 46, 46 µs |
| Server wake to first event | 31, 33 µs | 9, 9 µs |

On a real network the sendto goes through the NIC driver instead of
loopback; that cost was not measured.

### SendInput per pass

A SendInput call costs about the same for one input as for eight. The
injector now sends a pass's keyboard and mouse input in one call, in
order; touch, pen, and a key or button whose handling depends on an
unsent press of its kind, split the pass. `batch`, zero-distance moves,
600 passes per row, alternating:

| Events per pass | One call each, p50 / p99 | One call per pass, p50 / p99 |
|---|---|---|
| 1 | 37 / 157 µs | 36 / 121 µs |
| 2 | 62 / 339 µs | 33 / 198 µs |
| 4 | 98 / 347 µs | 31 / 212 µs |
| 8 | 180 / 577 µs | 38 / 270 µs |

Mouse moves within a pass were already merged, so this helps a move with
a click or scroll, key combinations and text, which was one call per
character.

### One-off calls

Finding the display for absolute input enumerates every display
(`display_rect`: p50 1.6 ms, max 2.6 ms; `display::monitors` once 21 ms).
It ran when the injector was made with the first input, every 500 ms while
the stream's display did not exist, and on every pass after a rename until
the new name was found. Lookups now run on their own thread; absolute
mouse, touch and pen wait for one still running, other input never does.
The injector is made when the session is first seen.

| `injector` | Before | After |
|---|---|---|
| `Injector::new_options`, existing display, p50 | 3,419 µs | 69 µs |
| `Injector::new_options`, missing display, p50 | 4,069 µs | 68 µs |
| `set_output` + `refresh` per 1 ms pass, display missing, 3 s, max | 18,333 µs | 65 µs |
| Passes over 200 µs | 5 | 0 |

Synthetic touch and pen devices (2.0-3.0 ms to create) are still made on
the first touch or pen event. While a synthetic touch device exists,
Windows reports touch to every application: `SM_MAXIMUMTOUCHES` went from
0 to 32 and `SM_DIGITIZER` from 0 to 193 (integrated multi-touch, ready),
and back after it was destroyed. Making one ahead of time would change
what a game sees in sessions that never touch.

Not measured here: a Moonlight client on a real network, a physical
controller, and the release end-to-end check, which needs the installed
host.
## October 7, 2026: AMF rate control and recovery keyframes

The rc.19 loss report motivated this test: a recovery keyframe can be several
ordinary frames of data, making a congested wireless route worse. These are
local encoder measurements on an AMD Radeon RX 7900 XT, driver
32.0.31041.1004, AMF runtime 1.5.2.0, Rust 1.98.1 on Windows GNU. They do not measure a Wi-Fi link, audio,
decoded picture quality, or client presentation age. RDNA4 was not available.

### Method

`rust/windows/examples/performance.rs` now reports non-keyframe size mean,
p50, p99 and maximum, startup keyframe bytes, each requested recovery
keyframe's bytes, and maximum recovery size divided by mean non-keyframe
size. AV1 keyframes occupy the same `idr_bytes` field as HEVC IDRs.
Percentiles use the sorted sample at `floor((N-1)*p)`. Encode time here is
submission through observed completed output, including GPU conversion and
output polling, not just time inside the codec. The probe drains outstanding
output before reporting, so its submission and completion counts can be
compared. No network traffic is generated.

The main matrix uses the release build, 16 deterministic moving synthetic
pictures, SDR 8-bit 4:2:0, GPU input, two seconds of paced warmup, and eight
seconds of measurement. It requests a recovery keyframe every nominal
second. Startup is measured separately, including the first keyframe before
warmup. Ordinary frames following a recovery request remain in the
non-keyframe distribution: recovering its bit budget is part of rate-control
behaviour. Runs normally complete 481 frames at 60 fps or 961 at 120 fps;
the last scheduled submission is drained after the eight-second interval.

Each mode runs the settings in order and then in reverse order, keeping
comparisons close and exposing run-to-run spread. Before and after every
run, the installed service log is checked for `CLIENT CONNECTED` without a
later `CLIENT DISCONNECTED`; other running performance probes are recorded.
The installed service, driver, display settings and other processes are not
changed. The local JSON results, settings logs and run manifests are in
`target/qa/ratecontrol/`; the tables below retain the per-run measurements.

From the worktree root, after sourcing the supplied `rust-env.ps1`:

```powershell
cargo build --release -p butterpollo-windows --example performance --target-dir target\qa
Copy-Item "$env:BUTTERPOLLO_PYROWAVE_ROOT\bin\libpyrowave-shared-0.dll" target\qa\release\examples\
New-Item -ItemType Directory -Force target\qa\ratecontrol | Out-Null
Set-Content target\qa\ratecontrol\cap2.conf 'amd_max_frame_size=2'
$env:RUST_LOG = 'info'
target\qa\release\examples\performance.exe --encoder amf --synthetic 16 --paced --seconds 8 --width 3840 --height 2160 --fps 60 --bitrate 40000 --codec hevc --idr-interval 60 --config target\qa\ratecontrol\cap2.conf
```

The defaults are ultra-low-latency usage, speed, latency-constrained VBR,
VBAQ on, pre-analysis off, LTR off, HRD off and no filler or frame skipping.
The Rust host sets frame rate and target bitrate at startup. Before this
change it did not explicitly set peak bitrate or VBV there: on this driver
VBR derives a 1.2× peak and one frame of VBV, whereas CBR derives a 1× peak
and **one second** of VBV. The existing bitrate-update path attempts to scale the
driver's positive rate-control values; its AV1 frame-cap property name is
now corrected from `Av1MaxAUSize` to `Av1MaxCompressedFrameSize`.

The new optional limits are applied after frame rate and target bitrate and
read back before Init. The settings line also reads the actual cap and intra
refresh properties after Init; an accepted property is not proof that every
frame obeys it. Explicit rejected requests produce an AMF setting error.
`LowLatencyInternal` and `InputQueueSize` remain untouched by default.

| Table setting | Configuration added to the defaults |
|---|---|
| default | none |
| cbr | `amd_rc=cbr` |
| cbr_vbv1 | `amd_rc=cbr`, `amd_vbv_buffer_frames=1` |
| peak1 / peak15 / peak2 | `amd_peak_bitrate_ratio=1` / `1.5` / `2` |
| vbv05 / vbv1 / vbv2 | `amd_vbv_buffer_frames=0.5` / `1` / `2` |
| cap1 / cap2 / cap4 | `amd_max_frame_size=1` / `2` / `4` |
| hrd | `amd_enforce_hrd=true` |
| cap2hrd | `amd_max_frame_size=2`, `amd_enforce_hrd=true` |
| intra | `--intra-refresh --idr-interval 0`; continuous refresh, no forced recovery keyframes |
| intra_idr | `--intra-refresh` with the same once-per-second recovery requests |

Frame budgets are bitrate divided by the exact negotiated frame rate. AMF
expects bits for VBV, `MaxAUSize`, `HevcMaxAUSize` and
`Av1MaxCompressedFrameSize`; the tables report bytes. The names and units
match the retained SDK and AMD's [HEVC header](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/public/include/components/VideoEncoderHEVC.h)
and [AV1 header](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/public/include/components/VideoEncoderAV1.h).

### Screening: 4K60 at 40 Mbps

The initial screening used a development build with 20 paced warmup frames
and eight seconds of measurement. Builds overlapped some of these runs, so
the release matrix is the basis for timing comparisons. Its byte-size
comparisons are repeatable; each second pass reverses the settings order.

| Mode / Mbps | Setting / run | Non-keyframes | Steady mean / p50 / p99 / max (bytes) | Startup bytes / steady mean | Recovery max bytes / steady mean | Encode mean / p99 (ms) |
|---|---|---:|---|---:|---|---|
| HEVC 3840×2160@60 / 40 | default / 1 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.117 / 6.817 |
| HEVC 3840×2160@60 / 40 | cbr / 1 | 473 | 79849 / 77566 / 143632 / 158407 | 615045 / 7.70× | 242453 / 3.04× | 5.932 / 6.818 |
| HEVC 3840×2160@60 / 40 | peak1 / 1 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.144 / 6.822 |
| HEVC 3840×2160@60 / 40 | peak15 / 1 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.269 / 6.885 |
| HEVC 3840×2160@60 / 40 | peak2 / 1 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.206 / 6.818 |
| HEVC 3840×2160@60 / 40 | vbv05 / 1 | 473 | 78514 / 78002 / 92474 / 105469 | 213837 / 2.72× | 278811 / 3.55× | 6.233 / 6.842 |
| HEVC 3840×2160@60 / 40 | vbv1 / 1 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.226 / 6.878 |
| HEVC 3840×2160@60 / 40 | vbv2 / 1 | 473 | 80504 / 80229 / 98435 / 105270 | 293486 / 3.65× | 248412 / 3.09× | 6.185 / 6.784 |
| HEVC 3840×2160@60 / 40 | cap1 / 1 | 473 | 79400 / 79008 / 93301 / 94779 | 178313 / 2.25× | 92557 / 1.17× | 6.186 / 6.819 |
| HEVC 3840×2160@60 / 40 | cap2 / 1 | 473 | 79194 / 78595 / 93307 / 93494 | 245474 / 3.10× | 167152 / 2.11× | 6.165 / 6.800 |
| HEVC 3840×2160@60 / 40 | cap4 / 1 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.180 / 6.802 |
| HEVC 3840×2160@60 / 40 | hrd / 1 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.191 / 6.792 |
| HEVC 3840×2160@60 / 40 | cap2hrd / 1 | 473 | 79194 / 78595 / 93307 / 93494 | 245474 / 3.10× | 167152 / 2.11× | 6.185 / 6.858 |
| HEVC 3840×2160@60 / 40 | intra / 1 | 481 | 82031 / 80867 / 93440 / 94347 | 245658 / 2.99× | — | 6.236 / 6.933 |
| HEVC 3840×2160@60 / 40 | intra_idr / 1 | 473 | 79901 / 79312 / 93137 / 96257 | 245658 / 3.07× | 243298 / 3.04× | 6.191 / 6.797 |
| HEVC 3840×2160@60 / 40 | intra_idr / 2 | 473 | 79901 / 79312 / 93137 / 96257 | 245658 / 3.07× | 243298 / 3.04× | 6.170 / 6.758 |
| HEVC 3840×2160@60 / 40 | intra / 2 | 481 | 82031 / 80867 / 93440 / 94347 | 245658 / 2.99× | — | 6.086 / 6.677 |
| HEVC 3840×2160@60 / 40 | cap2hrd / 2 | 473 | 79194 / 78595 / 93307 / 93494 | 245474 / 3.10× | 167152 / 2.11× | 6.122 / 6.712 |
| HEVC 3840×2160@60 / 40 | hrd / 2 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 5.999 / 6.404 |
| HEVC 3840×2160@60 / 40 | cap4 / 2 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.087 / 6.711 |
| HEVC 3840×2160@60 / 40 | cap2 / 2 | 473 | 79194 / 78595 / 93307 / 93494 | 245474 / 3.10× | 167152 / 2.11× | 6.105 / 6.758 |
| HEVC 3840×2160@60 / 40 | cap1 / 2 | 473 | 79400 / 79008 / 93301 / 94779 | 178313 / 2.25× | 92557 / 1.17× | 6.144 / 6.772 |
| HEVC 3840×2160@60 / 40 | vbv2 / 2 | 473 | 80504 / 80229 / 98435 / 105270 | 293486 / 3.65× | 248412 / 3.09× | 6.233 / 6.826 |
| HEVC 3840×2160@60 / 40 | vbv1 / 2 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.205 / 6.819 |
| HEVC 3840×2160@60 / 40 | vbv05 / 2 | 473 | 78514 / 78002 / 92474 / 105469 | 213837 / 2.72× | 278811 / 3.55× | 6.242 / 6.842 |
| HEVC 3840×2160@60 / 40 | peak2 / 2 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.291 / 6.887 |
| HEVC 3840×2160@60 / 40 | peak15 / 2 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.155 / 6.796 |
| HEVC 3840×2160@60 / 40 | peak1 / 2 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.077 / 6.873 |
| HEVC 3840×2160@60 / 40 | cbr / 2 | 473 | 79849 / 77566 / 143632 / 158407 | 615045 / 7.70× | 242453 / 3.04× | 5.988 / 6.704 |
| HEVC 3840×2160@60 / 40 | default / 2 | 473 | 79908 / 79312 / 93172 / 96174 | 245474 / 3.07× | 242986 / 3.04× | 6.009 / 6.587 |
| AV1 3840×2160@60 / 40 | default / 1 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 4.967 / 5.663 |
| AV1 3840×2160@60 / 40 | cbr / 1 | 473 | 82409 / 81892 / 128763 / 141099 | 307290 / 3.73× | 162898 / 1.98× | 5.042 / 5.701 |
| AV1 3840×2160@60 / 40 | peak1 / 1 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 4.919 / 5.576 |
| AV1 3840×2160@60 / 40 | peak15 / 1 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 5.033 / 5.770 |
| AV1 3840×2160@60 / 40 | peak2 / 1 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 4.885 / 5.449 |
| AV1 3840×2160@60 / 40 | vbv05 / 1 | 473 | 78369 / 79091 / 93732 / 99924 | 158745 / 2.03× | 155231 / 1.98× | 4.908 / 5.713 |
| AV1 3840×2160@60 / 40 | vbv1 / 1 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 4.958 / 5.477 |
| AV1 3840×2160@60 / 40 | vbv2 / 1 | 473 | 79098 / 79399 / 105131 / 115271 | 225386 / 2.85× | 160404 / 2.03× | 4.988 / 5.808 |
| AV1 3840×2160@60 / 40 | cap1 / 1 | 473 | 78197 / 79029 / 93730 / 94345 | 132770 / 1.70× | 81211 / 1.04× | 5.040 / 5.714 |
| AV1 3840×2160@60 / 40 | cap2 / 1 | 473 | 78997 / 79251 / 103113 / 111306 | 185400 / 2.35× | 152792 / 1.93× | 4.982 / 5.605 |
| AV1 3840×2160@60 / 40 | cap4 / 1 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 5.058 / 5.787 |
| AV1 3840×2160@60 / 40 | hrd / 1 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 4.953 / 5.754 |
| AV1 3840×2160@60 / 40 | cap2hrd / 1 | 473 | 78997 / 79251 / 103113 / 111306 | 185400 / 2.35× | 152792 / 1.93× | 5.044 / 5.802 |
| AV1 3840×2160@60 / 40 | intra / 1 | 481 | 80105 / 79238 / 93845 / 94157 | 185400 / 2.31× | — | 4.904 / 5.434 |
| AV1 3840×2160@60 / 40 | intra_idr / 1 | 473 | 78811 / 79184 / 105086 / 112245 | 185400 / 2.35× | 153888 / 1.95× | 5.017 / 5.789 |
| AV1 3840×2160@60 / 40 | intra_idr / 2 | 473 | 78811 / 79184 / 105086 / 112245 | 185400 / 2.35× | 153888 / 1.95× | 4.964 / 5.496 |
| AV1 3840×2160@60 / 40 | intra / 2 | 481 | 80105 / 79238 / 93845 / 94157 | 185400 / 2.31× | — | 4.948 / 5.575 |
| AV1 3840×2160@60 / 40 | cap2hrd / 2 | 473 | 78997 / 79251 / 103113 / 111306 | 185400 / 2.35× | 152792 / 1.93× | 5.082 / 5.799 |
| AV1 3840×2160@60 / 40 | hrd / 2 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 5.092 / 5.856 |
| AV1 3840×2160@60 / 40 | cap4 / 2 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 5.113 / 5.856 |
| AV1 3840×2160@60 / 40 | cap2 / 2 | 473 | 78997 / 79251 / 103113 / 111306 | 185400 / 2.35× | 152792 / 1.93× | 4.972 / 5.630 |
| AV1 3840×2160@60 / 40 | cap1 / 2 | 473 | 78197 / 79029 / 93730 / 94345 | 132770 / 1.70× | 81211 / 1.04× | 4.950 / 5.761 |
| AV1 3840×2160@60 / 40 | vbv2 / 2 | 473 | 79098 / 79399 / 105131 / 115271 | 225386 / 2.85× | 160404 / 2.03× | 4.983 / 5.564 |
| AV1 3840×2160@60 / 40 | vbv1 / 2 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 5.024 / 5.867 |
| AV1 3840×2160@60 / 40 | vbv05 / 2 | 473 | 78369 / 79091 / 93732 / 99924 | 158745 / 2.03× | 155231 / 1.98× | 5.104 / 5.804 |
| AV1 3840×2160@60 / 40 | peak2 / 2 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 5.011 / 5.861 |
| AV1 3840×2160@60 / 40 | peak15 / 2 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 4.976 / 5.598 |
| AV1 3840×2160@60 / 40 | peak1 / 2 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 5.203 / 5.928 |
| AV1 3840×2160@60 / 40 | cbr / 2 | 473 | 82409 / 81892 / 128763 / 141099 | 307290 / 3.73× | 162898 / 1.98× | 5.055 / 5.755 |
| AV1 3840×2160@60 / 40 | default / 2 | 473 | 79001 / 79208 / 103293 / 113084 | 185400 / 2.35× | 153531 / 1.94× | 5.087 / 5.899 |

### Release matrix

Rows marked **†** overlap a live service stream (15:50:54–15:51:34 UTC). They are retained as raw observations, excluded from latency comparisons, and repeated below. The AV1 4K40 default run completed 455 frames and the HEVC 4K80 cap1 run 448; all other matrix runs completed the usual 481 or 961. Every submitted frame was returned.

| Mode / Mbps | Setting / run | Non-keyframes | Steady mean / p50 / p99 / max (bytes) | Startup bytes / steady mean | Recovery max bytes / steady mean | Encode mean / p99 (ms) |
|---|---|---:|---|---:|---|---|
| HEVC 1920×1080@60 / 20 | default / 1 | 473 | 40681 / 41542 / 48500 / 49616 | 90690 / 2.23× | 96594 / 2.37× | 2.284 / 2.779 |
| HEVC 1920×1080@60 / 20 | cbr / 1 | 473 | 41054 / 41130 / 52972 / 55554 | 154192 / 3.76× | 80037 / 1.95× | 2.161 / 2.747 |
| HEVC 1920×1080@60 / 20 | cbr_vbv1 / 1 | 473 | 41038 / 41302 / 49118 / 50360 | 154192 / 3.76× | 86468 / 2.11× | 2.149 / 2.728 |
| HEVC 1920×1080@60 / 20 | cap1 / 1 | 473 | 40361 / 40935 / 48345 / 48722 | 77843 / 1.93× | 47530 / 1.18× | 2.250 / 2.888 |
| HEVC 1920×1080@60 / 20 | cap2 / 1 | 473 | 40669 / 41544 / 48338 / 50587 | 90690 / 2.23× | 92231 / 2.27× | 2.217 / 2.854 |
| HEVC 1920×1080@60 / 20 | intra / 1 | 481 | 41309 / 42372 / 48517 / 48668 | 90746 / 2.20× | — | 2.199 / 2.787 |
| HEVC 1920×1080@60 / 20 | intra / 2 | 481 | 41309 / 42372 / 48517 / 48668 | 90746 / 2.20× | — | 2.254 / 3.214 |
| HEVC 1920×1080@60 / 20 | cap2 / 2 | 473 | 40669 / 41544 / 48338 / 50587 | 90690 / 2.23× | 92231 / 2.27× | 2.278 / 2.901 |
| HEVC 1920×1080@60 / 20 | cap1 / 2 | 473 | 40361 / 40935 / 48345 / 48722 | 77843 / 1.93× | 47530 / 1.18× | 2.448 / 3.190 |
| HEVC 1920×1080@60 / 20 | cbr_vbv1 / 2 | 473 | 41038 / 41302 / 49118 / 50360 | 154192 / 3.76× | 86468 / 2.11× | 2.423 / 2.980 |
| HEVC 1920×1080@60 / 20 | cbr / 2 | 473 | 41054 / 41130 / 52972 / 55554 | 154192 / 3.76× | 80037 / 1.95× | 2.410 / 2.852 |
| HEVC 1920×1080@60 / 20 | default / 2 | 473 | 40681 / 41542 / 48500 / 49616 | 90690 / 2.23× | 96594 / 2.37× | 2.391 / 3.216 |
| AV1 1920×1080@60 / 20 | default / 1 | 473 | 41146 / 41475 / 47605 / 56855 | 66499 / 1.62× | 76640 / 1.86× | 1.959 / 2.145 |
| AV1 1920×1080@60 / 20 | cbr / 1 | 473 | 41384 / 40699 / 55727 / 59922 | 79012 / 1.91× | 63611 / 1.54× | 1.851 / 2.036 |
| AV1 1920×1080@60 / 20 | cbr_vbv1 / 1 | 473 | 41141 / 41258 / 49984 / 54971 | 79012 / 1.92× | 64778 / 1.57× | 1.886 / 2.241 |
| AV1 1920×1080@60 / 20 | cap1 / 1 | 473 | 40883 / 41081 / 47339 / 54164 | 56286 / 1.38× | 43347 / 1.06× | 1.822 / 1.996 |
| AV1 1920×1080@60 / 20 | cap2 / 1 | 473 | 41146 / 41475 / 47605 / 56855 | 66499 / 1.62× | 76640 / 1.86× | 1.782 / 1.905 |
| AV1 1920×1080@60 / 20 | intra / 1 | 481 | 41235 / 41608 / 46994 / 47452 | 66499 / 1.61× | — | 1.827 / 2.072 |
| AV1 1920×1080@60 / 20 | intra / 2 | 481 | 41235 / 41608 / 46994 / 47452 | 66499 / 1.61× | — | 1.845 / 2.024 |
| AV1 1920×1080@60 / 20 | cap2 / 2 | 473 | 41146 / 41475 / 47605 / 56855 | 66499 / 1.62× | 76640 / 1.86× | 1.858 / 2.062 |
| AV1 1920×1080@60 / 20 | cap1 / 2 | 473 | 40883 / 41081 / 47339 / 54164 | 56286 / 1.38× | 43347 / 1.06× | 1.745 / 1.954 |
| AV1 1920×1080@60 / 20 | cbr_vbv1 / 2 | 473 | 41141 / 41258 / 49984 / 54971 | 79012 / 1.92× | 64778 / 1.57× | 1.818 / 1.957 |
| AV1 1920×1080@60 / 20 | cbr / 2 | 473 | 41384 / 40699 / 55727 / 59922 | 79012 / 1.91× | 63611 / 1.54× | 1.818 / 2.117 |
| AV1 1920×1080@60 / 20 | default / 2 | 473 | 41146 / 41475 / 47605 / 56855 | 66499 / 1.62× | 76640 / 1.86× | 1.804 / 1.960 |
| HEVC 1920×1080@60 / 40 | default / 1 | 473 | 81591 / 82190 / 95755 / 98074 | 109896 / 1.35× | 165972 / 2.03× | 2.238 / 2.825 |
| HEVC 1920×1080@60 / 40 | cbr / 1 | 473 | 82498 / 81777 / 96972 / 102106 | 154192 / 1.87× | 136004 / 1.65× | 2.283 / 2.888 |
| HEVC 1920×1080@60 / 40 | cbr_vbv1 / 1 | 473 | 81987 / 82608 / 92698 / 93796 | 154192 / 1.88× | 140645 / 1.72× | 2.207 / 2.734 |
| HEVC 1920×1080@60 / 40 | cap1 / 1 | 473 | 81706 / 82426 / 94881 / 95460 | 109896 / 1.35× | 96997 / 1.19× | 2.178 / 2.827 |
| HEVC 1920×1080@60 / 40 | cap2 / 1 | 473 | 81683 / 82237 / 94522 / 97190 | 109896 / 1.35× | 153716 / 1.88× | 2.287 / 2.796 |
| HEVC 1920×1080@60 / 40 | intra / 1 | 481 | 82018 / 82519 / 94952 / 96880 | 109929 / 1.34× | — | 2.228 / 2.791 |
| HEVC 1920×1080@60 / 40 | intra / 2 | 481 | 82018 / 82519 / 94952 / 96880 | 109929 / 1.34× | — | 2.341 / 2.848 |
| HEVC 1920×1080@60 / 40 | cap2 / 2 | 473 | 81683 / 82237 / 94522 / 97190 | 109896 / 1.35× | 153716 / 1.88× | 2.338 / 2.877 |
| HEVC 1920×1080@60 / 40 | cap1 / 2 | 473 | 81706 / 82426 / 94881 / 95460 | 109896 / 1.35× | 96997 / 1.19× | 2.290 / 2.865 |
| HEVC 1920×1080@60 / 40 | cbr_vbv1 / 2 | 473 | 81987 / 82608 / 92698 / 93796 | 154192 / 1.88× | 140645 / 1.72× | 2.357 / 3.008 |
| HEVC 1920×1080@60 / 40 | cbr / 2 | 473 | 82498 / 81777 / 96972 / 102106 | 154192 / 1.87× | 136004 / 1.65× | 2.396 / 3.227 |
| HEVC 1920×1080@60 / 40 | default / 2 | 473 | 81591 / 82190 / 95755 / 98074 | 109896 / 1.35× | 165972 / 2.03× | 2.353 / 3.152 |
| AV1 1920×1080@60 / 40 | default / 1 | 473 | 82772 / 83111 / 100624 / 105872 | 71907 / 0.87× | 120405 / 1.45× | 1.972 / 2.256 |
| AV1 1920×1080@60 / 40 | cbr / 1 | 473 | 83206 / 84552 / 97402 / 97986 | 79012 / 0.95× | 100260 / 1.20× | 1.873 / 2.127 |
| AV1 1920×1080@60 / 40 | cbr_vbv1 / 1 | 473 | 82008 / 84113 / 94902 / 95264 | 79012 / 0.96× | 97148 / 1.18× | 1.843 / 2.080 |
| AV1 1920×1080@60 / 40 | cap1 / 1 | 473 | 82055 / 82708 / 100503 / 101143 | 71907 / 0.88× | 77800 / 0.95× | 1.814 / 2.001 |
| AV1 1920×1080@60 / 40 | cap2 / 1 | 473 | 82772 / 83111 / 100624 / 105872 | 71907 / 0.87× | 120405 / 1.45× | 1.877 / 2.057 |
| AV1 1920×1080@60 / 40 | intra / 1 | 481 | 82726 / 83067 / 100639 / 101241 | 71907 / 0.87× | — | 1.881 / 2.091 |
| AV1 1920×1080@60 / 40 | intra / 2 | 481 | 82726 / 83067 / 100639 / 101241 | 71907 / 0.87× | — | 1.901 / 2.075 |
| AV1 1920×1080@60 / 40 | cap2 / 2 | 473 | 82772 / 83111 / 100624 / 105872 | 71907 / 0.87× | 120405 / 1.45× | 1.894 / 2.107 |
| AV1 1920×1080@60 / 40 | cap1 / 2 | 473 | 82055 / 82708 / 100503 / 101143 | 71907 / 0.88× | 77800 / 0.95× | 1.874 / 2.035 |
| AV1 1920×1080@60 / 40 | cbr_vbv1 / 2 | 473 | 82008 / 84113 / 94902 / 95264 | 79012 / 0.96× | 97148 / 1.18× | 1.861 / 2.102 |
| AV1 1920×1080@60 / 40 | cbr / 2 | 473 | 83206 / 84552 / 97402 / 97986 | 79012 / 0.95× | 100260 / 1.20× | 1.856 / 1.990 |
| AV1 1920×1080@60 / 40 | default / 2 | 473 | 82772 / 83111 / 100624 / 105872 | 71907 / 0.87× | 120405 / 1.45× | 1.877 / 2.060 |
| HEVC 1920×1080@60 / 80 | default / 1 | 473 | 163579 / 162262 / 191592 / 192365 | 127237 / 0.78× | 309871 / 1.89× | 2.344 / 2.741 |
| HEVC 1920×1080@60 / 80 | cbr / 1 | 473 | 165849 / 165127 / 176931 / 190737 | 154192 / 0.93× | 260312 / 1.57× | 2.397 / 2.823 |
| HEVC 1920×1080@60 / 80 | cbr_vbv1 / 1 | 473 | 163872 / 167352 / 184602 / 185931 | 154192 / 0.94× | 286142 / 1.75× | 2.337 / 2.727 |
| HEVC 1920×1080@60 / 80 | cap1 / 1 | 473 | 163736 / 162409 / 191766 / 192129 | 127237 / 0.78× | 187840 / 1.15× | 2.334 / 2.776 |
| HEVC 1920×1080@60 / 80 | cap2 / 1 | 473 | 163279 / 161757 / 191459 / 192365 | 127237 / 0.78× | 309675 / 1.90× | 2.366 / 2.749 |
| HEVC 1920×1080@60 / 80 | intra / 1 | 481 | 164616 / 163113 / 191800 / 192106 | 127343 / 0.77× | — | 2.307 / 2.713 |
| HEVC 1920×1080@60 / 80 | intra / 2 | 481 | 164616 / 163113 / 191800 / 192106 | 127343 / 0.77× | — | 2.287 / 2.672 |
| HEVC 1920×1080@60 / 80 | cap2 / 2 | 473 | 163279 / 161757 / 191459 / 192365 | 127237 / 0.78× | 309675 / 1.90× | 2.286 / 2.700 |
| HEVC 1920×1080@60 / 80 | cap1 / 2 | 473 | 163736 / 162409 / 191766 / 192129 | 127237 / 0.78× | 187840 / 1.15× | 2.264 / 2.678 |
| HEVC 1920×1080@60 / 80 | cbr_vbv1 / 2 | 473 | 163872 / 167352 / 184602 / 185931 | 154192 / 0.94× | 286142 / 1.75× | 2.285 / 2.718 |
| HEVC 1920×1080@60 / 80 | cbr / 2 | 473 | 165849 / 165127 / 176931 / 190737 | 154192 / 0.93× | 260312 / 1.57× | 2.319 / 2.692 |
| HEVC 1920×1080@60 / 80 | default / 2 | 473 | 163579 / 162262 / 191592 / 192365 | 127237 / 0.78× | 309871 / 1.89× | 2.298 / 2.698 |
| AV1 1920×1080@60 / 80 | default / 1 | 473 | 163248 / 162500 / 204673 / 208384 | 74987 / 0.46× | 246750 / 1.51× | 1.962 / 2.204 |
| AV1 1920×1080@60 / 80 | cbr / 1 | 473 | 165879 / 166058 / 200869 / 230081 | 79012 / 0.48× | 238012 / 1.43× | 1.977 / 2.335 |
| AV1 1920×1080@60 / 80 | cbr_vbv1 / 1 | 473 | 161297 / 166396 / 203637 / 204852 | 79012 / 0.49× | 199973 / 1.24× | 1.907 / 1.977 |
| AV1 1920×1080@60 / 80 | cap1 / 1 | 473 | 161583 / 159934 / 205139 / 205607 | 74987 / 0.46× | 137621 / 0.85× | 1.975 / 2.139 |
| AV1 1920×1080@60 / 80 | cap2 / 1 | 473 | 163522 / 163182 / 205090 / 208100 | 74987 / 0.46× | 246750 / 1.51× | 1.916 / 2.031 |
| AV1 1920×1080@60 / 80 | intra / 1 | 481 | 164629 / 163207 / 205290 / 206221 | 74987 / 0.46× | — | 2.000 / 2.343 |
| AV1 1920×1080@60 / 80 | intra / 2 | 481 | 164629 / 163207 / 205290 / 206221 | 74987 / 0.46× | — | 1.930 / 2.058 |
| AV1 1920×1080@60 / 80 | cap2 / 2 | 473 | 163522 / 163182 / 205090 / 208100 | 74987 / 0.46× | 246750 / 1.51× | 1.921 / 2.116 |
| AV1 1920×1080@60 / 80 | cap1 / 2 | 473 | 161583 / 159934 / 205139 / 205607 | 74987 / 0.46× | 137621 / 0.85× | 1.953 / 2.260 |
| AV1 1920×1080@60 / 80 | cbr_vbv1 / 2 | 473 | 161297 / 166396 / 203637 / 204852 | 79012 / 0.49× | 199973 / 1.24× | 1.984 / 2.169 |
| AV1 1920×1080@60 / 80 | cbr / 2 | 473 | 165879 / 166058 / 200869 / 230081 | 79012 / 0.48× | 238012 / 1.43× | 1.910 / 2.076 |
| AV1 1920×1080@60 / 80 | default / 2 | 473 | 163248 / 162500 / 204673 / 208384 | 74987 / 0.46× | 246750 / 1.51× | 1.926 / 2.154 |
| HEVC 2560×1440@120 / 20 | default / 1 | 953 | 20500 / 20082 / 24354 / 24406 | 65915 / 3.22× | 63289 / 3.09× | 3.157 / 3.706 |
| HEVC 2560×1440@120 / 20 | cbr / 1 | 953 | 20846 / 20221 / 26649 / 30504 | 271818 / 13.04× | 51971 / 2.49× | 3.111 / 3.616 |
| HEVC 2560×1440@120 / 20 | cbr_vbv1 / 1 | 953 | 20563 / 20171 / 25463 / 25558 | 154202 / 7.50× | 58660 / 2.85× | 3.118 / 3.668 |
| HEVC 2560×1440@120 / 20 | cap1 / 1 | 953 | 20538 / 20155 / 24346 / 24426 | 45481 / 2.21× | 20680 / 1.01× | 3.177 / 3.703 |
| HEVC 2560×1440@120 / 20 | cap2 / 1 | 953 | 20506 / 20059 / 24370 / 24406 | 65915 / 3.21× | 41048 / 2.00× | 3.111 / 3.636 |
| HEVC 2560×1440@120 / 20 | intra / 1 | 961 | 20665 / 20139 / 24343 / 24402 | 65854 / 3.19× | — | 3.095 / 3.611 |
| HEVC 2560×1440@120 / 20 | intra / 2 | 961 | 20665 / 20139 / 24343 / 24402 | 65854 / 3.19× | — | 3.119 / 3.634 |
| HEVC 2560×1440@120 / 20 | cap2 / 2 | 953 | 20506 / 20059 / 24370 / 24406 | 65915 / 3.21× | 41048 / 2.00× | 3.163 / 3.726 |
| HEVC 2560×1440@120 / 20 | cap1 / 2 | 953 | 20538 / 20155 / 24346 / 24426 | 45481 / 2.21× | 20680 / 1.01× | 3.133 / 3.667 |
| HEVC 2560×1440@120 / 20 | cbr_vbv1 / 2 | 953 | 20563 / 20171 / 25463 / 25558 | 154202 / 7.50× | 58660 / 2.85× | 3.089 / 3.577 |
| HEVC 2560×1440@120 / 20 | cbr / 2 | 953 | 20846 / 20221 / 26649 / 30504 | 271818 / 13.04× | 51971 / 2.49× | 3.136 / 3.676 |
| HEVC 2560×1440@120 / 20 | default / 2 | 953 | 20500 / 20082 / 24354 / 24406 | 65915 / 3.22× | 63289 / 3.09× | 3.158 / 3.696 |
| AV1 2560×1440@120 / 20 | default / 1 | 953 | 20544 / 20317 / 24969 / 25066 | 53028 / 2.58× | 47847 / 2.33× | 2.558 / 3.074 |
| AV1 2560×1440@120 / 20 | cbr / 1 | 953 | 20593 / 19360 / 25502 / 46398 | 134832 / 6.55× | 43867 / 2.13× | 2.547 / 3.026 |
| AV1 2560×1440@120 / 20 | cbr_vbv1 / 1 | 953 | 20823 / 20412 / 27197 / 27797 | 86642 / 4.16× | 49660 / 2.38× | 2.546 / 3.006 |
| AV1 2560×1440@120 / 20 | cap1 / 1 | 953 | 20571 / 20223 / 24976 / 25106 | 34657 / 1.68× | 23931 / 1.16× | 2.605 / 3.143 |
| AV1 2560×1440@120 / 20 | cap2 / 1 | 953 | 20553 / 20252 / 24944 / 25196 | 53028 / 2.58× | 40503 / 1.97× | 2.602 / 3.145 |
| AV1 2560×1440@120 / 20 | intra / 1 | 961 | 20649 / 20271 / 24989 / 25088 | 53028 / 2.57× | — | 2.583 / 3.138 |
| AV1 2560×1440@120 / 20 | intra / 2 | 961 | 20649 / 20271 / 24989 / 25088 | 53028 / 2.57× | — | 2.694 / 3.163 |
| AV1 2560×1440@120 / 20 | cap2 / 2 | 953 | 20553 / 20252 / 24944 / 25196 | 53028 / 2.58× | 40503 / 1.97× | 2.574 / 3.136 |
| AV1 2560×1440@120 / 20 | cap1 / 2 | 953 | 20571 / 20223 / 24976 / 25106 | 34657 / 1.68× | 23931 / 1.16× | 2.558 / 3.089 |
| AV1 2560×1440@120 / 20 | cbr_vbv1 / 2 | 953 | 20823 / 20412 / 27197 / 27797 | 86642 / 4.16× | 49660 / 2.38× | 2.737 / 3.238 |
| AV1 2560×1440@120 / 20 | cbr / 2 | 953 | 20593 / 19360 / 25502 / 46398 | 134832 / 6.55× | 43867 / 2.13× | 2.573 / 3.107 |
| AV1 2560×1440@120 / 20 | default / 2 | 953 | 20544 / 20317 / 24969 / 25066 | 53028 / 2.58× | 47847 / 2.33× | 2.573 / 3.130 |
| HEVC 2560×1440@120 / 40 | default / 1 | 953 | 40275 / 41108 / 46697 / 54280 | 120344 / 2.99× | 137967 / 3.43× | 3.170 / 3.685 |
| HEVC 2560×1440@120 / 40 | cbr / 1 | 953 | 40896 / 40339 / 51911 / 82915 | 273504 / 6.69× | 102765 / 2.51× | 3.193 / 3.693 |
| HEVC 2560×1440@120 / 40 | cbr_vbv1 / 1 | 953 | 40861 / 41148 / 48109 / 58489 | 241930 / 5.92× | 121840 / 2.98× | 3.101 / 3.624 |
| HEVC 2560×1440@120 / 40 | cap1 / 1 | 953 | 40056 / 40840 / 46687 / 47410 | 87706 / 2.19× | 44501 / 1.11× | 3.113 / 3.655 |
| HEVC 2560×1440@120 / 40 | cap2 / 1 | 953 | 40109 / 40920 / 46697 / 47624 | 120344 / 3.00× | 81830 / 2.04× | 3.145 / 3.688 |
| HEVC 2560×1440@120 / 40 | intra / 1 | 961 | 40713 / 41292 / 46793 / 47701 | 120492 / 2.96× | — | 3.155 / 3.702 |
| HEVC 2560×1440@120 / 40 | intra / 2 | 961 | 40713 / 41292 / 46793 / 47701 | 120492 / 2.96× | — | 3.168 / 3.706 |
| HEVC 2560×1440@120 / 40 | cap2 / 2 | 953 | 40109 / 40920 / 46697 / 47624 | 120344 / 3.00× | 81830 / 2.04× | 3.179 / 3.734 |
| HEVC 2560×1440@120 / 40 | cap1 / 2 | 953 | 40056 / 40840 / 46687 / 47410 | 87706 / 2.19× | 44501 / 1.11× | 3.229 / 3.761 |
| HEVC 2560×1440@120 / 40 | cbr_vbv1 / 2 | 953 | 40861 / 41148 / 48109 / 58489 | 241930 / 5.92× | 121840 / 2.98× | 3.289 / 3.869 |
| HEVC 2560×1440@120 / 40 | cbr / 2 | 953 | 40896 / 40339 / 51911 / 82915 | 273504 / 6.69× | 102765 / 2.51× | 3.256 / 3.774 |
| HEVC 2560×1440@120 / 40 | default / 2 | 953 | 40275 / 41108 / 46697 / 54280 | 120344 / 2.99× | 137967 / 3.43× | 3.153 / 3.730 |
| AV1 2560×1440@120 / 40 | default / 1 | 953 | 39980 / 40456 / 45874 / 54962 | 91912 / 2.30× | 76289 / 1.91× | 2.752 / 3.606 |
| AV1 2560×1440@120 / 40 | cbr / 1 | 953 | 41005 / 41562 / 52080 / 73551 | 137697 / 3.36× | 76697 / 1.87× | 2.628 / 3.197 |
| AV1 2560×1440@120 / 40 | cbr_vbv1 / 1 | 953 | 41309 / 39539 / 48656 / 64844 | 126261 / 3.06× | 76626 / 1.85× | 2.690 / 3.236 |
| AV1 2560×1440@120 / 40 | cap1 / 1 | 953 | 39773 / 39881 / 44503 / 47864 | 64934 / 1.63× | 38589 / 0.97× | 2.658 / 3.191 |
| AV1 2560×1440@120 / 40 | cap2 / 1 | 953 | 40002 / 40268 / 45692 / 55494 | 91912 / 2.30× | 75412 / 1.89× | 2.573 / 3.137 |
| AV1 2560×1440@120 / 40 | intra / 1 | 961 | 40053 / 40482 / 45359 / 45964 | 91912 / 2.29× | — | 2.546 / 2.758 |
| AV1 2560×1440@120 / 40 | intra / 2 | 961 | 40053 / 40482 / 45359 / 45964 | 91912 / 2.29× | — | 2.561 / 3.109 |
| AV1 2560×1440@120 / 40 | cap2 / 2 | 953 | 40002 / 40268 / 45692 / 55494 | 91912 / 2.30× | 75412 / 1.89× | 2.606 / 3.142 |
| AV1 2560×1440@120 / 40 | cap1 / 2 | 953 | 39773 / 39881 / 44503 / 47864 | 64934 / 1.63× | 38589 / 0.97× | 2.665 / 3.191 |
| AV1 2560×1440@120 / 40 | cbr_vbv1 / 2 | 953 | 41309 / 39539 / 48656 / 64844 | 126261 / 3.06× | 76626 / 1.85× | 2.784 / 4.661 |
| AV1 2560×1440@120 / 40 | cbr / 2 | 953 | 41005 / 41562 / 52080 / 73551 | 137697 / 3.36× | 76697 / 1.87× | 2.565 / 3.120 |
| AV1 2560×1440@120 / 40 | default / 2 | 953 | 39980 / 40456 / 45874 / 54962 | 91912 / 2.30× | 76289 / 1.91× | 2.563 / 3.113 |
| HEVC 2560×1440@120 / 80 | default / 1 | 953 | 79432 / 79648 / 89451 / 91833 | 164372 / 2.07× | 172584 / 2.17× | 3.316 / 3.755 |
| HEVC 2560×1440@120 / 80 | cbr / 1 | 953 | 82951 / 80834 / 97724 / 114942 | 273504 / 3.30× | 139427 / 1.68× | 3.296 / 3.749 |
| HEVC 2560×1440@120 / 80 | cbr_vbv1 / 1 | 953 | 82445 / 85425 / 92433 / 96763 | 273504 / 3.32× | 179230 / 2.17× | 3.407 / 4.052 |
| HEVC 2560×1440@120 / 80 | cap1 / 1 | 953 | 79325 / 79384 / 89523 / 90888 | 149801 / 1.89× | 86920 / 1.10× | 3.318 / 3.757 |
| HEVC 2560×1440@120 / 80 | cap2 / 1 | 953 | 79396 / 79613 / 89539 / 91863 | 164372 / 2.07× | 154532 / 1.95× | 3.320 / 3.764 |
| HEVC 2560×1440@120 / 80 | intra / 1 | 961 | 79826 / 80282 / 89434 / 89686 | 164421 / 2.06× | — | 3.283 / 3.734 |
| HEVC 2560×1440@120 / 80 | intra / 2 | 961 | 79826 / 80282 / 89434 / 89686 | 164421 / 2.06× | — | 3.303 / 3.757 |
| HEVC 2560×1440@120 / 80 | cap2 / 2 | 953 | 79396 / 79613 / 89539 / 91863 | 164372 / 2.07× | 154532 / 1.95× | 3.350 / 3.783 |
| HEVC 2560×1440@120 / 80 | cap1 / 2 | 953 | 79325 / 79384 / 89523 / 90888 | 149801 / 1.89× | 86920 / 1.10× | 3.376 / 3.793 |
| HEVC 2560×1440@120 / 80 | cbr_vbv1 / 2 | 953 | 82445 / 85425 / 92433 / 96763 | 273504 / 3.32× | 179230 / 2.17× | 3.347 / 3.800 |
| HEVC 2560×1440@120 / 80 | cbr / 2 | 953 | 82951 / 80834 / 97724 / 114942 | 273504 / 3.30× | 139427 / 1.68× | 3.309 / 3.754 |
| HEVC 2560×1440@120 / 80 | default / 2 | 953 | 79432 / 79648 / 89451 / 91833 | 164372 / 2.07× | 172584 / 2.17× | 3.277 / 3.738 |
| AV1 2560×1440@120 / 80 | default / 1 | 953 | 79251 / 79181 / 90271 / 96057 | 115988 / 1.46× | 129240 / 1.63× | 2.617 / 3.144 |
| AV1 2560×1440@120 / 80 | cbr / 1 | 953 | 82938 / 81195 / 95358 / 121335 | 137697 / 1.66× | 112415 / 1.36× | 2.605 / 3.150 |
| AV1 2560×1440@120 / 80 | cbr_vbv1 / 1 | 953 | 82452 / 78778 / 93965 / 113246 | 137697 / 1.67× | 119465 / 1.45× | 2.745 / 3.226 |
| AV1 2560×1440@120 / 80 | cap1 / 1 | 953 | 79122 / 79041 / 88929 / 94888 | 105266 / 1.33× | 79608 / 1.01× | 2.734 / 3.259 |
| AV1 2560×1440@120 / 80 | cap2 / 1 | 953 | 79251 / 79181 / 90271 / 96057 | 115988 / 1.46× | 129240 / 1.63× | 2.759 / 3.275 |
| AV1 2560×1440@120 / 80 | intra / 1 | 961 | 79638 / 80068 / 90338 / 93959 | 115988 / 1.46× | — | 2.712 / 3.209 |
| AV1 2560×1440@120 / 80 | intra / 2 | 961 | 79638 / 80068 / 90338 / 93959 | 115988 / 1.46× | — | 2.655 / 3.188 |
| AV1 2560×1440@120 / 80 | cap2 / 2 | 953 | 79251 / 79181 / 90271 / 96057 | 115988 / 1.46× | 129240 / 1.63× | 2.643 / 3.188 |
| AV1 2560×1440@120 / 80 | cap1 / 2 | 953 | 79122 / 79041 / 88929 / 94888 | 105266 / 1.33× | 79608 / 1.01× | 2.630 / 3.181 |
| AV1 2560×1440@120 / 80 | cbr_vbv1 / 2 | 953 | 82452 / 78778 / 93965 / 113246 | 137697 / 1.67× | 119465 / 1.45× | 2.673 / 3.184 |
| AV1 2560×1440@120 / 80 | cbr / 2 | 953 | 82938 / 81195 / 95358 / 121335 | 137697 / 1.66× | 112415 / 1.36× | 2.578 / 3.134 |
| AV1 2560×1440@120 / 80 | default / 2 | 953 | 79251 / 79181 / 90271 / 96057 | 115988 / 1.46× | 129240 / 1.63× | 2.621 / 3.171 |
| HEVC 3840×2160@60 / 20 | default / 1 | 473 | 41318 / 39558 / 49226 / 50142 | 133527 / 3.23× | 113784 / 2.75× | 5.959 / 6.354 |
| HEVC 3840×2160@60 / 20 | cbr / 1 | 473 | 40300 / 37894 / 50655 / 53448 | 563110 / 13.97× | 102447 / 2.54× | 5.940 / 6.458 |
| HEVC 3840×2160@60 / 20 | cbr_vbv1 / 1 | 473 | 41254 / 37898 / 51685 / 53500 | 321254 / 7.79× | 102447 / 2.48× | 5.992 / 6.512 |
| HEVC 3840×2160@60 / 20 | cap1 / 1 | 473 | 41572 / 39799 / 50024 / 50212 | 90517 / 2.18× | 44496 / 1.07× | 5.971 / 6.369 |
| HEVC 3840×2160@60 / 20 | cap2 / 1 | 473 | 41256 / 39771 / 49265 / 50142 | 133527 / 3.24× | 84020 / 2.04× | 5.989 / 6.353 |
| HEVC 3840×2160@60 / 20 | intra / 1 | 481 | 41830 / 40919 / 49278 / 49351 | 133416 / 3.19× | — | 5.824 / 6.282 |
| HEVC 3840×2160@60 / 20 | intra / 2 | 481 | 41830 / 40919 / 49278 / 49351 | 133416 / 3.19× | — | 5.878 / 6.425 |
| HEVC 3840×2160@60 / 20 | cap2 / 2 | 473 | 41256 / 39771 / 49265 / 50142 | 133527 / 3.24× | 84020 / 2.04× | 6.034 / 6.443 |
| HEVC 3840×2160@60 / 20 | cap1 / 2 | 473 | 41572 / 39799 / 50024 / 50212 | 90517 / 2.18× | 44496 / 1.07× | 5.996 / 6.637 |
| HEVC 3840×2160@60 / 20 | cbr_vbv1 / 2 | 473 | 41254 / 37898 / 51685 / 53500 | 321254 / 7.79× | 102447 / 2.48× | 5.985 / 6.531 |
| HEVC 3840×2160@60 / 20 | cbr / 2 | 473 | 40300 / 37894 / 50655 / 53448 | 563110 / 13.97× | 102447 / 2.54× | 5.977 / 6.528 |
| HEVC 3840×2160@60 / 20 | default / 2 | 473 | 41318 / 39558 / 49226 / 50142 | 133527 / 3.23× | 113784 / 2.75× | 5.985 / 6.676 |
| AV1 3840×2160@60 / 20 | default / 1 | 473 | 41001 / 41103 / 48304 / 48958 | 106459 / 2.60× | 90172 / 2.20× | 4.834 / 5.327 |
| AV1 3840×2160@60 / 20 | cbr / 1 | 473 | 40984 / 38304 / 63241 / 74613 | 285815 / 6.97× | 91166 / 2.22× | 4.939 / 5.500 |
| AV1 3840×2160@60 / 20 | cbr_vbv1 / 1 | 473 | 41184 / 41061 / 53037 / 53149 | 181936 / 4.42× | 102945 / 2.50× | 4.918 / 5.380 |
| AV1 3840×2160@60 / 20 | cap1 / 1 | 473 | 41263 / 41379 / 48755 / 50545 | 68709 / 1.67× | 45928 / 1.11× | 4.923 / 5.389 |
| AV1 3840×2160@60 / 20 | cap2 / 1 | 473 | 41063 / 41246 / 48492 / 49312 | 106459 / 2.59× | 82943 / 2.02× | 4.938 / 5.547 |
| AV1 3840×2160@60 / 20 | intra / 1 | 481 | 41528 / 41283 / 49533 / 50787 | 106459 / 2.56× | — | 5.041 / 5.778 |
| AV1 3840×2160@60 / 20 | intra / 2 | 481 | 41528 / 41283 / 49533 / 50787 | 106459 / 2.56× | — | 4.994 / 5.718 |
| AV1 3840×2160@60 / 20 | cap2 / 2 | 473 | 41063 / 41246 / 48492 / 49312 | 106459 / 2.59× | 82943 / 2.02× | 5.050 / 5.779 |
| AV1 3840×2160@60 / 20 | cap1 / 2 | 473 | 41263 / 41379 / 48755 / 50545 | 68709 / 1.67× | 45928 / 1.11× | 5.037 / 5.524 |
| AV1 3840×2160@60 / 20 | cbr_vbv1 / 2 | 473 | 41184 / 41061 / 53037 / 53149 | 181936 / 4.42× | 102945 / 2.50× | 5.033 / 5.671 |
| AV1 3840×2160@60 / 20 | cbr / 2 | 473 | 40984 / 38304 / 63241 / 74613 | 285815 / 6.97× | 91166 / 2.22× | 5.051 / 5.725 |
| AV1 3840×2160@60 / 20 | default / 2 | 473 | 41001 / 41103 / 48304 / 48958 | 106459 / 2.60× | 90172 / 2.20× | 5.042 / 5.773 |
| HEVC 3840×2160@60 / 40 | default / 1 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.216 / 6.771 |
| HEVC 3840×2160@60 / 40 | cbr / 1 | 473 | 80935 / 78090 / 143341 / 158445 | 615045 / 7.60× | 245278 / 3.03× | 6.210 / 6.789 |
| HEVC 3840×2160@60 / 40 | cbr_vbv1 / 1 | 473 | 79922 / 78940 / 100353 / 112325 | 512986 / 6.42× | 253462 / 3.17× | 6.050 / 6.721 |
| HEVC 3840×2160@60 / 40 | cap1 / 1 | 473 | 79397 / 78767 / 93307 / 93482 | 178313 / 2.25× | 89626 / 1.13× | 6.103 / 6.752 |
| HEVC 3840×2160@60 / 40 | cap2 / 1 | 473 | 79425 / 78987 / 93313 / 93857 | 245474 / 3.09× | 167074 / 2.10× | 6.083 / 6.716 |
| HEVC 3840×2160@60 / 40 | intra / 1 | 481 | 82048 / 80867 / 93426 / 93797 | 245658 / 2.99× | — | 6.071 / 6.548 |
| HEVC 3840×2160@60 / 40 | intra / 2 | 481 | 82048 / 80867 / 93426 / 93797 | 245658 / 2.99× | — | 6.070 / 6.488 |
| HEVC 3840×2160@60 / 40 | cap2 / 2 | 473 | 79425 / 78987 / 93313 / 93857 | 245474 / 3.09× | 167074 / 2.10× | 6.034 / 6.395 |
| HEVC 3840×2160@60 / 40 | cap1 / 2 | 473 | 79397 / 78767 / 93307 / 93482 | 178313 / 2.25× | 89626 / 1.13× | 6.005 / 6.386 |
| HEVC 3840×2160@60 / 40 | cbr_vbv1 / 2 | 473 | 79922 / 78940 / 100353 / 112325 | 512986 / 6.42× | 253462 / 3.17× | 5.991 / 6.359 |
| HEVC 3840×2160@60 / 40 | cbr / 2 | 473 | 80935 / 78090 / 143341 / 158445 | 615045 / 7.60× | 245278 / 3.03× | 5.954 / 6.396 |
| HEVC 3840×2160@60 / 40 | default / 2 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.007 / 6.376 |
| AV1 3840×2160@60 / 40 | default / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 5.079 / 5.774 |
| AV1 3840×2160@60 / 40 | cbr / 1 | 473 | 82148 / 81758 / 128763 / 141099 | 307290 / 3.74× | 162112 / 1.97× | 4.945 / 5.659 |
| AV1 3840×2160@60 / 40 | cbr_vbv1 / 1 | 473 | 82217 / 79758 / 124565 / 127992 | 266944 / 3.25× | 164820 / 2.00× | 4.923 / 5.737 |
| AV1 3840×2160@60 / 40 | cap1 / 1 | 473 | 78366 / 78989 / 93744 / 94929 | 132770 / 1.69× | 82356 / 1.05× | 4.982 / 5.708 |
| AV1 3840×2160@60 / 40 | cap2 / 1 | 473 | 79150 / 79293 / 105441 / 111427 | 185400 / 2.34× | 152792 / 1.93× | 4.865 / 5.665 |
| AV1 3840×2160@60 / 40 | intra / 1 | 481 | 80191 / 79271 / 93847 / 94157 | 185400 / 2.31× | — | 4.997 / 5.805 |
| AV1 3840×2160@60 / 40 | intra / 2 | 481 | 80191 / 79271 / 93847 / 94157 | 185400 / 2.31× | — | 4.929 / 5.724 |
| AV1 3840×2160@60 / 40 | cap2 / 2 | 473 | 79150 / 79293 / 105441 / 111427 | 185400 / 2.34× | 152792 / 1.93× | 5.007 / 5.660 |
| AV1 3840×2160@60 / 40 | cap1 / 2 | 473 | 78366 / 78989 / 93744 / 94929 | 132770 / 1.69× | 82356 / 1.05× | 4.914 / 5.314 |
| AV1 3840×2160@60 / 40 | cbr_vbv1 / 2 | 473 | 82217 / 79758 / 124565 / 127992 | 266944 / 3.25× | 164820 / 2.00× | 5.001 / 5.591 |
| AV1 3840×2160@60 / 40 | cbr / 2 | 473 | 82148 / 81758 / 128763 / 141099 | 307290 / 3.74× | 162112 / 1.97× | 5.024 / 5.798 |
| AV1 3840×2160@60 / 40 | default / 2 **† stream/probe overlap** | 448 | 79360 / 79284 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 6.423 / 12.266 |
| HEVC 3840×2160@60 / 80 | default / 1 **† stream/probe overlap** | 473 | 156383 / 155179 / 180262 / 182161 | 358474 / 2.29× | 324108 / 2.07× | 6.136 / 6.645 |
| HEVC 3840×2160@60 / 80 | cbr / 1 **† stream/probe overlap** | 473 | 164289 / 162660 / 215923 / 216938 | 615045 / 3.74× | 315279 / 1.92× | 6.114 / 6.806 |
| HEVC 3840×2160@60 / 80 | cbr_vbv1 / 1 **† stream/probe overlap** | 473 | 163989 / 160788 / 195259 / 199848 | 615045 / 3.75× | 340352 / 2.08× | 6.157 / 6.766 |
| HEVC 3840×2160@60 / 80 | cap1 / 1 **† stream/probe overlap** | 441 | 154612 / 152135 / 181661 / 182206 | 308507 / 2.00× | 170031 / 1.10× | 8.273 / 64.236 |
| HEVC 3840×2160@60 / 80 | cap2 / 1 | 473 | 156500 / 154927 / 180484 / 182115 | 358474 / 2.29× | 315456 / 2.02× | 6.156 / 6.750 |
| HEVC 3840×2160@60 / 80 | intra / 1 | 481 | 158262 / 161579 / 181698 / 185403 | 358503 / 2.27× | — | 6.136 / 6.757 |
| HEVC 3840×2160@60 / 80 | intra / 2 | 481 | 158262 / 161579 / 181698 / 185403 | 358503 / 2.27× | — | 6.127 / 6.731 |
| HEVC 3840×2160@60 / 80 | cap2 / 2 | 473 | 156500 / 154927 / 180484 / 182115 | 358474 / 2.29× | 315456 / 2.02× | 6.008 / 6.704 |
| HEVC 3840×2160@60 / 80 | cap1 / 2 | 473 | 154374 / 151574 / 181661 / 182206 | 308507 / 2.00× | 170031 / 1.10× | 6.108 / 6.749 |
| HEVC 3840×2160@60 / 80 | cbr_vbv1 / 2 | 473 | 163989 / 160788 / 195259 / 199848 | 615045 / 3.75× | 340352 / 2.08× | 6.240 / 6.836 |
| HEVC 3840×2160@60 / 80 | cbr / 2 | 473 | 164289 / 162660 / 215923 / 216938 | 615045 / 3.74× | 315279 / 1.92× | 6.242 / 6.853 |
| HEVC 3840×2160@60 / 80 | default / 2 | 473 | 156383 / 155179 / 180262 / 182161 | 358474 / 2.29× | 324108 / 2.07× | 6.126 / 6.779 |
| AV1 3840×2160@60 / 80 | default / 1 | 473 | 154544 / 155726 / 173910 / 187397 | 253332 / 1.64× | 255986 / 1.66× | 4.804 / 5.318 |
| AV1 3840×2160@60 / 80 | cbr / 1 | 473 | 165737 / 162718 / 228771 / 251926 | 307290 / 1.85× | 250269 / 1.51× | 4.996 / 5.728 |
| AV1 3840×2160@60 / 80 | cbr_vbv1 / 1 | 473 | 164476 / 161764 / 208183 / 226477 | 307290 / 1.87× | 250464 / 1.52× | 4.935 / 5.770 |
| AV1 3840×2160@60 / 80 | cap1 / 1 | 473 | 153742 / 154901 / 174143 / 176661 | 215311 / 1.40× | 157031 / 1.02× | 4.900 / 5.750 |
| AV1 3840×2160@60 / 80 | cap2 / 1 | 473 | 154544 / 155726 / 173910 / 187397 | 253332 / 1.64× | 255986 / 1.66× | 4.924 / 5.730 |
| AV1 3840×2160@60 / 80 | intra / 1 | 481 | 155089 / 156143 / 172484 / 175581 | 253332 / 1.63× | — | 4.975 / 5.759 |
| AV1 3840×2160@60 / 80 | intra / 2 | 481 | 155089 / 156143 / 172484 / 175581 | 253332 / 1.63× | — | 4.984 / 5.482 |
| AV1 3840×2160@60 / 80 | cap2 / 2 | 473 | 154544 / 155726 / 173910 / 187397 | 253332 / 1.64× | 255986 / 1.66× | 5.006 / 5.894 |
| AV1 3840×2160@60 / 80 | cap1 / 2 | 473 | 153742 / 154901 / 174143 / 176661 | 215311 / 1.40× | 157031 / 1.02× | 4.952 / 5.736 |
| AV1 3840×2160@60 / 80 | cbr_vbv1 / 2 | 473 | 164476 / 161764 / 208183 / 226477 | 307290 / 1.87× | 250464 / 1.52× | 4.949 / 5.451 |
| AV1 3840×2160@60 / 80 | cbr / 2 | 473 | 165737 / 162718 / 228771 / 251926 | 307290 / 1.85× | 250269 / 1.51× | 4.961 / 5.539 |
| AV1 3840×2160@60 / 80 | default / 2 | 473 | 154544 / 155726 / 173910 / 187397 | 253332 / 1.64× | 255986 / 1.66× | 4.865 / 5.358 |

### Frequent recovery requests at 4K60 and 63 Mbps

The same release method, with `--bitrate 63000 --idr-interval 6`, requests
ten recovery keyframes per second, approximating frequent loss reports
without injecting network loss. `cbr_cap1` and `cbr_cap2` add the respective
cap to `amd_rc=cbr` and `amd_vbv_buffer_frames=1`.

| Mode / Mbps | Setting / run | Non-keyframes | Steady mean / p50 / p99 / max (bytes) | Startup bytes / steady mean | Recovery max bytes / steady mean | Encode mean / p99 (ms) |
|---|---|---:|---|---:|---|---|
| HEVC 3840×2160@60 / 63 | default / 1 | 401 | 109013 / 106784 / 139821 / 142313 | 334673 / 3.07× | 294072 / 2.70× | 5.924 / 6.351 |
| HEVC 3840×2160@60 / 63 | cbr_vbv1 / 1 | 401 | 114254 / 108384 / 165169 / 165169 | 615045 / 5.38× | 301985 / 2.64× | 6.083 / 6.736 |
| HEVC 3840×2160@60 / 63 | cap1 / 1 | 401 | 101273 / 100411 / 119624 / 140142 | 254477 / 2.51× | 136605 / 1.35× | 5.971 / 6.585 |
| HEVC 3840×2160@60 / 63 | cap2 / 1 | 401 | 108302 / 104644 / 136414 / 140626 | 334673 / 3.09× | 254958 / 2.35× | 5.997 / 6.669 |
| HEVC 3840×2160@60 / 63 | cbr_cap1 / 1 | 401 | 107310 / 101679 / 145249 / 162267 | 448837 / 4.18× | 186982 / 1.74× | 5.971 / 6.419 |
| HEVC 3840×2160@60 / 63 | cbr_cap2 / 1 | 401 | 113592 / 106497 / 159120 / 165644 | 615045 / 5.41× | 271885 / 2.39× | 6.106 / 6.834 |
| HEVC 3840×2160@60 / 63 | cbr_cap2 / 2 | 401 | 113592 / 106497 / 159120 / 165644 | 615045 / 5.41× | 271885 / 2.39× | 6.048 / 6.608 |
| HEVC 3840×2160@60 / 63 | cbr_cap1 / 2 | 401 | 107310 / 101679 / 145249 / 162267 | 448837 / 4.18× | 186982 / 1.74× | 5.890 / 6.519 |
| HEVC 3840×2160@60 / 63 | cap2 / 2 | 401 | 108302 / 104644 / 136414 / 140626 | 334673 / 3.09× | 254958 / 2.35× | 5.956 / 6.738 |
| HEVC 3840×2160@60 / 63 | cap1 / 2 | 401 | 101273 / 100411 / 119624 / 140142 | 254477 / 2.51× | 136605 / 1.35× | 6.078 / 6.822 |
| HEVC 3840×2160@60 / 63 | cbr_vbv1 / 2 | 401 | 114254 / 108384 / 165169 / 165169 | 615045 / 5.38× | 301985 / 2.64× | 6.014 / 6.678 |
| HEVC 3840×2160@60 / 63 | default / 2 | 401 | 109013 / 106784 / 139821 / 142313 | 334673 / 3.07× | 294072 / 2.70× | 5.910 / 6.486 |
| AV1 3840×2160@60 / 63 | default / 1 | 401 | 116619 / 115511 / 148503 / 148606 | 243277 / 2.09× | 213511 / 1.83× | 4.763 / 5.303 |
| AV1 3840×2160@60 / 63 | cbr_vbv1 / 1 | 401 | 123402 / 123240 / 162894 / 162894 | 307290 / 2.49× | 203733 / 1.65× | 4.810 / 5.257 |
| AV1 3840×2160@60 / 63 | cap1 / 1 | 401 | 110072 / 109551 / 135051 / 140585 | 180535 / 1.64× | 131815 / 1.20× | 4.790 / 5.234 |
| AV1 3840×2160@60 / 63 | cap2 / 1 | 401 | 116619 / 115511 / 148503 / 148606 | 243277 / 2.09× | 213511 / 1.83× | 4.864 / 5.291 |
| AV1 3840×2160@60 / 63 | cbr_cap1 / 1 | 401 | 118700 / 117256 / 155871 / 155871 | 244193 / 2.06× | 161413 / 1.36× | 5.190 / 5.813 |
| AV1 3840×2160@60 / 63 | cbr_cap2 / 1 | 401 | 123402 / 123240 / 162894 / 162894 | 307290 / 2.49× | 203733 / 1.65× | 4.947 / 6.385 |
| AV1 3840×2160@60 / 63 | cbr_cap2 / 2 | 401 | 123402 / 123240 / 162894 / 162894 | 307290 / 2.49× | 203733 / 1.65× | 4.935 / 5.336 |
| AV1 3840×2160@60 / 63 | cbr_cap1 / 2 | 401 | 118700 / 117256 / 155871 / 155871 | 244193 / 2.06× | 161413 / 1.36× | 4.917 / 5.376 |
| AV1 3840×2160@60 / 63 | cap2 / 2 | 401 | 116619 / 115511 / 148503 / 148606 | 243277 / 2.09× | 213511 / 1.83× | 4.944 / 5.702 |
| AV1 3840×2160@60 / 63 | cap1 / 2 | 401 | 110072 / 109551 / 135051 / 140585 | 180535 / 1.64× | 131815 / 1.20× | 4.904 / 5.562 |
| AV1 3840×2160@60 / 63 | cbr_vbv1 / 2 | 401 | 123402 / 123240 / 162894 / 162894 | 307290 / 2.49× | 203733 / 1.65× | 4.989 / 5.449 |
| AV1 3840×2160@60 / 63 | default / 2 | 401 | 116619 / 115511 / 148503 / 148606 | 243277 / 2.09× | 213511 / 1.83× | 4.987 / 5.696 |

### Peak and VBV recheck after two seconds of warmup

The development screening's short warmup could confound VBV comparisons.
These release runs repeat the peak and buffer options at 4K60/40 Mbps with
the same two-second warmup as the main matrix. They also check HRD, the
four-frame cap, and intra refresh with explicit recovery requests.

| Mode / Mbps | Setting / run | Non-keyframes | Steady mean / p50 / p99 / max (bytes) | Startup bytes / steady mean | Recovery max bytes / steady mean | Encode mean / p99 (ms) |
|---|---|---:|---|---:|---|---|
| HEVC 3840×2160@60 / 40 | default / 1 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 5.957 / 6.621 |
| HEVC 3840×2160@60 / 40 | peak1 / 1 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.056 / 6.575 |
| HEVC 3840×2160@60 / 40 | peak15 / 1 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.014 / 6.453 |
| HEVC 3840×2160@60 / 40 | peak2 / 1 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.027 / 6.463 |
| HEVC 3840×2160@60 / 40 | vbv05 / 1 | 473 | 78485 / 78020 / 93492 / 117456 | 213837 / 2.72× | 296535 / 3.78× | 6.011 / 6.385 |
| HEVC 3840×2160@60 / 40 | vbv1 / 1 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.025 / 6.396 |
| HEVC 3840×2160@60 / 40 | vbv2 / 1 | 473 | 80653 / 80187 / 98329 / 105790 | 293486 / 3.64× | 242837 / 3.01× | 6.050 / 6.615 |
| HEVC 3840×2160@60 / 40 | cap4 / 1 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.073 / 6.458 |
| HEVC 3840×2160@60 / 40 | hrd / 1 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.124 / 6.632 |
| HEVC 3840×2160@60 / 40 | cap2hrd / 1 | 473 | 79425 / 78987 / 93313 / 93857 | 245474 / 3.09× | 167074 / 2.10× | 6.121 / 6.679 |
| HEVC 3840×2160@60 / 40 | intra_idr / 1 | 473 | 79829 / 79143 / 92769 / 96778 | 245658 / 3.08× | 266805 / 3.34× | 6.104 / 6.496 |
| HEVC 3840×2160@60 / 40 | intra_idr / 2 | 473 | 79829 / 79143 / 92769 / 96778 | 245658 / 3.08× | 266805 / 3.34× | 6.042 / 6.773 |
| HEVC 3840×2160@60 / 40 | cap2hrd / 2 | 473 | 79425 / 78987 / 93313 / 93857 | 245474 / 3.09× | 167074 / 2.10× | 6.092 / 6.544 |
| HEVC 3840×2160@60 / 40 | hrd / 2 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.032 / 6.679 |
| HEVC 3840×2160@60 / 40 | cap4 / 2 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.059 / 6.771 |
| HEVC 3840×2160@60 / 40 | vbv2 / 2 | 473 | 80653 / 80187 / 98329 / 105790 | 293486 / 3.64× | 242837 / 3.01× | 6.158 / 6.732 |
| HEVC 3840×2160@60 / 40 | vbv1 / 2 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.130 / 6.660 |
| HEVC 3840×2160@60 / 40 | vbv05 / 2 | 473 | 78485 / 78020 / 93492 / 117456 | 213837 / 2.72× | 296535 / 3.78× | 6.129 / 6.718 |
| HEVC 3840×2160@60 / 40 | peak2 / 2 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.052 / 6.624 |
| HEVC 3840×2160@60 / 40 | peak15 / 2 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.054 / 6.537 |
| HEVC 3840×2160@60 / 40 | peak1 / 2 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.202 / 6.730 |
| HEVC 3840×2160@60 / 40 | default / 2 | 473 | 79820 / 79234 / 93258 / 96964 | 245474 / 3.08× | 265995 / 3.33× | 6.202 / 6.748 |
| AV1 3840×2160@60 / 40 | default / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.871 / 5.325 |
| AV1 3840×2160@60 / 40 | peak1 / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 5.021 / 5.767 |
| AV1 3840×2160@60 / 40 | peak15 / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.960 / 5.752 |
| AV1 3840×2160@60 / 40 | peak2 / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.773 / 5.304 |
| AV1 3840×2160@60 / 40 | vbv05 / 1 | 473 | 78508 / 79142 / 93829 / 102300 | 158745 / 2.02× | 163561 / 2.08× | 4.937 / 5.512 |
| AV1 3840×2160@60 / 40 | vbv1 / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.959 / 5.559 |
| AV1 3840×2160@60 / 40 | vbv2 / 1 | 473 | 79425 / 79431 / 105676 / 114494 | 225386 / 2.84× | 153760 / 1.94× | 4.901 / 5.604 |
| AV1 3840×2160@60 / 40 | cap4 / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.899 / 5.736 |
| AV1 3840×2160@60 / 40 | hrd / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.901 / 5.292 |
| AV1 3840×2160@60 / 40 | cap2hrd / 1 | 473 | 79150 / 79293 / 105441 / 111427 | 185400 / 2.34× | 152792 / 1.93× | 4.854 / 5.718 |
| AV1 3840×2160@60 / 40 | intra_idr / 1 | 473 | 78990 / 79133 / 105404 / 112083 | 185400 / 2.35× | 153531 / 1.94× | 5.000 / 5.732 |
| AV1 3840×2160@60 / 40 | intra_idr / 2 | 473 | 78990 / 79133 / 105404 / 112083 | 185400 / 2.35× | 153531 / 1.94× | 4.997 / 5.762 |
| AV1 3840×2160@60 / 40 | cap2hrd / 2 | 473 | 79150 / 79293 / 105441 / 111427 | 185400 / 2.34× | 152792 / 1.93× | 4.922 / 5.750 |
| AV1 3840×2160@60 / 40 | hrd / 2 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.814 / 5.266 |
| AV1 3840×2160@60 / 40 | cap4 / 2 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.876 / 5.363 |
| AV1 3840×2160@60 / 40 | vbv2 / 2 | 473 | 79425 / 79431 / 105676 / 114494 | 225386 / 2.84× | 153760 / 1.94× | 4.808 / 5.337 |
| AV1 3840×2160@60 / 40 | vbv1 / 2 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.967 / 5.374 |
| AV1 3840×2160@60 / 40 | vbv05 / 2 | 473 | 78508 / 79142 / 93829 / 102300 | 158745 / 2.02× | 163561 / 2.08× | 4.786 / 5.316 |
| AV1 3840×2160@60 / 40 | peak2 / 2 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.951 / 5.498 |
| AV1 3840×2160@60 / 40 | peak15 / 2 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.942 / 5.661 |
| AV1 3840×2160@60 / 40 | peak1 / 2 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.954 / 5.670 |
| AV1 3840×2160@60 / 40 | default / 2 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.929 / 5.692 |

### Repeats of modes affected by the live stream

Both affected mode groups were repeated in full, again in forward/reverse
order. All 24 runs below had no active stream or other observed performance
probe, completed 481 of 481 submissions, and reached the requested cadence.
Use these clean comparisons for AV1 4K60/40 Mbps and HEVC 4K60/80 Mbps;
the marked original rows remain above for transparency.

| Mode / Mbps | Setting / run | Non-keyframes | Steady mean / p50 / p99 / max (bytes) | Startup bytes / steady mean | Recovery max bytes / steady mean | Encode mean / p99 (ms) |
|---|---|---:|---|---:|---|---|
| AV1 3840×2160@60 / 40 | default / 1 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.904 / 5.454 |
| AV1 3840×2160@60 / 40 | cbr / 1 | 473 | 82148 / 81758 / 128763 / 141099 | 307290 / 3.74× | 162112 / 1.97× | 5.026 / 5.794 |
| AV1 3840×2160@60 / 40 | cbr_vbv1 / 1 | 473 | 82217 / 79758 / 124565 / 127992 | 266944 / 3.25× | 164820 / 2.00× | 5.027 / 5.771 |
| AV1 3840×2160@60 / 40 | cap1 / 1 | 473 | 78366 / 78989 / 93744 / 94929 | 132770 / 1.69× | 82356 / 1.05× | 4.950 / 5.321 |
| AV1 3840×2160@60 / 40 | cap2 / 1 | 473 | 79150 / 79293 / 105441 / 111427 | 185400 / 2.34× | 152792 / 1.93× | 4.969 / 5.695 |
| AV1 3840×2160@60 / 40 | intra / 1 | 481 | 80191 / 79271 / 93847 / 94157 | 185400 / 2.31× | — | 4.904 / 5.353 |
| AV1 3840×2160@60 / 40 | intra / 2 | 481 | 80191 / 79271 / 93847 / 94157 | 185400 / 2.31× | — | 4.969 / 5.414 |
| AV1 3840×2160@60 / 40 | cap2 / 2 | 473 | 79150 / 79293 / 105441 / 111427 | 185400 / 2.34× | 152792 / 1.93× | 4.943 / 5.427 |
| AV1 3840×2160@60 / 40 | cap1 / 2 | 473 | 78366 / 78989 / 93744 / 94929 | 132770 / 1.69× | 82356 / 1.05× | 4.938 / 5.423 |
| AV1 3840×2160@60 / 40 | cbr_vbv1 / 2 | 473 | 82217 / 79758 / 124565 / 127992 | 266944 / 3.25× | 164820 / 2.00× | 4.897 / 5.289 |
| AV1 3840×2160@60 / 40 | cbr / 2 | 473 | 82148 / 81758 / 128763 / 141099 | 307290 / 3.74× | 162112 / 1.97× | 4.883 / 5.367 |
| AV1 3840×2160@60 / 40 | default / 2 | 473 | 79254 / 79331 / 105540 / 112372 | 185400 / 2.34× | 154097 / 1.94× | 4.893 / 5.370 |
| HEVC 3840×2160@60 / 80 | default / 1 | 473 | 156383 / 155179 / 180262 / 182161 | 358474 / 2.29× | 324108 / 2.07× | 6.141 / 6.724 |
| HEVC 3840×2160@60 / 80 | cbr / 1 | 473 | 164289 / 162660 / 215923 / 216938 | 615045 / 3.74× | 315279 / 1.92× | 6.127 / 6.700 |
| HEVC 3840×2160@60 / 80 | cbr_vbv1 / 1 | 473 | 163989 / 160788 / 195259 / 199848 | 615045 / 3.75× | 340352 / 2.08× | 6.173 / 6.726 |
| HEVC 3840×2160@60 / 80 | cap1 / 1 | 473 | 154374 / 151574 / 181661 / 182206 | 308507 / 2.00× | 170031 / 1.10× | 6.066 / 6.718 |
| HEVC 3840×2160@60 / 80 | cap2 / 1 | 473 | 156500 / 154927 / 180484 / 182115 | 358474 / 2.29× | 315456 / 2.02× | 6.081 / 6.560 |
| HEVC 3840×2160@60 / 80 | intra / 1 | 481 | 158262 / 161579 / 181698 / 185403 | 358503 / 2.27× | — | 6.138 / 6.697 |
| HEVC 3840×2160@60 / 80 | intra / 2 | 481 | 158262 / 161579 / 181698 / 185403 | 358503 / 2.27× | — | 6.096 / 6.789 |
| HEVC 3840×2160@60 / 80 | cap2 / 2 | 473 | 156500 / 154927 / 180484 / 182115 | 358474 / 2.29× | 315456 / 2.02× | 6.146 / 6.776 |
| HEVC 3840×2160@60 / 80 | cap1 / 2 | 473 | 154374 / 151574 / 181661 / 182206 | 308507 / 2.00× | 170031 / 1.10× | 6.132 / 6.657 |
| HEVC 3840×2160@60 / 80 | cbr_vbv1 / 2 | 473 | 163989 / 160788 / 195259 / 199848 | 615045 / 3.75× | 340352 / 2.08× | 6.151 / 6.801 |
| HEVC 3840×2160@60 / 80 | cbr / 2 | 473 | 164289 / 162660 / 215923 / 216938 | 615045 / 3.74× | 315279 / 1.92× | 6.159 / 6.800 |
| HEVC 3840×2160@60 / 80 | default / 2 | 473 | 156383 / 155179 / 180262 / 182161 | 358474 / 2.29× | 324108 / 2.07× | 6.135 / 6.739 |

### Decision and limits

Keep `vbr_latency`, ultra-low-latency usage, speed, VBAQ on and HRD off as
the defaults. Peak bitrate, VBV and the maximum frame size remain driver
defaults (`0` in the three new settings). No input-queue, internal-latency,
lookahead, filler or frame-skipping setting is added to implement a cap.
The measurements do not justify a universal change that both removes
bursts and preserves encode time and picture quality across these modes.

CBR is not a general cure. At 4K60/40 Mbps HEVC its non-keyframe p99 is
143,341 bytes against VBR's 93,258; explicitly restoring a one-frame VBV
reduces that to 100,353. At 1080p60/80 Mbps CBR improves that comparison.
At 4K60/20 Mbps AV1, CBR with a one-frame VBV instead increases recovery
maximum by 14.2%. Imported `amd_rc=cbr` settings are not overwritten.

Explicit 1×, 1.5× and 2× peak requests read back correctly but produce
identical frame sizes in the tested latency-constrained VBR mode. One frame
of explicit VBV reproduces the default. Half a frame increases HEVC's
4K40 recovery maximum from 265,995 to 296,535 bytes; two frames reduce it
to 242,837 but raise non-keyframe p99 from 93,258 to 98,329. HRD adds no
size reduction in these tests, including when paired with a two-frame cap.

The frame cap is useful as an **opt-in trade-off**, not a hard bound.
`amd_max_frame_size=1` reduces 4K40 HEVC recovery maximum from 265,995 to
89,626 bytes (66.3%). Non-keyframe p99 barely changes, 93,258 to 93,307.
The two mean encode times are 6.216/6.007 ms without the cap and
6.103/6.005 ms with it. A two-frame cap gives 167,074 bytes (37.2% smaller)
and 6.083/6.034 ms. On AV1 a two-frame cap often does nothing because its
keyframes already fit that budget; the one-frame cap has a larger effect.

With ten recovery requests per second at 4K60/63 Mbps, HEVC VBR's maximum
falls from 294,072 to 136,605 bytes with a one-frame cap, but measured
encoded payload also falls from 64.12 to 51.01 Mbps. Mean encode time is
5.924/5.910 ms uncapped and 5.971/6.078 ms capped: a small **0.108 ms
increase** in the two-run mean. CBR plus a one-frame VBV and one-frame cap
still reaches 186,982 bytes for a recovery frame and 448,837 at startup.
These results do not establish a cost-free default or unchanged quality.

Even outside the frequent-recovery test, the cap can be exceeded. The
4K40 HEVC one-frame request is 83,333 bytes, but its startup frame is
178,313 bytes. Some ordinary frames exceed it too. All new properties were
accepted on this RX 7900 XT; that is not RDNA4 validation. The previous
RDNA4 freeze reports concern `LowLatencyInternal` and `InputQueueSize`;
this change forces neither. H.264 and HDR cap behaviour, driver rejection
on other cards, dynamic bitrate changes, decoded quality and long-session
stability were not measured here. The AV1 bitrate-update name is corrected,
but the driver's runtime acceptance of updates is not established by these
constant-bitrate probes.

Intra refresh stays client-negotiated. With no forced keyframes it avoids
the periodic spikes, but explicitly requesting recovery still produces a
full keyframe: 266,805 bytes for HEVC with refresh versus 265,995 without
at 4K40. The HEVC policy refreshes seven of 2,040 CTBs per slot at 4K.
Covering the picture once would take 292 slots, about 4.9 seconds at 60
fps; this is a budget calculation, not measured decoder recovery time.
The [AMF HEVC API](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_Encode_HEVC_API.md)
defines that property in 64×64 CTBs per slot. Replacing a decoder-reset
keyframe request with that gradual process is not justified by this test.

For an affected AMD user who can compare picture quality, the new setting
allows testing a one- or two-frame cap with the existing VBR default. It
does not promise that an arbitrary Wi-Fi route can carry the stream.
NVIDIA users should use [Vibepollo](https://github.com/Nonary/Vibepollo).

**Wire-time estimate, not a network measurement:** at 2×40 Mbps pacing and
20% FEC, ignoring packet headers and link overhead, the measured HEVC
4K40 maximum occupies about 31.9 ms of packet transmission uncapped,
10.8 ms with a one-frame cap and 20.0 ms with a two-frame cap
(`bytes × 8 × 1.2 / 80,000,000`). That explains the potential to reduce
burst pressure. It does not establish client picture age or audio recovery.
The paced submission-to-output results check the encoder's contribution
to latency; capture, networking, decoding and presentation remain outside
this probe.

Verification: 368 recorded runs completed (60 development screening runs
and 308 release runs). Every submitted frame returned, with one latency
sample per completed frame. Five original matrix runs overlapped a live
stream; they are marked above and both affected mode groups were repeated.
Two of those overlapping runs missed the requested cadence. Every clean
run reached 481 submissions at 60 fps or 961 at 120 fps. No other
`performance` process was observed at the pre-run checks.

The release performance example built successfully. Final checks passed:
`cargo fmt --all`; Clippy for `butterpollo-core` and `butterpollo-windows`,
all targets with `-D warnings`; 166 core tests; and 61 host binary tests.
The host's two existing ignored tests remain ignored (all-interface
listener/firewall interaction and official installer download). `npm ci`
and `npm run check` passed with 0 errors and 0 warnings across 163 files.
Policy tests cover driver-default preservation, codec property names,
bit units at fractional frame rates, and invalid settings. All Cargo
commands used the supplied environment and `--target-dir target\qa`
where applicable.

## Limits

This machine validates AMD AMF. Native NVENC now calls the installed NVIDIA driver directly, supports reviewed API versions 11.0–13.0, reference frame invalidation, D3D11 4:2:0/8-bit 4:4:4 and GPU-only CUDA interop for ten-bit 4:4:4. Seven mock-driver tests exercise compatibility, asynchronous ownership, timeout teardown, metadata lifetime, loss recovery and bitrate changes; NVIDIA execution/performance still needs NVIDIA hardware. QSV has native D3D11 imports, and TrueHDR has a shared-device GPU path; these need Intel/NVIDIA hardware respectively. PyroWave uses shared D3D11/Vulkan planar GPU inputs and reads back only the encoded bitstream. Unsupported native formats and software encoding use CPU compatibility paths. The wired LAN checks above do not establish Wi-Fi performance, multiple concurrent 4K sessions, dynamic game content, native 4K capture or end-to-end input/display latency. The GPU texture pools and native encoder queues are bounded to eight retained frames; capacity runs may intentionally keep those queues occupied. [PARITY.md](PARITY.md) separates implemented features from native validation.

The initial C++ comparison was blocked by an older `butter.2` executable whose startup performed global virtual-display recovery despite the isolated configuration; it was stopped before streaming tests. The October 4 comparison against pinned Vibepollo 2.0 above supersedes that initial limitation. The controlled FEC comparison uses the exact baseline sources without starting the C++ host.
