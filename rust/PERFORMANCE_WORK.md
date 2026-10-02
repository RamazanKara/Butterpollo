# Windows performance work — October 2, 2026

User objective: make the Rust host smoother and lower latency than Vibepollo,
without reducing features or picture quality. At 17:42 UTC on October 2 the
user explicitly requested a finish and handoff to Opus 5.5. This supersedes the
earlier minimum-day continuation. The existing heartbeat
`butterpollo-latency-and-smoothness` is PAUSED; do not resume it automatically.
Performance acceptance remains unfulfilled.

## Current state

Installed revision: `36bff835aa635068d21e6cbe142b306e1ebca61c`.
The final handoff source on `codex/butterpollo-rust` includes the subsequent
changes recorded below. Those changes are not installed or packaged. Use
`git log -1` for the source checkpoint; do not confuse it with installed bytes.
Push only to the owned `butterpollo` remote, not upstream `origin`.
Draft PR: https://github.com/RamazanKara/Butterpollo/pull/1.
Workspace: `/home/rambo/git/butterpollo-rust`, branch `codex/butterpollo-rust`.
The original `vibepollo` checkout contains unrelated work.

The latest customer disconnect observed before the handoff was 16:51:32 UTC.
Native 1968 × 2184/120 HDR sessions on the installed revision show AV1 means
around 6.0–6.6 ms and HEVC means around 7.1–7.4 ms, with tails above 10 ms.
One HEVC send interval was 32.969 ms. These are rolling diagnostic windows,
not whole-session percentiles. The customer reports all three codecs worse
than Vibepollo; acceptance has not been achieved. A display arrangement restore
was still pending at 17:05:50 UTC with Windows error 31.

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
- Check `http://127.0.0.1:47989/serverinfo` AND the installed connection ledger
  before GPU tests and updates. Public FREE/currentgame are scoped to the
  requesting client and cannot prove global idleness. The new loopback counts
  are not available in the installed revision. Stop owned test workloads if
  the customer connects; preserve their stream.
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

### Installed candidate and capture wait investigation, 15:36–15:52 UTC

- Revision `36bff835a` is committed and pushed to the `butterpollo` remote.
  `origin` is the upstream Nonary repository; an initial push there returned
  HTTP 403 and changed nothing. Subsequent work uses the owned Butterpollo fork.
- Draft PR description/title now describe the final changes and current limits.
  Windows CI is running for the exact revision:
  https://github.com/RamazanKara/Butterpollo/actions/runs/37028771427.
- Refreshed package with current documentation verified all 58 files again.
  Host bytes are unchanged from the validated candidate; package SHA-256 is
  `d196bca099e2212a599a6c554f992f02679f2b08253a0e1338490ee5765c63d7`.
- Routine installation succeeded at 15:44:23 UTC while the installed host was
  idle. The updater preserved the profile and a separate rollback backup in
  `day-work-20261002/update-36bff835a/installed-backup`. Installed host SHA-256
  matches the tested candidate; all 58 installed hashes match, service is
  running and listeners/capabilities are ready. This updater launches no
  benchmark and does not retry the rejected elevated benchmark action.
- A new normal-user, read-only DXGI wait probe changes no display settings,
  opens no window and reads back no pixels. Forty timeout samples per case on
  a separate D3D11 device: requested 1/2/4 ms waits have 7.305/11.468/10.800 ms
  means and 15.260/16.741/18.868 ms maxima. Zero-timeout acquisition costs
  0.0112 ms mean, 0.018 ms max. Arrivals are excluded from timeout distributions.
  This rules out replacing the current nonblocking worker with a short blocking
  DXGI wait on this Windows installation. It does not measure shared-device
  contention, copies, encoding or delivery. Artifact: `ddx-waits.json`.

### DDX lifecycle and normal-user motion work, 15:52–16:20 UTC

- Implemented deferred DDX release in the next candidate: release immediately
  before acquisition, and release on errors/Drop. This follows Microsoft's
  recommendation to reduce redundant desktop updates; acquisition remains
  nonblocking. Owned textures remain separate from the acquired DXGI surface.
  Switching through the CPU acquisition path invalidates the GPU pixel cache.
