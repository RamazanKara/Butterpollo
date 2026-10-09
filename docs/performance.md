# Performance and validation

[Docs](README.md) · [How compute works](architecture.md) · [Troubleshooting](troubleshooting.md) · [Full measurement record](../rust/PERFORMANCE.md)

Butterpollo's performance work targets fresh pictures and lower picture age. This guide collects the main results; each table links to the fixture, exact runs and scope behind it.

## What the numbers mean

| Metric | What it measures |
| --- | --- |
| **Render-to-decode delay / picture age** | Time from a timestamped rendered picture to independent decoding, including desktop composition, capture, encoding and the fixture's transport. These local tests use loopback; remote network transit and display scanout are separate. |
| **95th percentile (p95)** | The slower end of the sample: 95% of observations are at or below this delay. |
| **Fresh pictures per second** | Distinct pictures received, counted from changing picture IDs. Repeated frames do not increase this count. |
| **Encoder time** | The encoder's submission-to-completion interval. It covers one part of the frame journey. |
| **`detect_mean_ms`, `detect_p95_ms`** | Legacy capture timestamp to host-owned copy submission, on newly claimed pictures. On DDX the stamp is the later of desktop presentation and cursor update. On WGC it is `SystemRelativeTime`, with future, missing or more than two-second-old stamps replaced by the time of conversion. This is a clamped estimate, not app-Present-to-capture delay. |
| **`claim_wait_mean_ms`, `claim_wait_p95_ms`** | Host-owned copy submission to the encoder's claim, on newly claimed pictures. The copy may still be running on the GPU. |
| **`wgc_stamp_to_host_mean_ms`, `wgc_stamp_to_host_p95_ms`** | Signed time from WGC's unclamped `SystemRelativeTime` to host-owned copy submission. Negative means the stamp was still in the future. The helper path includes transport through the helper and shared textures; both paths exclude completion of the host's GPU copy. These are stamp offsets, not app-Present latency. |
| **`wgc_stamp_frames`, `wgc_stamp_future_frames`** | Number of valid raw WGC stamps sampled and how many were later than host acquisition. Missing stamps are excluded, not counted as zero delay. With no valid WGC samples, both counts are zero and the two stamp-offset fields are absent. |
| **`frame_age_mean_ms`, `frame_age_p95_ms`, `frame_age_p99_ms`** | Legacy capture timestamp to encoder claim, for sent frames. This inherits the timestamp limitations above; static repeats refresh that timestamp. It is not the age of the original rendered picture. |
| **`present_to_send_mean_ms`, `present_to_send_p95_ms`, `present_to_send_p99_ms`** | `frame_age` plus claim to final packet sent, including packetization and send pacing. The historical name remains for compatibility; its start is the legacy capture stamp, not a measured application Present. |
| **`host_mean_ms`, `host_p95_ms`, `host_p99_ms`, `host_max_ms`** | Claim to the pre-packetization sample, matching Moonlight's host latency. The session API calls these `host_processing_*`. Capture age and final-packet sending are separate. |

The `stream timings` capture split and WGC stamp diagnostics use up to 4,096
newly claimed pictures since the previous log (normally five seconds).
`frame_age`, `present_to_send`, host and encoder statistics use sent frames
from the last two seconds, up to 1,024 frames. Their different populations
mean the averages need not add up exactly.

