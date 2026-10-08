# Windows performance work — October 2, 2026

User objective: make the Rust host smoother and lower latency than Vibepollo,
without reducing features or picture quality. Opus took over from Codex in the
evening of October 2. Performance acceptance on the customer's own sessions is
still open; the measured fixture results below are local loopback evidence.

## October 8 encoder stall recovery backs off (RX 9070 XT report)

Report: RX 9070 XT host, HD 630 client on Streamlight, 4K60 HEVC 80 Mbps,
rc.22: freeze, then the stream ends, with `encoder_recovery` "returned no
frame for 100 ms". That rule dates from rc.19 (`07a8a55e`); rc.22 only moved
the message onto the stream card. When two frames sit in AMF for 100 ms the
encoder is recreated, and a recreated encoder that was silent for another
100 ms was recreated again, about ten create/destroy cycles a second, until
the 5 s budget ended the session ("the encoder stopped returning frames").
A fresh encoder's first 4K keyframe on the 9070 XT's single VCN beside a game
can outlast 100 ms, so the loop could never let it finish.

Change: the first stall still recreates after 100 ms; each further
recreation without a frame doubles the wait (200, 400, 800 ms), and any frame
out resets the stall timer even if the in-flight count did not drop. The
5 s budget is unchanged and streams without stalls take the same path.
Follow-up: a stuck encoder now steps to safer settings instead of being
recreated unchanged until the stream ends. The second recreation in one
failure (or a second failure in the session) converts colour on the
graphics queue, as before only after a second failure; the third switches
AMF to its conservative profile for the rest of the session (low-latency
usage, no SmartAccess Video, LTR, forced queue, LowLatencyInternal or
pre-analysis), the settings tied to RDNA4 stalls in foundation-sunshine#666.
The stream card says so. The give-up budget is 10 s instead of 5 s, so a
driver reset (Windows allows about 2 s plus recovery) no longer ends it.
Not measured: the RX 9070 XT is not available, and the RX 7900 XT has not
reproduced the stall. The debug soak's encoder-failure fault exercises the
encode-error path, not this one; a stall injection is still to be written.

## October 8 rc.22 baseline at the owner's settings

The goal is now a finished product measured at the owner's own settings
(1968×2184 HDR, 120 fps, AV1 and HEVC) plus 1080p60 and 1440p120. This is
the host-side baseline every later latency change is compared with.

Fixture: `run-motion.py` from `bench-rc17` copied to
`bench-rc21` in the artifacts folder, unchanged except for size, rate,
bitrate and decoder options. Isolated host started as SYSTEM in the signed-in
session, virtual HDR display at twice the stream rate, WGC capture (the
default), compute conversion on, native AMF at ultra-low latency with
`speed`. Picture age is the timestamped rendered picture to independent
decoding on loopback. Bitrates requested: 80 Mbps native, 50 Mbps 1440p, 20
Mbps 1080p. rc.22 is the installed release (`3adaa1c22345b6fd`). Three runs
per row; the numbers are the means of the runs' mean, p95 and p99.

The independent client must decode in hardware at these sizes: with FFmpeg's
software HEVC decoder (`moonlight-motion-client-dump.exe`, 8 threads) native
120 fps took 7.8-8.1 ms per frame, the queue grew and picture age reached
340 ms mean with a 950 ms p99. The client from `rust/tests/moonlight_client.c`
built with `BUTTERPOLLO_TEST_HW_DECODER=d3d11va` decodes in about 5 ms. For
AV1 it must pick FFmpeg's `av1` decoder (the default, libdav1d, has no
hardware path); `rust/tests/moonlight_client.c` now does that when a hardware decoder is set.

| rc.22, idle | Picture age mean / p95 / p99 (ms) | New pictures/s | Host latency |
|---|---|---|---|
| 1968×2184 HEVC HDR 120 fps | 13.71 / 14.40 / 15.10 | 120.3 | 3.5 ms |
| 1968×2184 AV1 HDR 120 fps | 13.45 / 14.08 / 15.01 | 120.5 | 3.1 ms |
| 2560×1440 HEVC HDR 120 fps | 12.48 / 13.12 / 14.03 | 120.4 | 3.0 ms |
| 2560×1440 AV1 HDR 120 fps | 12.22 / 12.90 / 13.73 | 120.4 | 2.8 ms |
| 1920×1080 HEVC HDR 60 fps | 14.60 / 15.83 / 17.57 | 60.3 | 2.3 ms |
| 1920×1080 HEVC HDR 60 fps beside `gpu_load 45 1000 0 200`, software decode | 33.14 / 41.94 / 44.38 | 59.1 | 2.0 ms |