- The new native retention/teardown fixture passes in 0.34 seconds, across
  three duplication instances and CPU/GPU transitions. Windows' 39 ordinary
  checks, Clippy and example checks pass. Encrypted HEVC/audio compatibility
  also passes (119.974 fps, zero late intervals; host p99 3.8 ms/max 4.2 ms).
  The physical desktop's activity differs from the previous compatibility run,
  so do not attribute the lower host mean to this lifecycle change.
- Added a non-activated 128-pixel-high motion strip at the bottom of an existing
  physical output. It changes no display mode and reads source sequence/QPC
  directly from independently decoded pixels. No physical desktop frame dump
  is saved. Audio stays on the quiet test-owned virtual-speaker tone.
- The first strip launch was mistakenly started after a failed C client build
  (its compiler runtime PATH was missing). Stopped only the owned fixture tree
  and rebuilt successfully. The interrupted run is not accepted evidence.
- The next run caught a source assumption mismatch: the current Odyssey
  G93SC output is 5120×1440/120 Hz, while the earlier primary was 2560×1440.
  Diagnostics confirm two physical outputs (Odyssey and HISENSE), not a created
  virtual display. The strip fixture now diagnoses and locks the actual source
  name/dimensions, and verifies the renderer's source identity after the run.
- Corrected Rust AV1 SDR 5120×1440/120/80 Mbps requested strip run passes strict
  dimensions, independent timestamp coverage (100%), audio and permissions.
  After five seconds warmup: 3,308 unique pictures at 119.996 fps, zero repeats,
  source skips or intervals above 1.5 periods. Host mean/p95/p99/max is
  6.048/6.8/7.0/7.8 ms. Picture age is 18.468/19.222/19.678/20.744 ms. Actual
  renderer rate is 119.999 Hz. This covers partial-window native SDR motion;
  full-game/native HDR and remote scanout remain separate acceptance checks.
- Exact installed/pushed `36bff835a` Windows CI passed at
  https://github.com/RamazanKara/Butterpollo/actions/runs/37028771427.

### Matched SDR motion and customer regressions, 16:20–17:03 UTC

- Active work continues from 13:29 UTC through this interval; elapsed scheduled
  time is not counted as active effort. The minimum-day continuation remains
  2026-10-03 12:30 UTC, with acceptance still required afterward.
- Exact Vibepollo `8a8c4b03` passes the same 5120×1440 SDR strip fixture at an
  actual source rate of 119.999 Hz. It delivers 119.846 unique fps, three source
  skips/late intervals; picture age mean/p95/p99/max is
  18.104/21.616/22.253/39.383 ms. The prior Rust strip run has a slightly worse
  mean but lower tails and zero late intervals. One partial-window pair is not
  proof that the product is better for the customer's HDR desktop or games.
- A source-rate-aware freshness experiment (disabled by default) passes the
  Rust strip fixture: 120.003 unique fps, no repeats/skips/late intervals;
  picture age 16.833/17.749/18.168/19.993 ms. Independent decoder mean differs
  (1.717 ms versus 2.342 ms for C++), so repeated alternating comparisons are
  required. Its arrival p99 is 9.871 ms versus 9.297 ms without the experiment;
  do not hide that tradeoff. Core fallback checks cover slower/irregular,
  restarted and overdue sources. Full-stream acceptance remains open.
- Customer reports all three codecs are still worse than Vibepollo, plus an
  Artemis HDR10 warning and occasional `invalid digit found in string`/503.
  Installed revision remains `36bff835a`; experimental scheduling is not
  installed. Recent native 1968×2184/120 HDR logs show AV1 host mean about
  6.0–6.6 ms and HEVC about 7.1–7.4 ms, with capture-age p95 around 3.5–5.2 ms.
  HEVC also has one send interval of 32.969 ms. These reports fail acceptance.
- Found a signed stream-key ID protocol defect: Android sends a signed random
  Java int, while Rust parsed only u32. Added signed/unsigned decimal handling
  that preserves all 32 IV bits and rejects out-of-range/noninteger values.
  Boundary unit check passes; encrypted end-to-end signed-ID check is pending.
- Corrected native/motion fixture guards: unauthenticated GameStream FREE is
  scoped to the requesting client and cannot prove global idleness. Guards now
  also read the installed host's connection ledger, fail closed and allow a
  quiet interval after customer activity. The candidate adds loopback-only
  counts for actual active/pending sessions and application state. Installed
  updater already checks the connection ledger; no user session is stopped.
