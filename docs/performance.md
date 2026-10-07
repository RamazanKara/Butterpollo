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
