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
4x). Three runs each; the game-like load ran at 174-177 fps in all of them.
"Host latency" is the per-frame value Moonlight reports from the host.

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
Vibepollo and 59 fps with Butterpollo. Vibepollo applied an RTSS 60 fps
limit during every stream (Butterpollo's virtual-display policy does the
same); it does not reach the probe, which rendered 121 fps idle under both.
Absolute load numbers move between batches (Butterpollo measured 33.5 ms in
an earlier batch without Vibepollo runs), so only compare rows measured
together. Artifacts: `day-work-20261002\hh1080-*`.

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

## Limits

This machine validates AMD AMF. Native NVENC now calls the installed NVIDIA driver directly, supports reviewed API versions 11.0–13.0, reference frame invalidation, D3D11 4:2:0/8-bit 4:4:4 and GPU-only CUDA interop for ten-bit 4:4:4. Seven mock-driver tests exercise compatibility, asynchronous ownership, timeout teardown, metadata lifetime, loss recovery and bitrate changes; NVIDIA execution/performance still needs NVIDIA hardware. QSV has native D3D11 imports, and TrueHDR has a shared-device GPU path; these need Intel/NVIDIA hardware respectively. Unsupported native formats, PyroWave and software encoding use CPU compatibility paths. These measurements do not establish network streaming outside loopback, multiple concurrent 4K sessions, dynamic game content, native 4K capture or end-to-end input/display latency. The GPU texture pools and native encoder queues are bounded to eight retained frames; capacity runs may intentionally keep those queues occupied. [PARITY.md](PARITY.md) separates implemented features from native validation.

A whole-host C++ A/B performance comparison remains outstanding. The available local C++ executable was an older `butter.2` build, and its startup performed global virtual-display recovery despite the isolated configuration. It was stopped before streaming tests. The controlled FEC comparison above uses the exact baseline sources without starting the C++ host. Original C++ streaming measurements in the parent README remain reference data for that implementation.