- Confirmed GPU converter preparation costs 2415.026 ms at 1968×2184 and
  2743.044 ms at 3840×2160 (`shader-before.json`). This is a startup/reconnect
  cost, not steady frame conversion. Moving the unchanged HLSL and compiler
  optimization flags into the native Windows Rust build embeds DXBC and keeps
  the runtime compiler fallback for cross builds. Native correctness and
  after-change preparation/first-frame measurements are pending.
- HDR10 HEVC/AV1 flags are present in the installed server's current response.
  Standard codecs were published incrementally during probing. The candidate
  waits asynchronously for the complete standard codec set before answering
  serverinfo/applist. The first isolated startup sample already had HEVC HDR
  available, so the customer's warning is not yet reproduced or proven fixed.
  Exact Artemis warning clarification is optional and pending.
- Current source passes all-target Clippy. Full release tests/build are running.
  No new candidate is installed yet; no elevated benchmark retry is attempted.

### Final source validation and handoff, 17:03–17:42 UTC

- Signed Android stream-key IDs now pass an encrypted end-to-end HEVC HDR
  request: `-2147483525` preserves IV bits `0x8000007b`. Exact 1968×2184,
  BT.2020/PQ, audio and permissions pass; 1,800 total decoded frames and 2,814
  audio packets have zero decode failures. After warmup: 119.998 fps, zero late
  intervals; host mean/p95/p99/max 7.718/8.1/8.5/9.4 ms. This identifies one
  concrete cause of intermittent 503/invalid-digit failures, not every possible
  failure. Artifact: `signed-key-hevc-contract`.
- Embedded GPU shaders retain the previous HLSL and optimization flags. Native
  converter preparation falls from 2415.026 to 6.385 ms at 1968×2184, and from
  2743.044 to 1.272 ms at 3840×2160. This removes runtime compilation from the
  first frame; it is not a sustained latency result. All thirteen available
  native checks pass, including DDX retained-snapshot teardown. NVIDIA checks
  and the known failing AMD AV1 geometry check remain excluded, not passed.
  Artifacts: `shader-before.json`, `native-all-shaders`.
- All twelve PyroWave chroma/depth/framing profiles encode, and twenty-four
  encrypted/unencrypted transport cases pass the original C++ FEC and vendor
  decoder checks after shader embedding. Artifacts: `pyrowave-shaders`,
  `pyrowave-shader-transport.json`.
- Startup serverinfo waits for the complete standard codec probe. The isolated
  first response includes all standard HDR flags (`0x30301`) at about 0.593 s;
  the full probe completes in about 1.28 s instead of about 38.5 s. The prior
  first response already included HEVC HDR. The customer's Artemis warning
  remains unreproduced; obtain its exact text and test with the real client.
  Artifacts: `hdr-startup-before`, `hdr-startup-after`.
- Freshness waiting, faster fixed polling and bounded predictive polling remain
  experiments. Defaults are `capture_freshness_wait=false`,
  `capture_predictive_poll=false`, `capture_poll_interval_us=1000`. No installed
  profile was changed to enable them. Faster 100 µs fixed polling lowers mean
  picture age but produces 16 late intervals/119.386 unique fps, so it is rejected
  as a production default. Predictive polling limits fast polling to a window
  around a learned source cadence and returns to normal waits for static,
  irregular or restarted sources; its first motion result remains mixed.

Matched physical-output motion results below use a 128-pixel strip without
changing the existing display mode. Picture age includes rendering, DWM,
capture, encoding, loopback and independent software decoding; it excludes
remote display scanout. The C++ host counter excludes pre-acquisition capture
age while Rust includes it, so compare picture age and delivery together.
These runs do not establish native virtual-display or full-game acceptance.

| Case | Unique fps / late intervals | Mean / p95 / p99 / max picture age ms |
| --- | --- | --- |
| C++ AV1 SDR strip, second run | 119.765 / 3 | 16.424 / 21.132 / 22.022 / 46.623 |
| Rust AV1 SDR, embedded shaders + freshness | 119.998 / 0 | 16.173 / 16.819 / 17.286 / 17.906 |
| C++ HEVC native physical HDR, first pair | 119.798 / 4 | 15.677 / 16.057 / 17.058 / 32.182 |
| Rust HEVC physical HDR + freshness, first pair | 120.000 / 0 | 16.663 / 17.116 / 17.344 / 20.370 |
| Rust HEVC HDR + fixed 100 µs polling | 119.386 / 16 | 15.111 / 15.585 / 16.009 / 24.123 |
| C++ HEVC HDR, final pair without session API polling | 118.645 / 31 | 18.633 / 24.091 / 83.201 / 111.426 |
| Rust HEVC HDR + predictive polling, final pair | 119.355 / 14 | 15.841 / 18.115 / 19.406 / 28.169 |

