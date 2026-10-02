# Windows performance work — October 2, 2026

User objective: make the Rust host smoother and lower latency than Vibepollo,
without reducing features or picture quality. Continue autonomous work for at
least a full day, starting October 2 at 14:30 Europe/Berlin, through October 3
at 14:30. This is a minimum duration, not a performance acceptance result.

## Current state

Installed and pushed revision: `5d5da05ae26f629b2732e5467689e0fc9eca9b8a`.
Draft PR: https://github.com/RamazanKara/Butterpollo/pull/1.
Workspace: `/home/rambo/git/butterpollo-rust`, branch `codex/butterpollo-rust`.
The original `vibepollo` checkout contains unrelated work.

The customer's latest actual session ran from 12:30:32 to 12:31:37 UTC.
Native desktop duplication captured 1968 × 2184 FP16 HDR at 120 fps, AMF AV1.
Five-second log samples show 4.81–6.38 ms mean host processing, 8.51–8.98 ms
p95, and a maximum of 12.82 ms. Encoder p95 is 3.46–3.72 ms. These are recent
two-second windows, not whole-session percentiles. The customer still reports
unacceptable latency and smoothness; acceptance has not been achieved.

Raw session and video configuration are saved in
`C:\Users\ramaz\.codex\artifacts\butterpollo-rust-20260930\day-work-20261002`.
No actual console credentials were copied or reset.

## Next work

1. Reproduce movement at the customer's native HDR resolution and frame rate.
   Static repeated-frame benchmarks cannot establish smoothness improvement.
2. Separate capture age, submission/completion delay, send pacing and frame
   intervals. Keep genuine capture timestamps and existing quality settings.
3. Measure changes to frame scheduling, native GPU conversion and queues before
   accepting them. Investigate the remaining AV1 padded-resolution failure.
4. Establish a same-content, same-settings full-stream Vibepollo comparison.
   Explain any difference in how the two hosts report processing time.
5. Run relevant unit/native checks, package, update the draft PR, and install
   tested improvements while the installed host is idle. Validate actual
   customer sessions; keep remaining release gates visible.

## Operating rules

- Windows only; production host and service code in Rust.
- Do not change codec, dimensions, fps, HDR, bitrate or quality to claim a win.
- Check `http://127.0.0.1:47989/serverinfo` before GPU tests and updates.
  Stop owned test workloads if the customer connects; preserve their stream.
- Preserve profiles, pairing and credentials. No password reset is authorized.
- Routine idle-safe updates are authorized. Do not add project approval gates.
- Preserve timestamps. Report capture-to-delivery and frame intervals alongside
  host processing, so a prettier counter cannot conceal older frames.
- Keep the PR draft and do not claim release readiness without evidence.

## Work log

- 12:30–12:42 UTC: recovered current state, inspected the real customer session,
  capture and encoder scheduling, and previous native acceptance fixtures.
  Arranged a 15-minute follow-up in the current chat. No code optimization yet.
  Follow-up ID: `butterpollo-latency-and-smoothness`.
- 12:42–13:20 UTC: built a native Rust moving HDR renderer and an independent
  decoding client with in-picture QPC timestamps. Removed three-frame buffering
  from the test decoder; this corrects measurement and is not a host improvement.
  Tested 1968 × 2184 HDR at 120 fps. The renderer runs on the virtual output at
  240 Hz, matching its automatic 2× frame-generation mode. Steady loopback
  delivery is 120 fps with no repeated pictures or intervals over 1.5 periods.
  AV1 still decodes to 1984 × 2186 instead of the requested size; strict
  dimension checks fail. An AMF 8×2 alignment request did not fix this hardware.
  A transient HEVC startup failed with a keyed-mutex error; a retry succeeded.
- 13:20–13:29 UTC: context handover and elapsed gap; do not count this as active
  implementation or testing time.
- 13:29 UTC onward: archived current Vibepollo revision
  `8a8c4b03a280ab9f567beb380110abb80f5220b8` with 15 exact pinned dependencies
  for a full-stream comparison. Added same-size shader fast path, GPU timestamp
  measurements, and p99/completion-interval diagnostics. No candidate installed
  or pushed yet.

### Measurements from this pass

Artifacts are in the `day-work-20261002` directory above. These are local test
results, not acceptance of a better customer experience or release readiness.