Native AV1 still decodes as 1984×2186 (AMD's AV1 alignment), so its runs fail
the fixture's strict size check; the timings are complete. The load row uses
the software client as on October 7 (33.4 ms for rc.17 WGC then): with a
hardware decoder on the same GPU as the load, decoding starves and picture
age reaches seconds, which measures the client, not the host. Native-size
rows beside a load therefore need the separate laptop client. An earlier
rc.21 batch matched these idle numbers within 0.3 ms, but some of its runs
overlapped the rc.22 release session's own e2e streams and it is not used.

Artifacts: `bench-rc21\r22-*` (baseline), `bench-rc21\ab-*` (below).

### Reconnect and issue #6 fixes, no latency change

The sign-in, listener and control-timeout fixes committed alongside this
entry were checked against rc.22 in one alternating batch, three runs each:

| | rc.22 | With the fixes |
|---|---|---|
| 1968×2184 HEVC 120 fps idle, mean / p95 / p99 | 13.66 / 14.39 / 15.15 | 13.76 / 14.49 / 15.21 |
| 1080p60 beside the load, mean / p95 / p99 | 33.25 / 42.02 / 44.76 | 32.94 / 42.02 / 44.39 |

Every run decoded all received frames with zero failures. The differences
are within the spread of the runs. The fixes themselves (a reset during
accept, a session that loses its control peer, a 5-10 s Wi-Fi drop, a launch
before sign-in) are covered by code review and unit tests; they still need a
network test on 192.168.4.10 and a signed-out launch on this host.

### Native size beside a game, and two rejected settings

The October 7 load (`gpu_load 45 1000 0 200`, uncapped) starves the
loopback client's hardware decoder at 1968×2184 120 fps: decoding took 8.3-12.6
ms a frame, above the 8.3 ms period, and picture age grew to seconds. Lighter
uncapped loads (fewer draws or iterations) did the same. A game capped at 60
fps (`gpu_load 45 1000 60 200`, 5.3-5.6 ms of GPU work per frame) leaves the
decoder at 4.7-6.8 ms and is the native loaded cell from now on. A game
capped at 120 fps beside 1440p120 sits at the decoder's limit (8.2 ms) and
its runs spread from 17 to 172 ms, so that cell is not used.

rc.22, 1968×2184 HEVC HDR 120 fps beside the 60 fps game, seven default runs
from three batches: picture age 17.2-18.7 ms mean, p95 18.8-21.7, p99
19.3-22.6, 120 new pictures a second. The host's own split shows where the
load costs time: `claim_wait` (copy submitted to encoder claim) rises from
0.06 ms idle to 1.0-3.0 ms mean, p95 6 ms, and `frame_age` from 0.2 to 1.6-3.5
ms. Encoding stays at 3.3 ms. `claim_wait` is bimodal by run (about 1 or 2.8
ms), which follows the phase between the 60 fps game and the 120 fps stream.

Two settings were tried against that wait, alternating with the default in
one batch each, three runs per row:

| Beside the 60 fps game | Picture age mean / p95 / p99 (ms) | claim_wait mean |
|---|---|---|
| Default (compute queue priority HIGH) | 18.11 / 20.15 / 21.51 | 2.81 ms |
| `compute_queue_realtime=true` (GLOBAL_REALTIME granted) | 17.93 / 19.96 / 21.40 | 2.71 ms |
| Default, second batch | 17.64 / 19.08 / 19.93 | 2.22 ms |
| `frame_pacing_source_phase=false` | 18.09 / 20.23 / 21.77 | 2.23 ms |
| also `frame_pacing_predictive=false` | 17.87 / 22.62 / 24.54 | 2.72 ms |

A realtime copy queue changes nothing, so the wait is not the copy queuing
behind the game's compute. Turning off source-phase or predictive pacing
does not remove it either and makes the 95th and 99th percentiles worse.
Both stay at their defaults. Next: trace where the claim waits (the copy
fence, WGC delivery, or the session thread busy in `QueryOutput`), which is
what the sender-thread and AMF-poll work would change.

Artifacts: `bench-rc21\lp-*`, `rt-*`, `pc-*`.

### First remote client: the laptop over Wi-Fi

Moonlight-qt 6.1.0 on a Radeon 780M laptop (driver 32.0.31035.1003, D3D11VA,
panel not HDR) on 5 GHz Wi-Fi, against the installed rc.22 host on Ethernet,
October 8 07:04-07:12 UTC. Desktop app, `motion_probe` drawing on the
streamed display for every run, client "lap" on the Extended layout so the
owner's monitor stays on. Moonlight's whole-session statistics, one 35 s run
per row (averages only, startup included):

| Run | Received fps | Host processing min / max / avg | Decode |
|---|---|---|---|
| 1968×2184 AV1 HDR 120 fps, 80 Mbps | 121.2 | 2.6 / 4.5 / 2.9 ms | 0.37 ms |
| 1968×2184 HEVC HDR 120 fps, 80 Mbps | 121.0 | 3.1 / 13.9 / 3.5 ms | 0.38 ms |
| 2560×1440 HEVC HDR 120 fps, 50 Mbps | 121.0 | 2.8 / 14.9 / 3.0 ms | 0.51 ms |
| 2560×1440 AV1 HDR 120 fps, 50 Mbps | 121.2 | 2.4 / 4.3 / 2.6 ms | 0.62 ms |
| 1920×1080 HEVC HDR 60 fps, 20 Mbps | 59.5 | 1.9 avg, 9.2 max | |
| 1920×1080 AV1 HDR 60 fps, 20 Mbps | 59.8 | 1.6 avg, 8.5 max | |

Network jitter drops stayed at or below 0.13% at 120 fps. The HEVC maxima of
14-15 ms are each stream's first frames, not a steady-state difference
between the codecs: in the host's own 5 s windows during these runs HEVC
peaked at 4.1-4.7 ms native and 3.4-3.8 ms at 1440p, with p99 3.3-4.2 ms, as
AV1. In the 18 loopback runs above, the first keyframe took 12.7-17.7 ms on
both codecs and frames 2-5 sometimes 7-17 ms; after that no frame exceeded
6 ms. Warming the encoder before the first frame would only shorten stream
start. A first 1080p60 pair overlapped the owner changing his monitor mode
(one 48.7 ms frame) and was repeated.

Render-to-decode delay over the network is not measured yet: the
independent client must launch from the laptop's own paired identity, since
the host ties a session to the address that launched it.

## October 5 rc.3 release continuation

The user approved WGC compute by default for the next test release after
reviewing the latency/freshness tradeoff below, and authorized publication.
`wgc_compute_copy=false` and the global compute switch remain available.
Shared captures now respect those settings. The fixed 150 ms recovery wait
has been replaced by acknowledgements after each stream releases old GPU
resources; failed reopen attempts still back off.

The reporter can run the candidate but is only reachable through the user on
Reddit, with no reply time known. There is no access to the RX 9070 XT. Do not
close or advertise a fix for the exact 4.7 versus 3.9 ms latency report based
on local RX 7900 XT/wired tests. Validation and release records follow in
PERFORMANCE.md; earlier dated records below describe their original state.

## October 5 continuation from OpenCode

The active OpenCode work moved from `vibepollo` into this `butterpollo-rust`
worktree. Opus shipped rc.2, fixed stream termination on transient UDP send
errors, compared the DDX path with Vibepollo 2.0, and prepared the launch posts.
Its final unfinished task was WGC measurement and optimization, followed by
investigating packet-burst pacing on slower client links.

The last WGC smoke test succeeded as the interactive user but failed under
SYSTEM at `CreateForMonitor` with `0x80070424`. Opus left additional error
contexts in `windows/src/capture.rs`; those are preserved. The service's host
runs as SYSTEM in the interactive session, so this is a host limitation, not
just an isolated-harness failure.

The continuation on `codex/wgc-capture-recovery` shares the startup/recovery
fallback to DDX and investigates event-driven WGC wakeups. Explicit capture probes stay
strict: requesting WGC cannot silently benchmark DDX. Native reconnect testing
also found that revoking `FrameArrived` after the pool closes aborts inside
Windows; notification removal now precedes pool closure and is idempotent.
Pure notification waits improved idle detection but regressed under GPU load,
so notification registration remains opt-in for the probe; production capture
keeps polling. These changes are local and unreleased. The installed host remains rc.2.

Validation: all 177 workspace tests pass (19 hardware-specific tests remain
ignored by the default suite), Clippy passes with warnings denied, and the
release host builds. The explicit native WGC test passes 16 reconnect/COM
teardown cycles. An isolated user-mode WGC stream decodes 715/715 HEVC frames
at 1080p60 with zero failures and nonzero audio. No SYSTEM-context runtime
retest or installation was performed. The quantitative wakeup experiment and
its limitations are recorded in [PERFORMANCE.md](PERFORMANCE.md#october-5-wgc-startup-and-notification-experiment).

Follow-up: service-mode WGC needs a capture helper running as the signed-in
user. DDX fallback does not establish equivalent VRR or generated-frame
behavior. The subsequent WGC compute-copy follow-up below validates content
and synchronization independently before measuring full-stream latency.
The subsequent follow-up adds completion-based packet pacing with wire
overhead and a cap for known local Ethernet links. A late send no longer
creates a catch-up burst. Three core tests cover the cap, delayed sends and
per-datagram overhead. Independent UDP stress tests on the NUC at
`192.168.4.10` cover integrity, real socket overruns and a separately labeled
modeled bottleneck. This wired fixture does not reproduce the reporter's Wi-Fi.

The user identifies the reporter's GPU as RX 9070 XT, with the latest driver.
Their rc.2 log reports one HEVC instance and transient UDP errors 10055/10035.
The 4.7 versus 3.9 ms comparison remains unresolved. Local 7900 XT tests
confirm the existing compute conversion helps under GPU load; disabling
multi-instance encoding has no useful local improvement. An output-wait
optimization improved component and idle results but regressed loaded LAN
host mean from 6.102 to 6.501 ms, so it was reverted. Do not promote it based
only on the favorable component measurement.

A native DDX failure led to finding a missing display-awake request in Rust's
capture worker. Vibepollo holds this request. The workstation's idle timeout is
three minutes; the unchanged snapshot test passed once with a temporary
request, but later failed again even with a moving source and that request.
Its failure remains unresolved; do not attribute it solely to display sleep.
The worker now holds a thread-bound guard which preserves and restores prior
requirements. This does not establish the cause of the reporter's restarts.

The 230-second wired LAN pair crossed the workstation's 180-second display
timeout. The control recorded no fresh capture claims in all ten samples
after 180 seconds; the revised worker recorded fresh claims in all ten.
Both decoded every received picture and the test tone, with zero codec
failures. Both still required two startup DDX restarts. Preserve that open
startup issue and the failing standalone DDX test in the handoff.

Before the compute-copy follow-up, the workspace/native run passed 198 checks
(180 ordinary and 18 native), excluding the known failing AV1 geometry check
and unavailable NVIDIA hardware. The DDX test passes in that active-desktop
state; earlier inactive-desktop failures remain open. Final LAN checks pass
H.264, HEVC HDR, aligned AV1 and explicit WGC at approximately 60 FPS, with
exact geometry, nonblank pictures, test-tone audio and zero decode errors.

The independent C receiver now runs on Linux as well as Windows, optionally
uses hardware decoding with readback of every picture, checks pixel contrast,
and can require a minimum steady frame rate. The NUC passed 1080p60 HEVC but
could only deliver 27.588 FPS in the 4K60 readback case; that is a failed
performance gate, not a successful 4K60 test. Exact unaligned AMD AV1 geometry
was rechecked and still fails for all twelve SDR/HDR/alignment combinations.

Local validation artifacts are in
`C:\Users\ramaz\.codex\artifacts\butterpollo-wgc-20261005`; the NUC fixtures
are in `/home/rambo/butterpollo-tests-20261005`. Complete measurements and
reproduction are in [PERFORMANCE.md](PERFORMANCE.md#october-5-lan-pacing-and-encoder-follow-up).

That earlier release host built and Clippy/format/diff checks passed. Its SHA-256
is `ff9da9483253a3e5b70737ad7e77006e689effc24ebc4519e0a719dcd66880d2`.
The final 4K60 HEVC loopback check decodes 985/985 received pictures at 60.585
steady FPS, but includes startup blank pictures and recovery time. No installed
service or profile was replaced; test-owned processes and receiver containers
are stopped. Changes remain uncommitted on `codex/wgc-capture-recovery`.
`validation-summary.json` records the checks, limitations and source hashes.

### Subsequent WGC compute-copy and startup investigation

The user asked which optimizations were reverted before continuing. The two
reversions remain default WGC notifications and the shorter encoder-output
wait. Neither was re-enabled. Supported AMD WGC capture can use DDX's fenced
compute handoff and compute AMF conversion with `wgc_compute_copy=true`, with
graceful graphics fallback. Default activation was subsequently reverted after
the corrected comparison below; the option remains experimental.

Native WGC verification compares the candidate before the reference: 120
exact frames and 119 changing pictures at idle, then the same under GPU load,
including a snapshot retained after teardown. Eight complete HEVC streams
in alternating order found loaded host mean 8.557 → 1.953 ms and decoded
picture age 49.052 → 40.630 ms. Idle picture age was 31.432 → 31.835 ms.
The strict distinct-frame gate still fails in several control/candidate runs;
do not equate approximately 60.6 transport FPS with 60 distinct pictures.
See the full table and limits in PERFORMANCE.md. This is not a 9070 XT retest.

A second native regression test reproduced compute-sharing fallback losing
the only available frame. It now copies that same frame through D3D11, and
the test passes. The original DDX arrival probe also falsely reported one
frame when it had no samples; actual frame and presentation counts are now
separate. The new startup probe found no DDX frames while Windows reported
the display off, while WGC supplied one cached image. During a cold stream,
raw duplication observed 5120×1440 BGRA → 3840×2160 FP16 → 5120×1440 BGRA,
coincident with two access-loss events. The initiator remains unidentified;
display changes were disabled in the fixture. A warm repeat had no restart.
Do not add a blind delay or reject legitimate dark frames to hide this.

The motion probe's numeric rate previously changed physical refresh. It now
paces only the animation, with before/during/after checks confirming the
physical 5120×1440 output remains at 240 Hz. The initial comparison above used
the old 60-Hz fixture and forced static repeats at 60 FPS. A four-run reversed
comparison found that using production's existing 20-FPS repeat floor restores
59.9–60.0 distinct FPS at idle instead of 44–46, without changing production.

Eight further streams with production's repeat floor and the corrected fixture
found compute reduces loaded picture age from 52.859 to 34.603 ms, but distinct
FPS drops from 51.423 to 49.955. All four loaded runs fail the 58.2-FPS gates;
all four idle runs pass. This prompted reverting the compute default too.
Keep the option available, the failures visible, and the user's RX 9070 XT
acceptance open. Reports: `repeat-cadence`, `wgc-compute-abba3`.

The moving-desktop workspace/native run passes 200 checks, including 20 native
checks. The unavailable NVIDIA test and known failing AV1 geometry test remain
excluded. The final retained build and validation are recorded in
`validation-summary.json`; the installed service and profile remain unchanged.
Final retained host SHA-256:
`c8341cefcb1cdfc50f8038e735c412175571222a743da5f0ba6ec6a388366959`.
The final run again passes all 200 available tests. Wired default WGC HEVC,
opt-in compute HEVC and opt-in compute HEVC HDR all deliver approximately 60
distinct FPS with zero decode errors. An absent-motion negative check correctly
fails despite decoding all 724 received pictures. Reports: `retained-native-final`
and `retained-lan-final`. All workspace release binaries and Clippy pass.

## October 3 installed state (historical)

Installed revision: `8db29eee4` (October 3, 07:37 UTC; `butterpollo.exe`
SHA-256 `DB543650FC4824E0CDA61919E619E6419106E7B5D45A815AD0C541CB816ACA77`,
58 package files, profile preserved). It contains the scheduling work below
and the multi-stream and display fixes of October 3.
Push only to the owned `butterpollo` remote. Draft PR:
https://github.com/RamazanKara/Butterpollo/pull/1.

### Why the customer saw higher host latency than Vibepollo

1. Different counters. The C++ host sends Moonlight
   `frame_processing_latency = send - host_processing_timestamp`, taken when it
   picks the frame up (`src/stream.cpp:2037-2046`, `display_wgc.cpp:384`). The
   Rust host sent `send - presentation`, which adds the 2-3 ms a frame waits
   after Windows presents it. The Rust host now reports claim to packet, as the
   C++ host did, and logs the waiting separately (`frame_age`, split into
   `detect` and `claim_wait`), so it stays visible.
2. Real waiting. The encoder claimed frames on a fixed 120 Hz grid unrelated
   to presentation, so frames aged 2-3 ms before encoding (phase lottery: the
   value was fixed per session). Frames are now claimed on arrival
   (`frame_pacing = arrival`, default; `grid` keeps the old scheduler).
3. Coarse waits. Waitable timers on this PC wake 0.3-0.5 ms late for short
   waits (`windows/examples/timer_probe.rs`), so the 100 us AMF output poll was
   really ~0.5 ms and deadline claims were late. AMF now waits in the driver
   (`QueryTimeout = 1`, only while a frame is in flight); streams raise the
   timer resolution, opt out of power throttling and use high priority, as
   the C++ host did.

### Measured (October 2 evening, same binary, A/B)

Fixture: `day-work-20261002/run-motion.py av1 <label> rust physical-strip-motion`
with `BUTTERPOLLO_TEST_STREAM_SCALE=0.5` (physical 5120x1440 at 240 Hz
streamed at 2560x720/120 so the local software decoder is not the bottleneck),
`BUTTERPOLLO_TEST_DECODER_THREADS=4`, `BUTTERPOLLO_TEST_FRAME_PACING`.
`received_age.py` reports picture age minus client decode time (renderer to
fully received frame), which excludes the loopback decoder's CPU contention.

| Run (v6, final) | Picture age mean / p99 | Received age mean / p99 | Present to send | Frame age |
| --- | --- | --- | --- | --- |
| grid | 12.55 / 13.66 ms | 9.37 / 9.86 ms | 4.8 ms | 2.9 ms |
| arrival a | 10.40 / 11.74 ms | 7.10 / 7.85 ms | 2.5-2.6 ms | 0.40 ms |
| arrival b | 10.29 / 11.51 ms | 7.08 / 7.84 ms | 2.55-2.6 ms | 0.41 ms |
| arrival c (stopped when the customer connected) | - | 7.10 / 7.88 ms | 2.5 ms | 0.41 ms |

Zero late intervals and 120.0 unique fps in every run. Arrival pacing is
2.3 ms faster end to end than the grid on the same binary. The 1 ms timer also
helps the local renderer and client, so do not compare these absolute values
with runs before `d9a04bbb0`.

Three pacer defects were found with per-claim traces
(`BUTTERPOLLO_TEST_RUST_LOG=info,pacing=trace`, `claims.py`) and are covered
by tests: the session-sampled cadence estimate drifted (now the capture
worker's median interval); a fresh frame presented just before the claim slot
but detected after it lost to the older one; and a credit deficit from startup
persisted for a whole session at exactly the stream rate (credit now refills
1 % faster, claims need 7/8 of a frame of credit).

### Against Vibepollo 2.0 (pinned C++ baseline)

Same fixture, same scaled 2560x720 AV1 stream and client, run alternately
after the Rust build above was installed (22:32 local). The C++ baseline's
first run failed (its virtual display restart was denied, the renderer timed
out) and its display recovery re-enabled the HISENSE monitor and set the
Odyssey from 240 Hz to 120 Hz; the second pair therefore ran on a 120 Hz
source. The displays were put back afterwards (Odyssey 5120x1440 at 240 Hz,
HISENSE detached, as before the test). Do not run the C++ baseline fixture
on this machine again without isolating its display recovery.

| 120 Hz source | Rust | Vibepollo 2.0 |
| --- | --- | --- |
| Moonlight host latency (claim to send, both) | 2.05 ms | 2.47 ms |
| Picture age mean / p99 | 15.6 / 18.7 ms | 18.2 / 29.9 ms |
| Received age mean / p99 | 11.6 / 12.5 ms | 13.2 / 22.6 ms |
| Intervals over 1.5 periods in 30 s | 1 | 17 |

One pair only; repeat on the customer's real sessions before claiming it in
release notes.

### Build loop warning

The WSL clock ran 43 s behind Windows, so cargo on Windows skipped rebuilding
files edited in WSL within that window. `systemd-timesyncd` in WSL was stopped
and the WSL clock set from Windows. If edits seem to have no effect, compare
`wsl date` with Windows time and touch the sources.

## October 3: multiple streams and display restoration

The customer reported that multiple streams did not work. Fixed in
`c23384eae`..`8db29eee4`:

1. Reconnect lockout. A client launching again after an abandoned launch or
   stream got "client already has a session" until the old one timed out
   (four failures in a row at 05:09 UTC). A launch now supersedes the same
   client's launch or stream in that role and waits up to 5 s for its teardown.
2. Second client refused. A different display mode ("another stream owns a
   different display mode") or a running arrangement refused the second
   stream, and clients shared one retained display. Each client now has its
   own retained display, a mode set by one stream is kept (the other stream
   scales), and the arrangement is shared and re-laid out as streams come
   and go.
3. A second client's virtual display broke the first client's DDX capture:
   `0x887A0026` on every re-created duplication, 117 restarts, never
   recovered. Windows keeps returning the stale adapter while any device on
   it is alive (`multi_ddx_probe` reproduces this). The capture worker now
   drops the lost capture, withdraws the frame and waits 150 ms for streams
   to release their encoder before re-creating it. The first stream recovers
   after one or two restarts, about a second without frames.
4. RTSP refusals and failures were logged at debug; now warnings.
5. Display restoration. `Topology::set_mode_rate` trusted SetDisplayConfig:
   the HISENSE TV asked for 1080p120 came back at 60 Hz with no error, and
   re-applying the unchanged layout (`set_positions`) did the same. It now
   skips a display already in the mode, verifies the result, and falls back
   to the display's mode list. The layout recorded for restore no longer
   contains the stream's own virtual display (a retained one stayed as an
   invisible monitor beside the Odyssey; restoring its settings after it was
   removed is the likely source of the customer's "os error 31"). Restore
   continues past a failing display, names the failed step, re-checks rates
   after moving displays, and retries briefly while Windows applies another
   change ("cannot read display mode").

Fixtures (in `day-work-20261002`): `multi_stream.py` runs two paired clients
against an isolated host (concurrent join, retry after an unconnected launch,
resume after a client crash). `run-system-multi.ps1` runs it as SYSTEM so each
client gets its own virtual display (`-Layout extended`). The check
`capture_restarts_bounded` allows up to four capture restarts when a display
arrives. `restore_probe` re-applies the current layout, optionally step by
step, and prints every display's rate.

| Run | Result |
| --- | --- |
| Physical display, all scenarios | PASS, 0 capture restarts |
| SYSTEM, per-client virtual displays, before the fix | FAIL, 117 restarts |
| SYSTEM, per-client virtual displays, final | PASS, 1 restart; A 58.1 fps at 60, B 114 fps at 120; layout and rates unchanged; no restore warning |

The arrival fixture could not be compared with the 240 Hz runs above: the
Odyssey was at 120 Hz and the phone's retained 240 Hz virtual display was
attached. Same display state, installed `f44de5dd8` against the new build:
received age 13.6 / 19.8 ms against 13.2 / 20.2 ms (mean / p99), host p99
2.6 ms for both. No pacing regression.

Display state found on October 3 (not changed back without the customer):
Odyssey 5120x1440 at 120 Hz (240 Hz on October 2), HISENSE active at 120 Hz
until `restore_probe` reproduced the 60 Hz bug on it several times; it was
returned to 120 Hz and then went inactive about a minute later, while no
probe ran (TV standby or switched off; still connected). After the update
restarted the service it was active again at 1920x1080 at 120 Hz in its
usual place, and the retained virtual display was gone. The Odyssey is still
at 120 Hz.

Open: the customer's own configuration (exclusive layout, HDR virtual
display) was not run here because it turns the physical monitors off. If
error 31 still occurs, the warning now names the display and step.

## October 3 evening: Vibepollo 2.0 parity

Goal from the customer: Butterpollo replaces Vibepollo 2.0 for its users,
with the latency wins kept and Vibepollo's behaviour everywhere else.
`rust/PARITY.md` is the current list. Done since `967fe6a87`:

- Setup (`rust/setup`): `butterpollo-setup-<version>.exe` upgrades a
  Vibepollo installation in place (drivers as SYSTEM, service, firewall,
  shortcuts, uninstall). Installed on this PC; the host name is now `homepc`.
- New web console (`rust/web`, Svelte 5); the host serves it from
  `assets\web` and keeps the server-rendered pages as a fallback.
- Vibepollo replies for `/bitrate` (capped by `max_bitrate`), `/unpair` on
  HTTP, ABR capabilities, the applist placeholder and
  `VirtualDisplayDriverReady`.
- Device `display_mode`, the config override allow-list, Vibepollo's
  `frame_limiter_auto_virtual_framegen` spellings, `--creds`, credential
  folder permissions at start, the display restore hotkey, an invalid
  `apps.json` no longer stopping the host, and letterboxing in the software
  encoders.
- Steam library sync (`core::steam`, `host/src/steam.rs`): verified against
  this PC's 17 installed apps in three libraries; covers from the cache or the
  store. A Steam app's stream follows the game's processes.

- Playnite (`core::playnite`, `host/src/playnite.rs`) through Vibepollo's
  plugin, and Lossless Scaling (`core::lossless`, `host/src/lossless.rs`),
  both tested against stand-ins because neither is installed on this PC
  (the customer's profile has Playnite apps and the fullscreen entry).
- Tray notifications for pairing and new versions; release checks compare
  versions and skip streams.

Still missing: the virtual display render GPU and reclaim after restart,
Playnite focus retries and fullscreen relaunch, `/api/browse`.

Unverified here: the secure desktop during a stream, streaming the sign-in
screen after a reboot, a Steam game ending its stream, the restore hotkey.

## October 4: latency beside a game

Customer request: the lowest possible encode and end-to-end latency. Encode
is at the VCN floor when idle; the remaining cost was D3D11 work queued behind
a game on the graphics engine. Captures are now copied and converted on D3D12
compute queues and AMF encodes from D3D12 (`cc05d5018`, details and numbers in
`PERFORMANCE.md`). Idle picture age fell 0.6 ms; beside a heavy GPU load it
fell 7-9 ms with more new pictures per second. `gpu_compute_conversion = false`
restores the graphics-queue path.

Probes (`156c9f348`): `gpu_load`, `d3d12_probe`, `copy_probe`,
`ddx_sync_probe`, and `performance --live-capture --arrival` for
present-to-bitstream timing. Scripts in
`<artifact>\day-work-20261004` (`load_matrix.py`, `e2e-latency.ps1`,
`ddx-sync.ps1`, `live-latency.ps1`, `guarded.py`, `quiet.py`).

Lessons for test runs:

- Run GPU tests through `guarded.py`: it refuses to start unless the
  installed host has been quiet for two minutes and kills the test the moment
  a client launches. An idle check right before a test is not enough; a
  customer session started a second after one passed.
- Do not start the isolated host while the installed host has an app running
  (`RustHostApplicationActive`). Its virtual display heartbeat then failed
  with ERROR_BUSY and the customer's exclusive layout was replaced by the
  physical displays. `quiet.py --host` checks this.
- This PC sleeps when idle; a long run may resume hours later.

Next: AMD AV1 still pads 1968×2184 to 1984×2186 (AMF issue 423). Only AMD
GPUs use the compute queues; NVIDIA and Intel keep the graphics-queue copy
and conversion until the path is tested there.

Waiting in Desktop Duplication instead of polling was measured and rejected
(`examples/ddx_arrival_probe.rs`, `day-work-20261004\ddx-wait`, `e2e11`,
`e2e12`). A blocking `AcquireNextFrame` returns a frame 0.08 ms after its
present (p95 0.11 ms) against 0.39 ms (p95 0.89 ms) when polling every
0.5 ms, but it holds the device's immediate-context lock: another thread's
`Flush` meanwhile took 1.1 ms at the median and up to 56 ms (texture
`GetDesc` and output `GetDesc1` are unaffected). With AMF on compute queues
nothing else used that context, and idle streams gained 0.16 ms of picture
age over four runs. Beside the game load, waiting was worse in all three
pairs: 30.6 against 29.2 ms picture age, 91 against 94 new pictures per
second, 0.7 ms more host time. Capture keeps polling.

## Next work

1. Install the current source while the host is idle (package with
   `build.ps1 -Dependencies <artifact>\bootstrap-sdk -TargetDirectory
   <artifact>\target -NvidiaRoot <artifact>\ngx-sdk -MsvcSdk
   <artifact>\msvc-sdk -Package` after dot-sourcing
   `performance-probe/rust-env.ps1`). Check the customer's next 1968x2184 HDR
   sessions: `frame_age`, `host_mean` (now claim to packet) and the Artemis
   HDR10 warning after a host restart.
2. Repeat the A/B against the pinned Vibepollo baseline with the scaled
   fixture (picture and received age), now that the counters agree.
3. Encode is 0.2-0.3 ms slower when claiming right after composition (the
   conversion waits for the capture copy). Investigate converting directly from
   the duplicated surface.
4. AMD AV1 padded decoded size remains open (see below).

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
- Release only at the customer's request (2.0.0-rc.1 on October 4), and name
  unverified hardware and missing features in the release notes.

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