The final pair's source and delivery cadence are worse than the earlier pair.
Renderer interval medians are near 119.918 Hz but do not account for every missed
vblank. Repeat under controlled CPU/GPU load and measure the producer's overall
rate before promoting any scheduling change. Do not select only the favorable
tail result or claim a whole-product victory.

At 17:49 UTC the final locked release workspace test run passes all 134 ordinary
tests (host 16, core 78, Windows 39, Vulkan 1); fifteen native tests remain
opt-in. Formatting, `git diff --check` and all-target Clippy with warnings denied
pass. Logs: `handoff-tests.log` and `handoff-clippy.log`. The current normal host
release build passes. Previous
native checks validate embedded shaders; no GPU workload was started during
the final wrap. The current source is not packaged or installed.

Active work through 17:42 covers about five hours, with the 13:20–13:29 gap
excluded; this was not a full day of active work. The user-requested wrap
records results and checkpoints code rather than continuing experiments.

## Handoff priorities and reproduction

1. Review the final source checkpoint; package/install it only when the actual
   installed host is idle. Existing package/ZIP and installed host still belong
   to `36bff835a`. Preserve configuration and pairings. Verify the negative-key
   launch and complete startup HDR flags with Artemis after installation.
2. Establish a repeated matched full-stream baseline at native 1968×2184 HDR
   120 fps, same scene, codec, bitrate and client. Diagnose capture age, send
   intervals and mouse smoothness. Keep every scheduling experiment disabled
   until both picture age and cadence improve reliably.
3. Resolve AMD AV1 decoded geometry: requested 1968×2184 becomes 1984×2186;
   1920×1080 becomes 1920×1082; 2184×1968 becomes 2240×1968. Driver FrameSize,
   alignment and explicit crop attempts have not fixed it. Do not weaken exact
   checks, falsify headers or silently change the requested picture.
4. Reproduce the HDR10 warning and display restoration error 31. NVIDIA/Intel,
   NGX, VHF feedback and full VRR/HDR client acceptance still need appropriate
   hardware. Keep the PR draft; neither latency acceptance nor release readiness
   is established.

All fixtures and raw results are in the artifact directory stated above.
`installed_state.py` reads the real connection ledger with a quiet interval;
`guard-idle.py`, `run-native.py`, `run-motion.py` and `probe-startup.py` are
normal-user runners. The physical motion runner autodetects the existing source,
stores host SHA/settings, uses a test-owned tone and saves no desktop frame dump.
`BUTTERPOLLO_TEST_POLL_SESSION_API=0` disables intrusive fixture API sampling.
The final Rust motion binary SHA-256 is
`5c6689313bff0ccb09d9d4ccaf180be4193900a4a07ec1242a1c358ad1631022`.

The pinned C++ baseline is `8a8c4b03a280ab9f567beb380110abb80f5220b8` in
`vibepollo-baseline-source`/`vibepollo-baseline-build`, with its exact modules and
startup-only test isolation patches. Its capture/codec/stream hot path is
unchanged. Do not run a globally installed C++ host for these isolated fixtures:
its startup recovery can affect the real display arrangement.

Native Rust environment: dot-source the sibling
`performance-probe/rust-env.ps1` and put `C:\msys64\ucrt64\bin` first on PATH.
Use `cargo test --locked --release --workspace`,
`cargo clippy --locked --workspace --all-targets -- -D warnings` and
`cargo fmt --all -- --check`. Native GPU checks require the idle guard.
Artifact Python is `..\test-python\Scripts\python.exe`; fixture commands and
matched settings are retained alongside their logs.

Automatic approval review rejected the elevated SYSTEM benchmark with
"blocked by policy". It was not retried through another privilege mechanism.
Subsequent tests used normal-user physical outputs. Routine idle-safe updating
was a separate accepted action. Customer streams and credentials were untouched.

Update this file with concrete changes, measurements, failures and next steps.
Record active work intervals separately from elapsed wall time and scheduled
idle gaps. A schedule running for a day does not prove a day of active effort.