| Case | Mean / p95 / p99 / max host ms | Mean / p95 / p99 / max picture age ms | Result |
| --- | --- | --- | --- |
| 5d5 HEVC, low-delay decoder | 6.158 / 9.5 / 9.8 / 13.5 | 16.307 / 20.220 / 21.968 / 27.133 | 120.001 fps, strict decode passed; startup probe overlapped |
| 5d5 AV1 | 5.630 / 8.9 / 9.3 / 12.5 | 14.498 / 18.305 / 18.859 / 21.749 | 119.999 fps; wrong decoded dimensions |
| AMF 8×2 request, AV1 | 5.542 / 8.9 / 9.3 / 12.7 | 14.386 / 17.954 / 18.453 / 21.995 | 120.000 fps; wrong decoded dimensions; full startup probe awaited |
| Same-size shader and explicit AMF crop, AV1 | 6.785 / 8.7 / 8.8 / 9.2 | 15.490 / 18.013 / 18.338 / 19.066 | 120.002 fps; wrong decoded dimensions; full startup probe awaited |

Picture age uses the timestamp rendered into the picture and the decode
completion QPC on this machine. It includes local rendering, DWM, capture,
encoding, loopback and software decoding, but excludes remote display scanout.
Vibepollo starts its reported host timer after acquiring a frame; Rust uses its
source timestamp. Those counters alone cannot establish a fair comparison.

Before the same-size shader change, 64 GPU samples measured HDR FP16-to-P010
conversion at native size: 0.335 ms mean, 0.337 ms p95, 0.343 ms p99. Resizing
to 3840 × 2160: 0.520 ms mean, 0.619 ms p95. These isolate conversion and exclude
capture, encoding and delivery; any component gain must be checked in a stream.

After the same-size shader change, native conversion measured 0.170 ms mean,
0.175 ms p95 and 0.179 ms p99. Scaled conversion measured 0.532 ms mean and
0.641 ms p95, versus 0.520/0.619 before; verify this small variance if further
shader changes affect resizing. All six native GPU checks passed, including
absolute HDR luminance/gamut/linear resizing, SDR range/matrix, cursor blending,
4:4:4 chroma, retained texture ownership and conversion timestamps.

The stream result still depends strongly on capture/timer phase: the last AV1
test had a lower maximum but a worse mean. Do not call it a host-latency win.
Its capture-age estimate p95 remained approximately 5.1 ms; encoding p95 was
3.7 ms. The 8×2 alignment experiment was reverted. Explicit surface crop was
ported from the C++ AMF backend but did not fix decoded dimensions on this GPU.

The archived C++ baseline was built successfully with streaming/capture/codec
paths unchanged. Test-only patches guard four machine-wide startup recovery
operations and add an isolated-display creation marker. PyroWave/WebRTC and
driver packaging are disabled in this comparison build; native AMF/DDX remain
the compared paths. Its initial launches failed before streaming because the
portable asset working directory and log parent were missing; fix the fixture,
not the installed host. Preserve both patch files and the pinned inventory.

The GPU process scheduling class was already ported, but per-device GPU thread
priority and maximum render-queue latency were missing. Added the previous
host's relative priority 7 and queue hint 1 as independent best-effort settings.
These compile but await a measured full stream. They are rendering hints, not
evidence that the capture/codec pipeline previously queued three frames.

### Later work, 14:36–15:12 UTC

Continued active coding and testing. No candidate has been installed or pushed.

- Pointer-only DDX snapshots now retain immutable desktop pixels and update
  the cursor separately, avoiding a full desktop GPU copy. A missed new desktop
  invalidates that cache before a later pointer update can reuse it. The native
  test passes for retained pixels, a full eight-texture pool and recovery.
- Added a bounded freshness-wait experiment, **disabled by default**. It waits
  at most half a frame period (capped at 4 ms) and drains encoder output while
  waiting. It is not yet rate-aware or measured, so do not promote the default.
- Added a known quiet tone on the same Steam Streaming Speakers endpoint for
  both hosts. Vibepollo delivered 6,837 audio packets, peak 0.0492 and RMS 0.0347.
  Its earlier zero-packet silent case was not evidence of broken audio capture.
- C++ baseline source refresh still ran at 120 Hz, even after requesting 240 Hz:
  its isolated display helper launch failed with Windows error 5. The requested
  virtual display was created, but its active mode remained 120 Hz. The fixture
  now lets the test-owned renderer set the owned display's mode and verifies
  actual QPC presentation intervals. That matched run has not executed yet.
- C++ at actual 120 Hz source, HDR AV1 1968×2184/120 fps/80 Mbps request, with
  the tone: 120.001 fps, zero late intervals or repeated pictures; host counter
  mean/p95/p99/max 3.514/3.9/3.9/4.3 ms; picture age 17.618/18.730/19.170/19.782
  ms. Source refresh differs from the Rust cases above, so this is **not** a
  valid performance acceptance comparison. Strict AV1 dimensions still fail.