On the October 7 physical-display stamp probe, 92% of WGC stamps lay in the
future at arrival, with an average signed offset of about -0.79 ms. The old
clamped `detect_mean_ms` of about 0.12 ms did not establish near-zero capture
delay. WGC supplies no reliably matched application Present time here, so
the host reports the raw signed offset separately instead of inventing one.
Use the [picture-ID capture study](../rust/PERFORMANCE.md#october-7-where-wgc-loses-a-millisecond)
or an end-to-end rendered-picture fixture to compare delivery latency.
The raw diagnostic does not change the timestamp used by pacing or encoders.

Game input-to-display latency requires its own measurement. Read each row using its named metric, capture path, source and client fixture.

## Next to Vibepollo 2.0

The historical whole-host comparison used **Vibepollo 2.0** and **Butterpollo rc.2** with the same settings: native AMF at ultra-low latency, DDX and realtime GPU priority on both. RX 7900 XT, 1080p60 HEVC HDR, 20 Mbps requested, controlled GPU load; arithmetic means of three alternating runs per host on October 4, 2026.

| Under controlled GPU load | Vibepollo 2.0 | Butterpollo rc.2 |
| --- | ---: | ---: |
| Average render-to-decode delay | 96.4 ms | **42.4 ms** |
| Mean per-run 95th-percentile delay | 137.0 ms | **56.5 ms** |
| Fresh pictures per second | 23.9 | **51.4** |
| Game frame rate beside the host | 176.7–177.4 fps | 173.8–174.9 fps |

The idle comparison measured 16.0 ms versus 13.8 ms. Most of the loaded gap is not the compute path: with compute off, Butterpollo still delivered about 57 fresh pictures a second beside the same load in an earlier batch, and the [same-build comparison](#isolating-radeon-compute) puts the compute gain at 7.5 ms. Vibepollo handed its native AMF encoder about 24 frames a second, and the encoder logged that its output had not caught up. That encoder came from Butterpollo's author ([Vibepollo #342](https://github.com/Nonary/Vibepollo/pull/342)); where the frames are lost is still being traced, and the fix goes to Vibepollo.

[Baseline revision, workload and recorded runs →](../rust/PERFORMANCE.md#against-vibepollo-20)

## Isolating Radeon compute

This separate test changes one setting in the **same Butterpollo build**: compute off versus compute on. RX 7900 XT, DDX, 1080p60 HEVC HDR, controlled GPU load; arithmetic means of two runs per path on October 4, 2026.

| Render-to-decode delay | Compute off | Compute on |
| --- | ---: | ---: |
| Average | 41.0 ms | **33.5 ms** |
| Mean per-run 95th percentile | 54.4 ms | **42.3 ms** |

This isolates the benefit of moving frame preparation onto compute. The whole-host result above includes the combined implementation. [Queue placement and synchronization](architecture.md#the-radeon-frame-path) explain the change.

[Same-build runs and synthetic component probes →](../rust/PERFORMANCE.md#1080p-at-60-fps)

## rc.17 against rc.2

The same fixture three days later, with rc.2 and rc.17 alternating in one batch: RX 7900 XT, 1080p60 HEVC HDR, 20 Mbps, compute on, 120 Hz virtual display; arithmetic means of three runs per row on October 7, 2026.

| Render-to-decode delay | rc.2, DDX | rc.17, DDX | rc.17, WGC (default) |
| --- | ---: | ---: | ---: |
| Idle, average | 14.7 ms | 14.7 ms | 14.9 ms |
| Beside the load, average | 35.6 ms | 35.4 ms | **33.4 ms** |
| Beside the load, mean per-run 95th percentile | 45.3 ms | 44.8 ms | **43.9 ms** |
| Beside the load, fresh pictures per second | 58.0 | 59.0 | 58.3 |

On the same capture path the two releases deliver the picture at the same time. rc.17's default WGC capture delivers it about 2 ms sooner beside the load, and the host latency Moonlight reports falls from 5.7 to 1.9 ms. rc.2 measured 35.6 ms here against 42.4 ms in the October 4 comparison with Vibepollo: compare only rows from one batch.

[Runs and settings →](../rust/PERFORMANCE.md#october-7-rc17-against-rc2-on-the-october-4-fixture)

## New in rc.19

Two changes in rc.19 target Radeon cards under pressure, measured on the RX 7900 XT against the code before them. rc.19's idle render-to-decode delay on the fixture above is 14.9 ms, as rc.17's.

| | Before rc.19 | rc.19 |
| --- | ---: | ---: |
| **PyroWave beside a GPU-heavy game**, per 1080p120 HDR 4:4:4 frame | 5.7 ms | **0.55 ms** |
| PyroWave beside the game, paced at 120 fps | 4.62 ms | **0.71 ms** |
| **Encoder saturated** (5120×1440 HEVC at 240 fps), game frame to packet | 42.7 ms | **11.1 ms** |
| Encoder saturated, 99th percentile | 45.9 ms | **13.3 ms** |

PyroWave's colour conversion now runs on the Radeon compute queue instead of waiting behind the game on the graphics queue; idle it takes 0.47 ms either way, and the planes are byte-identical. When the encoder cannot keep up, the host now claims a new picture only while fewer than two wait in the encoder, so the frames it sends are fresh; the encoder delivered 220 fps either way.

[PyroWave runs →](../rust/PERFORMANCE.md#october-7-pyrowave-conversion-on-the-compute-queue) · [Encoder queue runs →](../rust/PERFORMANCE.md#october-7-two-frames-in-the-encoder)

**AMF's low-latency switches were checked too.** With the default ultra-low-latency usage, the driver already applies its internal low-latency mode and AV1's lowest latency; forcing them changed neither encode time nor output size, so they stay on Driver default. [When forcing them helps →](configuration.md#capture-and-video)

## Input under load

Every client's input passes through one host thread. Changes after rc.19 give virtual controllers a thread of their own, let the input thread keep its multimedia priority boost, and send the keyboard and mouse input of one network pass to Windows in one call. Ryzen 7 5800X3D, October 7, 2026:

| | Before | After |
| --- | ---: | ---: |
| Mouse move behind a controller update, CPU-bound load on every core, worst case | 287 ms | **2.0 ms** |
| Input packet to the input thread beside time-critical threads on every core, median | 3.5 ms | **17 µs** |

Smaller changes take a few tens of microseconds off every input packet (the network acknowledgement now goes out after the input is applied) and remove 2–18 ms pauses at a stream's first input and while its display is created or renamed. These are host-side measurements on loopback; the network and the client's decoding come on top.

[Method and all runs →](../rust/PERFORMANCE.md#october-7-input-on-the-control-thread)

## WGC capture and pacing

Since rc.9, Automatic capture prefers WGC and supported Radeon streams use compute by default. Guarded source-phase pacing waits for a predicted fresh update when the capture history supports it.

In the controlled local 720p60 AV1 comparison, guarded pacing delivered **59.862–60.000 fresh pictures/s**, versus **57.650**, and reduced estimated source-presentation-to-software-decode age by about **3.8 ms on average**. The final rc.10 default-path SDR check recorded **59.999 fresh pictures/s**.

These WGC results use a different fixture from the DDX comparisons above. The full record includes slower sources, 120 FPS operation, helper recovery and GPU saturation. When a GPU is fully occupied, game rendering and desktop composition can still delay the source picture; the [saturation investigation](../rust/PERFORMANCE.md#saturation-diagnosis-and-queue-drain-rejection) keeps those observations alongside the successful runs.

[Pacing comparisons](../rust/PERFORMANCE.md#guarded-ab-stress-and-compatibility-checks) · [Final rc.10 checks](../rust/PERFORMANCE.md#hdr-state-correction-and-current-sdr-validation)

## HDR and PyroWave validation

| Check | Recorded result | What was inspected |
| --- | --- | --- |
| **Native HEVC and AV1 HDR** | **5,173 / 5,173 frames decoded** across four rc.10 runs | RX 7900 XT, 720p60 FP16 virtual-HDR capture, decoded BT.2020/PQ, reference colours and display restoration. |
| **PyroWave HDR 4:4:4 transport** | **2,357 / 2,357 frames decoded**, zero video/audio decode errors | 1080p/120 encrypted transport, profile/HDR control state and independent vendor decoding from an SDR desktop source. |
| **Moonlight PC 6.2.0** | Ten codec/reconnect sessions | H.264, HEVC, AV1, HEVC HDR and AV1 HDR, two connections each at 720p60 with the released Windows client. |

The native HDR tests include four reference-frame pixel dumps. Their per-channel mean error is below 0.51 ten-bit code values; this is a check of those reference frames. Complete frame decoding and even arrival cadence are separate checks: one AV1 run recorded 31 intervals above 25 ms, while its repeat recorded zero.

The PyroWave transport fixture uses normalized eight-bit CPU readback to validate the profile and transport. Physical TV calibration, client scanout and full ten-bit PyroWave presentation need their own display-side checks. The Moonlight compatibility matrix used an SDR desktop, separately from the later native HDR pixel fixtures.

[Native HDR evidence](../rust/PERFORMANCE.md#final-native-virtual-hdr-pixels-excluding-physical-panel-calibration) · [PyroWave transport](../rust/PERFORMANCE.md#vibepollo-20-pyrowave-transport) · [Client and hardware matrix](../rust/PARITY.md#evidence)

## CPU and correctness work

Video error correction uses **21–29% less CPU time** than the original C++ implementation on the recorded video blocks, with byte-identical parity. [FEC sources and reproduction](../rust/PERFORMANCE.md#controlled-comparison-with-the-original-c-fec) keep this component result separate from stream latency.

The rc.10 ordinary suite passed **263 tests**, with 27 environment-dependent tests excluded from that count. Formatting, Clippy and release builds also passed in that release validation. Hardware fixtures and later documentation edits are recorded separately. [Validation scope](../rust/PARITY.md#evidence) · [Release notes](../rust/RELEASE_NOTES.md)

## Measure your setup

Start with the [troubleshooting guide](troubleshooting.md) to collect the actual capture backend, codec, stream settings, GPU/driver, network type and client statistics. Compare one setting at a time with the same source motion and game load.

For development, the [full record](../rust/PERFORMANCE.md#reproduce-on-another-machine) documents capture/encode probes, picture-ID fixtures and independent decoding. Use a release build and record display restoration for fixtures that change the display layout. The detailed record retains earlier failures and rejected optimizations so each result remains traceable.

To compare static-screen recovery, build the independent receiver, set `BUTTERPOLLO_TEST_IDR_PROBE=10` and run `python rust/tests/interop.py C:\path\to\artifacts hevc 1920 1080 60 25 20000 1` against each [isolated test host](../rust/PERFORMANCE.md#reproduce-on-another-machine). Keep the desktop still: no motion probe, `BUTTERPOLLO_TEST_REQUIRE_MOTION` or `BUTTERPOLLO_TEST_MIN_FPS`. After `BUTTERPOLLO_TEST_WARMUP_SECONDS` (default 2), `LiRequestIdrFrame` requests are at least 1.5 seconds apart, with one outstanding request. Allow an explicit duration of at least warm-up + N × 1.5 seconds. `IDR_PROBE` summarizes request-to-fully-assembled-IDR latency; `<BUTTERPOLLO_TEST_TIMING_CSV>.idr.csv` (default `idr-probe.csv`) records request, arrival and decode-completion times on Moonlight's local monotonic clock. Missing values are −1 and incomplete probes fail. Use identical fixture settings for both hosts; unset the probe variable for normal behavior.