- Automatic approval review rejected the next elevated benchmark launch with
  “blocked by policy.” Do not retry the denied elevated action through another
  mechanism. Native probes and code work continue without elevation. Installed
  service and credentials are unchanged.
- Added a non-elevated native AV1 geometry fixture. Twelve cases (1920×1080,
  1968×2184, 2184×1968; SDR/HDR; alignment modes 3/4) encode and decode eight
  frames each. The driver reads back the requested size and alignment, but
  actual decoded sizes are 1920×1082, 1984×2186 and 2240×1968 respectively.
  Independent traces show the enlarged sequence size and
  `render_and_frame_size_different=0`. No crop/render correction is present.
  The strict test deliberately fails; do not weaken it or claim an AV1 fix.
  See `day-work-20261002/av1-geometry/geometry.json` and header traces. AMD's
  issue 423 documents this family of padded-resolution behavior:
  https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423.
- Found synchronous driver renewal and monitor enumeration under the same
  `Ready` lock read by the capture worker every 100 ms and the encoder every
  250 ms. Changed realtime readers to a short published snapshot of output and
  generation. Maintenance finishes before publication; leases remain owned
  until worker teardown. Retained output/generation are now read together.
  Added a concurrency check for readable old state during a blocked refresh,
  then atomic publication of recovered identity. Full workspace tests and
  Clippy pass; native/customer improvement is unproven.
- A non-elevated read-only maintenance probe sampled the primary physical
  display twenty times, one second apart. Monitor enumeration cost 1.558 ms
  mean, 2.605 ms p95, 4.333 ms maximum. HDR metadata reads cost 0.00196 ms mean
  and 0.012 ms maximum, so an extra HDR polling thread is not justified.
  Enumeration can consume half an 8.33 ms frame slot under the old shared lock;
  removal of that dependency still needs full-stream validation. This probe
  excludes privileged renewal, capture, encoding, network and decoding.

### Regression and package validation, 15:12–15:36 UTC

Continued active work; no candidate has been installed or pushed.

- Full locked release workspace tests pass: 130 ordinary tests. All-target
  Clippy with warnings denied, formatting, release build, separate MSVC NGX
  adapter compilation and packaging pass.
- Twelve available native tests pass together in 58.29 seconds. NVIDIA tests
  and the known failing AV1 geometry gate are explicitly excluded. The geometry
  gate remains a recorded failure, not silently converted into a pass.
- Package verification passes: all 58 manifest entries match staged files and
  ZIP payloads, ZIP integrity passes, and no JavaScript/TypeScript is packaged.
  Package SHA-256: `facedca934ab7bbc2bb409cf1e4ac1881b500c2e3d3d2e233bcd61b6b60542dd`.
  Host SHA-256: `2e6ea482cda5dc4de86a46bcdb032c49273e17d8fb9abfe7d0a8c5e9d8ce9c25`.
- Normal-user physical-contract fixture passes encrypted HEVC Main10,
  permissions, exact 1968×2184, BT.2020/PQ and a known quiet tone through
  WASAPI/Opus. It changes no display settings, uses a disposable profile and
  terminates only owned test processes if the installed customer connects.
  After five seconds warmup: 900 frames, 119.993 fps, zero intervals above
  1.5 periods; host mean/p95/p99/max 5.517/5.9/6.3/6.4 ms; arrival p99/max
  9.198/9.375 ms; audio peak 0.049211/RMS 0.034739, zero decode failures.
  Source is unchanged physical 2560×1440 SDR, scaled/converted to the requested
  HDR format. This is compatibility evidence, not native HDR motion acceptance.
- Packaged remembered-login/legacy-session/API-key migration fixture passes
  all twelve checks across four restarts. It writes only its disposable profile;
  installed credentials remain unchanged.

- The same non-elevated C++ HEVC request delivers audio and 120 fps, but fails
  all strict HDR color checks: its decoded primaries/transfer are SDR (6/6)
  instead of BT.2020/PQ (9/16) from the physical SDR source. Its timings cannot
  be used as a same-quality HDR comparison. No check is weakened.

Next: finish regression checks, measure native capture/metadata maintenance
costs without changing display settings, establish a permitted matched stream
comparison, and keep the freshness experiment disabled until delivery tails
and unique-frame rate improve. Retain exact geometry failures as release gates.

Update this file with concrete changes, measurements, failures and next steps.
Record active work intervals separately from elapsed wall time and scheduled
idle gaps. A schedule running for a day does not prove a day of active effort.
