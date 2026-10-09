# Windows performance work — October 2, 2026

User objective: make the Rust host smoother and lower latency than Vibepollo,
without reducing features or picture quality. Opus took over from Codex in the
evening of October 2. Performance acceptance on the customer's own sessions is
still open; the measured fixture results below are local loopback evidence.

## October 9 AV1 padding at 1968x2184

**Investigation only; no encoder, shader, codec policy or service changes.**
The RX 7900 XT's extra coded area is generated inside AMF/VCN, outside the
host's logical input texture. The host already reuses textures and converts
only the requested picture. There is no demonstrated uninitialised host
padding to fix, and no measured benefit that justifies changing allocation,
alignment mode or AV1 headers. No new size/alignment logic or unit tests were
added. H.264 and HEVC are unchanged.

This review used source, the installed SDK headers and saved October 2
fixtures. The GPU remains unavailable pending reboot: no hardware tests,
capture, encoding, live streams or service operations were run. New evidence
is CPU-only decoding of an existing synthetic D3D11 AV1 fixture, not a fresh host
measurement. Review artifacts and check logs are in
`C:\Users\ramaz\.codex\artifacts\butterpollo-av1pad-20261009`.

### Alignment and the actual padded picture

The SDK used by `windows/build.rs` includes `AMF/components/VideoEncoderAV1.h`
from `BUTTERPOLLO_FFMPEG_ROOT/include`. Its `Av1AlignmentMode` enum is:

| Value | SDK suffix | Meaning for this RX 7900 XT |
|---|---|---|
| 1 | `64X16_ONLY` | Requires width divisible by 64 and height by 16; cannot accept 1968x2184. |
| 2 | `64X16_1080P_CODED_1082` | Same restriction, with a 1920x1080 exception producing 1920x1082. |
| 3 | `NO_RESTRICTIONS` | Current public constructors; accepts the requested input but permits padded output. |
| 4 | `8X2_ONLY` | Smaller alignment exists in the API; the saved RDNA3 experiment still produced the same padded output. |

AMD documents black padding and the older hardware restriction in its
[AV1 guide](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/AV1-Encoder#av1-specific-api).
The [SDK header](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/8c648005e07d4309033282bfd9947df2c7e76104/amf/public/include/components/VideoEncoderAV1.h)
also exposes `Av1WidthAlignmentFactor` and `Av1HeightAlignmentFactor`.
AMD identifies newer hardware with smaller requirements in
[issue 423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423#issuecomment-2692368597);
setting a newer enum does not remove the RX 7900 XT's hardware limitation.
Read back those capabilities after reboot rather than opening AMF now.

This is **not simply width-to-64 and height-to-2 rounding**: 2184 is already
even. FFmpeg's [AMF crop calculation](https://github.com/FFmpeg/FFmpeg/commit/2128c1773956d96a9f6dad06aabd93ee4715b2e3)
uses the reported alignment factors (64x16 fallback for Navi3x), rounds each
dimension up, then replaces an eight-row bottom pad with two rows because of
a hardware special case. Thus the usual 16 extra columns remain, while
2184 would need eight rows to reach 2192 but gets two, reaching 2186.
This explains more than the old guide's specifically named 1080p exception.
It matches all saved SDR/HDR, mode-3/mode-4 cases in
`butterpollo-rust-20260930/day-work-20261002/av1-geometry/geometry.json`:
1920x1080 -> 1920x1082; 1968x2184 -> 1984x2186;
2184x1968 -> 2240x1968. Property readback still reports the requested sizes.

The saved HDR header has `use_128x128_superblock=0`, so its coding
superblocks are 64x64. That is distinct from input alignment and signalled
frame dimensions: 2186 is not a multiple of 64. Both 1968x2184 and
1984x2186 occupy a 31x35 superblock grid, with partial edge superblocks.
There is no additional whole superblock row or column here.

### Who fills it, and what is converted each frame

`windows/src/amf.rs` passes the requested width/height to `FrameSize`, AMF
`Init` and the converters, and calls `SetCrop(0, 0, width, height)` before
submission. That surface crop controls input; it has not produced an output
crop in the saved bitstreams. In `amf/gpu.rs`, the native D3D11/D3D12 surface
wrappers retain the converted texture without a host-side padded copy.

- `compute.rs::Converter::target` allocates **1968x2184 P010** for HDR (NV12
  for SDR), reuses free targets and caps the pool at eight. There is no
  explicit host clear, but `shaders/color.hlsl::yuv420_cs` writes every logical
  luma and chroma sample before submission; AMF waits on the conversion fence.
- `Converter::convert` dispatches `ceil((width/2)/8)` by
  `ceil((height/2)/8)`: **123x137 groups**, each with 8x8 threads, one thread
  per 2x2 block. The `targetSize/2` bounds check rejects the last four thread
  rows before reading or writing pixels. Conversion covers 1968x2184, not
  1984x2186 or the dispatch envelope of 1968x2192.
- The D3D11 fallback in `gpu_color.rs` likewise allocates the requested size
  and draws full luma/chroma viewports at that size. The CPU input path copies
  only logical rows into an AMF host surface. Resource pitch/tile alignment
  is separate from coded image padding; the host does not fill those hidden
  allocation bytes as pixels. Capture `CopyResource` operations copy capture
  textures, not an AV1-sized 1984x2186 image.

The black border is therefore supplied by AMF/VCN, not a host edge-replication
or clear pass. To check actual pixels without the GPU, FFmpeg 9.0.2 decoded
the existing `av1-1968x2184-hdrtrue-align3.obu` with `-hwaccel none`,
`-c:v libdav1d -threads 2` to planar 10-bit raw samples. In all eight frames,
every pixel in the 16 right columns and two bottom rows has Y=3 and U=V=512.
The adjacent visible right-edge luma is 431/615 in the fixture's two halves.
This is stable neutral, below-limited-range black after lossy decoding, not
replicated picture content or evidence of uninitialised host pixels. It does
not establish a universal raw fill value for every driver/input path.
The input SHA-256 is
`7ab9991b6ba3ab7802a108c856183a379ea7f0b2801169b4ce6dd34e16c003d0`;
the sample counts are in `border-samples.json` in the review artifacts.

### Cropping and the client

The saved keyframe's sequence header says
`max_frame_width_minus_1=1983`, `max_frame_height_minus_1=2185`.
Its frame header has `frame_size_override_flag=0`,
`render_and_frame_size_different=0`, and super-resolution is disabled.
Consequently the decoded and render sizes are both 1984x2186. **Render size
belongs to the frame header, not the sequence header, and is not a crop
rectangle.** The [AV1 specification, section 6.8.5](https://github.com/AOMediaCodec/av1-spec/blob/5e04f3f75e73a5898d7616c47c52f032144b8f80/07.bitstream.semantics.md#render-size-semantics)
defines it as an application display hint with no effect on decoding.
FFmpeg's [libdav1d wrapper](https://github.com/FFmpeg/FFmpeg/blob/c766e0de78b0239a36e49e50573e2297a7b500b4/libavcodec/libdav1d.c#L413-L426)
uses it to derive sample aspect ratio while retaining decoded dimensions.
Rewriting it could therefore imply scaling/aspect changes rather than cutting
off the border. The earlier render-size prototype below is not a general
crop fix; changing coded dimensions without re-encoding the tiles is unsafe.
FFmpeg's AMF container crop side data does not travel in this host's raw AV1
GameStream packets (`amf.rs` output -> `host/src/stream.rs` -> `core/src/packet.rs`).

[Moonlight Qt's FFmpeg path](https://github.com/moonlight-stream/moonlight-qt/blob/a57e947d7185fd01384f6973df4df59ae2968d01/app/streaming/video/ffmpeg.cpp#L1923)
has an explicit RDNA3 workaround: compare decoded dimensions with the
original requested dimensions, and crop right/bottom if each excess is
nonnegative and less than 64. Here that removes exactly 16 columns and two
rows with `av_frame_apply_cropping`. Its
[D3D11 renderer](https://github.com/moonlight-stream/moonlight-qt/blob/a57e947d7185fd01384f6973df4df59ae2968d01/app/streaming/video/ffmpeg-renderers/d3d11va.cpp#L887)
uses the resulting frame dimensions for the source rectangle and texture
coordinates. This path should show the original 1968x2184 picture without a
padding bar or squeezing the padded image; ordinary scaling to the client
window remains. Check for the client's cropping log and a border/grid image
after reboot to establish actual client acceptance.

The owner's exact client/version was not supplied, so that conclusion cannot
be applied to every Moonlight platform or fork. The inspected
[Android MediaCodec path](https://github.com/moonlight-stream/moonlight-android/blob/b48494cb96bff23d8886c4775cc4f39a1075495d/app/src/main/java/com/limelight/binding/video/MediaCodecDecoderRenderer.java)
configures the requested dimensions, uses scale-to-fit, and logs decoder
output-format changes; it has no equivalent negotiated-size crop correction.
Correct display there depends on the device decoder/output crop and needs a
real client check. A requested-size overlay alone does not prove cropping.

### Cost and post-reboot host A/B proposal

| Quantity | Value |
|---|---:|
| Visible luma pixels | 1968 x 2184 = 4,298,112 |
| Coded luma pixels | 1984 x 2186 = 4,337,024 |
| Extra per frame | 38,912 = **0.9053%** of visible pixels, not 1.4% |
| Extra at 120 fps | 4,669,440 luma pixels/s |
| Equivalent extra tightly packed P010 storage | 116,736 bytes/frame; 14.0 MB/s for one write at 120 fps |

The storage/bandwidth figures are arithmetic, not measured transfers. There
is **no extra host conversion dispatch or host copy for this coded border**.
A hypothetical full 1984x2186 conversion would use 124x137 groups instead of
123x137. VCN still codes the border and decoders reconstruct it, but constant
black, the unchanged superblock count, internal layouts and possible driver
copies prevent deriving an encode-time or bitrate penalty from pixel count.
AMF's closed implementation may do additional preparation; source inspection
cannot establish whether it clears or copies its internal padding per frame.

Allocating an aligned surface once and clearing padding once would only help
if it removed a measured AMF internal operation. The host already performs
the proposed visible-only conversion and pool reuse. Edge replication would
need updates when the edge changes. A one-time clear must initialize **each
pool surface**, use correct YUV black (limited P010 Y=64<<6, U/V=512<<6;
full-range Y=0), and finish before AMF reads it; clearing zero bytes would
give incorrect chroma. Merely enlarging `targetSize` would scale the desktop
and increase conversion work, so it is not a safe optimisation.

After reboot, first rerun the existing ignored
`amf::tests::native_av1_geometry_and_hdr_are_preserved` fixture with a fresh
`BUTTERPOLLO_TEST_GEOMETRY_DIRECTORY` and explicit executable paths in
`BUTTERPOLLO_TEST_FFMPEG` and `BUTTERPOLLO_TEST_FFPROBE`. It compares both
alignment modes in SDR/HDR and is expected to fail strict decoded dimensions
on this driver. Preserve that failure criterion. Record alignment capability
readback and the actual client's output crop, including a one-pixel border
and square grid to distinguish cropping from squeezing/scaling.

Only if profiling identifies an internal padding/preparation cost, try a
test-build A/B, **not a default or a new flag**:

1. **A:** Current 1968x2184 HDR input, mode 3, 120 fps and the owner's normal
   bitrate/settings/capture path. Save the bitstream and conversion, AMF
   submit-to-output and end-to-end frame-age timings.
2. **B:** AV1-only 1984x2192 (64x16) coded canvas and pool allocation, explicitly
   black-initialized once per surface, with the 1968x2184 picture at the
   top-left. Keep capture, negotiation, conversion coordinates and 123x137
   dispatch unchanged; use the full canvas for AMF FrameSize/Init/input crop
   and mode 1. This requires a confirmed client crop of 16 columns/eight rows.
   It codes **more** pixels than A, so a win is not assumed. Do not substitute
   a larger negotiated desktop, which would change content and scaling.
3. Before implementing B, add CPU unit tests separating visible size, storage
   size and dispatch: 1968x2184 -> 1984x2192 / 123x137;
   1920x1080 -> 1920x1088 / 120x68; already-aligned 1984x2192 stays unchanged.
   Verify all visible edge pixels and both chroma planes, pool reuse, first
   frame initialization and crop on the real decoder in subsequent GPU tests.
4. Alternate A/B/A/B with the same moving scene, refresh, HDR state, bitrate,
   warm-up and sample count. Compare p50/p95/p99 conversion and encode times,
   frame age, stalls/drops, frame bytes and cropped visible-image quality.
   Reject missing edge pixels, bars, scaling changes, colour contamination or
   gains inside run-to-run variance. Do not ship B without a repeatable win.

Validation passed using the supplied `performance-probe/rust-env.ps1`,
`CARGO_TARGET_DIR=D:\bp-build\av1pad-target` and the serialized
`pwsh -NoProfile -File C:\src\cargo-one.ps1` wrapper:

- `fmt --all -- --check`: passed.
- `clippy --workspace --all-targets --locked -j 2 -- -D warnings`: passed.
- `test --workspace --locked -j 2 -- --skip heartbeat_check_finds_the_monitors
  --skip encoders_open_on_the_configured_gpu`: passed; 590 passed, 44 ignored,
  two filtered out, zero failures. Ignored hardware fixtures were not enabled.
- `git diff --check`: passed. Only this findings section changed.

## October 9 Vibepollo fixture failure

Read-only investigation and fixture repair; no host, receiver, GPU workload,
service operation or build was run. Streaming validation must wait for the
reboot described below. Artifacts are under
`C:\Users\ramaz\.codex\artifacts\butterpollo-rust-20260930\bench-rc21`.

**The zero-video failure is a missing WGC helper in the pinned benchmark
bundle.** The `frz-wgc-*` label makes the fixture write `capture = wgc` for
both hosts. Vibepollo needs `vibepollo-baseline-build\tools\sunshine_wgc_capture.exe`,
which is absent; that directory contains only the display helper executable
and CMake files. The October 8 comparison used `capture = ddx`. The current
`sunshine.exe` SHA-256 is
`a539dd0f4d5fdee4537b317efee07e96b201a0c229e050a6af8a18225edb46cf`,
identical to `vp-vibepollo-nat-hevc-a/matched-settings.json`.

The four attempts failed at different stages. Paths in this list are relative
to `frz-wgc-vibepollo-rN`; host logs are under `config/logs/logs`:

- **r1:** `result.log:3-5` and all three `second-*/result.log` files fail with
  `KeyError: 'csrf_token'` in the Rust `interop_second_ab.py`. There is no
  stream launch. `baseline-20261009-014244-363.log:65` is the startup probe
  warning, before this independent login-contract failure.
- **r2/r3:** `result.log:64-75` shows `/launch` still using `timeout=10`.
  In `baseline-20261009-014341-371.log:180-199`, display setup runs from
  01:43:44.714 to `Executing [Desktop]` at 01:43:55.252 (10.538 s).
  `baseline-20261009-014533-494.log:180-200` takes 10.994 s. The timed-out
  primary never starts RTSP; later secondary clients reach capture and log
  the missing WGC executable (r2:259-261, r3:253-255).
- **r4:** the 60-second launch timeout gets past setup. In
  `baseline-20261009-014716-795.log`, lines 180-198 span 01:47:20.589 to
  01:47:34.391; line 195 creates `\\.\DISPLAY13`, line 218 connects the
  client, and line 221 selects that same output. At **01:47:38.557,
  lines 251-253**, `Failed to launch process: ...\tools\sunshine_wgc_capture.exe,
  error: 2` is followed by `WGC IPC helper failed to initialize; requesting
  capture reinit.` This repeats at lines 291-293 and 318-320.
  `result.log:39,44,54` records `TERMINATED error=-100`, no video traffic,
  and `frames=0 decoded_frames=0 audio_packets=1089`. `renderer.log:1-6`
  reaches first present on DISPLAY13, so selecting the wrong display or
  failing to start the renderer does not explain the absence of video.

The r4 first secondary starts at 01:47:44.489 (`second-clients.json`), after
the first missing-helper error. Its `second-0/result.log:6` returns 503 while
the primary disconnects and tears down (host log:321,346-372). Vibepollo's
`src/nvhttp.cpp:6351-6364` admits only one blocking stream mutation at a time;
this response does not diagnose a driver lock. Later secondaries also receive
zero video and hit the same missing helper. The final `motion_probe.exe`
timeouts in the run-level r2/r3/r4 logs are fixture teardown failures after
these earlier errors, not the original streaming failure.

**The logs do not support stopping the installed Butterpollo service.** r4
creates its own display and selects it for capture after the temporary-lease
warning.
More decisively, the successful October 8
`vp-vibepollo-nat-hevc-a/config/logs/logs/baseline-20261008-164312-585.log:66`
has the identical warning; its `result.log:56,67` reports 3,817 decoded frames
and `INTEROPERABILITY PASS`. In the pinned source at
`day-work-20261002/vibepollo-baseline-source`,
`src/platform/windows/virtual_display_sunshine.cpp:8257-8267` can return an
existing physical display without acquiring a temporary probe lease;
`src/main.cpp:839-841` still emits this warning when there is no owned lease.
It then validates encoders with synthetic surfaces. This is not evidence of
exclusive driver ownership by rc.26. Likewise, the unrecognized
`wgc_slot_aligned_publish`, `gpu_compute_conversion` and `prep_cmd` settings
appear in both successful and failed stdout logs (October 8:49-51, r4:48-50).
Omitting `minimum_fps_target = 20` retains Vibepollo's default 20
(`src/config.cpp:969`).

Edited only the external `run-motion-ab.py`, preserving its original bytes
as `run-motion-ab.py.bak` (SHA-256
`3e39523eb6adec8db538e2755292119bda67ccf2c712c589600ae939676eafe7`):

- Pin Vibepollo to its previously working DDX capture, announce it in the
  run log and record `capture: ddx` in matched settings. Butterpollo's WGC
  label selection and configuration remain unchanged.
- For Vibepollo, run the audio tone for `AB_MOTION_SECONDS` and allow helpers
  that duration plus eight seconds to finish. The former fixed 40-second
  tone and eight-second wait could invalidate the 60-second receiver /
  70- or 90-second renderer cases even after capture was repaired.
- For Vibepollo secondaries only, remove the primary's barcode/tone
  requirements and first-frame dump path. They retain decoding/audio checks
  and `AB_NO_CANCEL=1`; three client cycles and the existing delay remain.
  Butterpollo's secondary environment and timing are unchanged.

`interop_vp_ab.py` already contains the separate CSRF-token fetch, 60-second
launch timeout and no-cancel handling; these were retained. Neither it nor
`run-one-ab.ps1`, `batch-ab.ps1`, `run-motion.py` or the installed service was
modified. CPU-only checks passed: Python syntax, PowerShell wrapper parsing,
the backup hash, eight old/new Butterpollo config comparisons, unchanged
Butterpollo secondary commands/environment/timing, Vibepollo capture and
secondary isolation, helper durations, and the 15-column batch row below.
No streaming success or new performance result is claimed before the reboot.

After reboot, with the installed host idle, use this fresh row in the
`RunsFile` supplied to `batch-ab.ps1` through the existing SYSTEM/session
launcher (HEVC SDR 1080p60, 20 Mbps, 120 Hz source, D3D11VA, three secondary
cycles, 60-second receiver and 70-second renderer):

```text
frz-ddx-vibepollo-r5|vibepollo-baseline-build|hevc|1920x1080|60|20000|120|0||d3d11va|3|60|70||vibepollo
```

This is explicitly a DDX baseline. A WGC comparison needs the matching
baseline WGC helper packaged and capture selection restored. Check actual
source refresh and secondary display identities in the new artifacts before
using the result as a matched hotplug comparison: Vibepollo can join another
game client's existing output (`src/remote_session.cpp:263-268`), so
`per_client` and three connections alone do not prove three new displays.

## October 9 AMF settings review

Source audit of `59bcc477` plus the two fixes below, for the RX 7900 XT
(RDNA3, two VCN instances). No encoder workload, quality probe, ignored test
or stream was run, and the installed service was untouched. The GPU still
needs a reboot; the workspace test run encountered an additional non-ignored
GPU-discovery test, documented under verification below.
The target is 1968x2184 HDR 120 fps in HEVC/AV1, plus 1080p60 and 1440p120.
Performance numbers in this section are earlier measurements, not new results.

The build takes its AMF **1.5.2.0** headers from
`C:\Users\ramaz\git\butterpollo-pw-build\build\_deps\ffmpeg-v2026.516.30821\ffmpeg\include\AMF`.
The three encoder headers and `ColorSpace.h` match upstream tag `v1.5.2`
(`eae4a4b7efc35f8b0a3977a0984c0d642efc4e63`) after normalizing line endings.
The tables below use those exact contracts:
[H.264](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/v1.5.2/amf/public/include/components/VideoEncoderVCE.h),
[HEVC](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/v1.5.2/amf/public/include/components/VideoEncoderHEVC.h),
[AV1](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/v1.5.2/amf/public/include/components/VideoEncoderAV1.h),
and [colour definitions](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/v1.5.2/amf/public/include/components/ColorSpace.h).
SDK availability does not prove that this GPU/driver implements a property.

### Findings and scope of the fixes

- **Fixed: nonexistent HEVC B-frame property.** The generic setter prefixed
  `BPicturesPattern`, sending `HevcBPicturesPattern=0`. The HEVC header has
  neither that property nor a B-picture enum. Policy now requests zero B-frames
  only through `BPicturesPattern` and `Av1BPicturesPattern`. The latter really
  exists in this SDK, but is labelled a VCN5 feature, so it can be unsupported
  and redundant on this RDNA3 host. Both remain optional; failures now use the
  policy's normal warning/readback path. Commit `e8af3a99`, with a CPU unit test
  covering all three codecs. No supported HEVC encoding setting changed.
- **Fixed: misleading AV1 frame-skip diagnostics.** `log_effective` queried
  `Av1RateControlSkipFrame`, while the SDK string is
  `Av1RateControlSkipFrameEnable`. The old `?` could not establish lack of driver
  support. Commit `7f0b4b86` corrects the query and tests all three log names
  without constructing an encoder. AV1 frame-skip policy remains untouched;
  its actual support/value must be read after reboot.
- **No wrong usage, preset, rate-control, colour or intra-refresh enum found.**
  In particular AV1 swaps low/ultra-low usage numbers relative to AVC/HEVC,
  and AVC swaps CBR/latency-VBR numbers relative to HEVC/AV1. The code handles
  both. No proven HDR metadata unit bug was found; preserve the AV1 workaround
  described below.
- **Main latency/quality risks are optional tuning.** PA/lookahead, pre-encode,
  slower usage/presets, extra slices/tiles and large recovery frames can cost
  time or bits. CBR, HRD and a smaller VBV/AU cap trade quality against bursts.
  The current defaults and the existing measured H.264 VBAQ exception stay.
- **Known gaps remain visible in this review.** The full-range setters only
  warn on failure; matrix signalling is inferred from the colour profile;
  HDR metadata write success is not a bitstream inspection. The console's
  input-queue help still incorrectly claims zero forces one for VRR. Its SAV
  help mentions only integrated/discrete sharing, although AMD also describes
  multiple VCNs within one GPU. These are recorded, without expanding this
  patch into console or driver-policy changes.

### AMD guidance and the measured baseline

AMD distinguishes interactive game streaming from broadcasting gameplay.
Its game-streaming recommendations include no B-frames, an infinite GOP,
intra refresh, AQ, CBR and a 0.1-second VBV. At 120 fps that VBV is twelve
frame budgets. Butterpollo leaves VBV driver-derived by default; its explicit
buffer settings request 0.5-2 frame budgets. It agrees on no reordering and
on-demand keyframes, but enables intra refresh only when negotiated and uses
latency-constrained VBR. Those are deliberate streaming tradeoffs.
[AMD recommended settings](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/Recommended-FFmpeg-Encoder-Settings).

The usage preset is applied first, then the explicit settings override it.
Consequently `amd_usage=high_quality` alone still gets the host's speed
preset, latency VBR and PA-off policy unless those are also changed. AMD
identifies ultra-low latency and speed as appropriate to interactive use;
PA/TAQ can override VBAQ, and adaptive mini-GOP can override a B-picture
pattern. Changing usage therefore requires checking effective settings and
the resulting bitstream, not just the dropdown label.
[AMD tuning and priorities](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/AMF-Encoder-Settings-and-Tuning-in-FFmpeg).

Earlier evidence in **AMF settings per codec: quality at a given bitrate**
below: speed to quality cost about 0.7 ms HEVC, 6.1 ms AV1 and 1.9 ms H.264
mean encode time in the SDR sweep, without consistent VMAF gain; low-latency
usage cost 6-8 ms. CBR spent more of the target budget and enlarged p99 frames
by 30-67% in the SDR clips and 10-42% in HDR. Turning H.264 VBAQ off improved
VMAF by up to 9.42 at 20 Mbps and is already the default. The later **48 HDR
encodes** found no consistent reason to change HEVC/AV1 AQ, balanced preset
or CBR; AQ-off ranged -0.30 to +0.90 VMAF for HEVC and -0.03 to +0.31 for AV1.
Those HDR clips were SDR sources converted to PQ, not native HDR game scenes.

Two VCNs do not establish two-engine acceleration of this session. The
[October 7 split-frame sweep](PERFORMANCE.md#october-7-amf-split-frame-encoding)
found no consistent gain from on/off/untouched across 720 runs, including
sizes up to 7680x2160; readback was already on. Treat a split as unproven until
timing or engine activity shows it. AMD's SAV description covers both
multi-VCN GPUs and multi-device systems; SAV and the per-codec split-frame
property are separate controls.
[AMD SAV primer](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/Smart-Access-Video-Primer).

### Complete encoder-property inventory

Sources: `core/src/encoder_policy.rs::{amf,amf_rate_control,amf_split_frame}`,
`windows/src/amf.rs::{create,configure_ltr,prepare_surface,set_bitrate,write_hdr_metadata}`
and `web/src/lib/schema/video.ts`. Names below are literal AMF strings, not
the SDK macro identifiers. Unless noted, properties are set before `Init`.
`auto` omits a write; **unset** below means policy does not write it, even if
`AMF encoder settings` reads it. Policy writes are read back: required failures
abort creation, optional failures warn. Direct `property` calls generally
check only SetProperty success, with the exceptions specified below.

| Setting | H.264 | HEVC | AV1 | Value, SDK comparison and verdict |
|---|---|---|---|---|
| Usage | `Usage` | `HevcUsage` | `Av1Usage` | Default ultra-low = **1 / 1 / 2**. Transcoding 0; low-latency **2 / 2 / 1**; webcam 3; high-quality 4; low-latency-high-quality 5. Correct, required unless auto. Other usages may buffer frames. |
| Quality | `QualityPreset` | `HevcQualityPreset` | `Av1QualityPreset` | Speed **1 / 10 / 100**; balanced **0 / 5 / 70**; quality **2 / 0 / 30**. Correct; default speed, required unless auto. SDK also has high-quality presets, which the console does not offer; no new option proposed. |
| Rate control | `RateControlMethod` | `HevcRateControlMethod` | `Av1RateControlMethod` | Default latency VBR **3 / 1 / 1**; CBR **1 / 3 / 3**; CQP 0; peak VBR 2; QVBR 4; HQVBR 5; HQCBR 6. Correct. Explicit choice is required; default rejection warns. HDR demotes modes 4-6 to peak VBR. CQP ignores the stream bitrate budget. |
| QVBR target | `QvbrQualityLevel` | `HevcQvbrQualityLevel` | `Av1QvbrQualityLevel` | Only QVBR: explicit 1-51, zero leaves default (SDK 23). This is not AV1's 1-255 QIndex. Names/range correct. The headers do not substantiate the console's low/high quality endpoint labels. |
| Adaptive quantization | `EnableVBAQ` | `HevcEnableVBAQ` | `Av1AQMode` | Default false / true / **1 (CAQ)**; AV1 0 = none. Disabled for explicit CQP; auto omitted. Correct codec distinction, though the common console label says VBAQ for AV1 too. AQ may redistribute distortion rather than improve PSNR. |
| Pre-analysis | `EnablePreAnalysis` | `HevcEnablePreAnalysis` | `Av1EnablePreAnalysis` | False by default and for HDR; true on SDR when requested or RC >=4. Required when true. Correct names. The AVC/HEVC header comment saying peak VBR only is stale relative to AMD's documented PA-dependent quality RC modes. |
| PA lookahead | `PALookAheadBufferDepth` | `PALookAheadBufferDepth` | `PALookAheadBufferDepth` | Shared name, **1** only when PA is enabled, required. Adds analysis/buffering risk. No codec prefix belongs here. |
| HRD | `EnforceHRD` | `HevcEnforceHRD` | `Av1EnforceHRD` | False by default, configurable bool; explicit setting required. Correct. Off permits looser buffering; on may raise QP. It is not a network packet pacer. |
| RC frame skipping | `RateControlSkipFrameEnable` | `HevcRateControlSkipFrameEnable` | `Av1RateControlSkipFrameEnable` | Writes false optionally for AVC/HEVC. AV1 **unset**, now logged with the correct SDK name. Confirm actual value/support; do not infer it from the old log. |
| Input queue | `InputQueueSize` | `HevcInputQueueSize` | `Av1InputQueueSize` | `amd_input_queue_size=0` writes nothing for **all** clients; positive requests are clamped to 1-32 and required. SDK default 16 is capacity, not a mandatory 16-frame delay. The VRR claim in console help is stale. |
| Query timeout | `QueryTimeout` | `HevcQueryTimeout` | `Av1QueryTimeout` | **1 ms**, optional direct write; SDK default 0 = no wait. Correct units. QueryOutput may return earlier on output; this does not deliberately delay every frame by 1 ms. A pending query can still occupy the capture/encode thread until timeout. |
| Internal latency | `LowLatencyInternal` | `LowLatencyInternal` | — | Shared **unprefixed** bool for AVC/HEVC, explicit only, required. Correct exception to prefixing. AVC also selects POC mode 2. Under ultra-low usage the driver commonly already enables it; forcing on can be redundant. |
| AV1 latency | — | — | `Av1EncodingLatencyMode` | Auto omitted; none 0, power-saving 1, realtime 2, lowest 3. Correct. SDK targets respectively no deadline, 1/fps, 1/(2*fps), fastest possible; these are effort/power modes, not guaranteed timing. |
| SmartAccess Video | `EnableEncoderSmartAccessVideo` | `HevcEnableEncoderSmartAccessVideo` | `Av1EnableEncoderSmartAccessVideo` | Auto omitted; explicit bool, required only when enabling. Guard forces SAV off (required) when AVC/HEVC **explicitly** requests internal low latency. It does not inspect the usage-derived effective latency value; keep this driver-interaction question separate from a proven reset cause. |
| Split frame | — | `HevcMultiHwInstanceEncode` | `Av1MultiHwInstanceEncode` | Optional bool after other policy: only if `HevcNumOfHwInstances` / `Av1CapNumOfHwInstances` >1; auto asks only if not already on. Correct names; accepted hint is not proof of a split. On this host normally redundant. |
| High motion | `HighMotionQualityBoostEnable` | `HevcHighMotionQualityBoostEnable` | `Av1HighMotionQualityBoost` | Explicit bool only, optional. Correct AV1 exception. Existing sweep cost 0.2-1.1 ms without consistent quality gain. |
| Profile | `Profile` | `HevcProfile` | — | AVC **100 (High)** always; HEVC **2 (Main10)** for ten-bit, otherwise driver Main. AV1 profile left to driver (Main =1). Correct; AVC ten-bit/HDR and all AMF 4:4:4 are rejected. No level/tier override is made. |
| AVC entropy coding | `CABACEnable` | — | — | Auto omitted; CABAC 1, CAVLC 2. Correct enum despite the Enable name; do not replace with a bool. High profile's automatic coding is CABAC in the SDK. |
| Screen-content tools | — | — | `Av1ScreenContentTools` | Explicit bool only, required. SDK defaults true and gates palette/integer-MV tools; turning this on alone need not change anything. `Av1PaletteMode` and `Av1ForceIntegerMv` are not written. |
| Slices/tiles | `SlicesPerFrame` | `HevcSlicesPerFrame` | `Av1NumTilesPerFrame` | AVC/HEVC write negotiated count only if >1. AV1 explicit 1/2/4, or negotiated slices capped at 4 if >1; zero/one automatic leaves driver alone. Correct: tiles are a suggestion, and readback accepts any positive result. Extra partitions may cost coding efficiency; no slice/tile output mode is enabled. |
| References | `MaxNumRefFrames` | `HevcMaxNumRefFrames` | `Av1MaxNumRefFrames` | Negotiated positive count is required; zero leaves driver default. AVC intra refresh requires at least two; a one-reference client falls back to IDR. Correct budget handling; more slots are not evidence that the encoder actually uses extra predictors. |
| B-frame pattern | `BPicturesPattern` | **No property** | `Av1BPicturesPattern` | Optional zero for AVC/AV1 only after this fix. AV1 property is VCN5-only per header and may be absent here; HEVC had an invalid prefixed write. No B-reference, maximum-B, or adaptive-mini-GOP setting is written. |
| Size | `FrameSize` | `HevcFrameSize` | `Av1FrameSize` | Negotiated width/height as AMFSize, required. Correct. |
| Rate | `FrameRate` | `HevcFrameRate` | `Av1FrameRate` | `fps_millihz()/1000` as AMFRate, required; preserves fractional refresh. Correct. |
| Target | `TargetBitrate` | `HevcTargetBitrate` | `Av1TargetBitrate` | Requested kbps *1000 bits/s, required initially; updated dynamically. Correct units. Failed live update warns and retains driver state. |
| Peak | `PeakBitrate` | `HevcPeakBitrate` | `Av1PeakBitrate` | Default unset; configured ratio 1-2 * target bits/s, required if requested. Not an individual-frame limit; its relevance depends on RC mode. |
| VBV | `VBVBufferSize` | `HevcVBVBufferSize` | `Av1VBVBufferSize` | Default unset; configured 0.5-2 * bits/frame, required if requested. Written after frame rate and target; **bits**, not bytes or milliseconds. Correct. Small buffers can hurt detail/scene changes. |
| Maximum AU/frame | `MaxAUSize` | `HevcMaxAUSize` | `Av1MaxCompressedFrameSize` | Default unset; configured 1-8 * bits/frame, required if requested. Correct names and bit units. Zero config means omit, not actively clear. HEVC header's default-60 comment disagrees with API guide's zero; read driver state instead of assuming either. |
| GOP/IDR | `IDRPeriod` | `HevcGOPSize` | `Av1GOPSize` | **0**, AVC failure ignored, HEVC/AV1 required. No scheduled recovery IDRs; request on surfaces. `HevcGOPSPerIDR`, AVC `IntraPeriod`, `Av1IntraPeriod` and insertion cadence stay unwritten. Finite-GOP stalls are a retained host workaround, not an AMD guarantee for every driver. |
| Intra refresh | `IntraRefreshMBsNumberPerSlot` | `HevcIntraRefreshCTBsNumberPerSlot` | `Av1IntraRefreshMode`, `Av1IntraRefreshNumOfStripes` | Only when negotiated: AVC 16x16 MBs, HEVC 64x64 CTBs; N=ceil(w/block)*ceil(h/block), request ceil(N/clamp(N,1,299)) per slot. AV1 **continuous=2**, **300 stripes**. Correct enums/block units; approximately 300 pictures means ~2.5 s at 120 fps or 5 s at 60, not immediate full recovery. Small pictures/granularity change the AVC/HEVC duration. |
| AV1 alignment | — | — | `Av1AlignmentMode` | **3 (no restrictions)** on public constructors, required. Correct enum, but this driver still pads 1968x2184 to 1984x2186 without render-size signalling; already measured below. Changing alignment is not a proven fix. |

LTR uses additional component and surface properties; all names and the
reset-unused enum agree with the SDK. `amd_ltr_frames=0` leaves them alone.
A positive request (maximum four) is limited to negotiated references minus
one rolling reference, to property limits and, for AV1, `Av1CapMaxNumLTRFrames`.
Intra refresh disables LTR. Failed setup restores the maximum to zero and
falls back to IDRs. This is loss recovery, not a free steady-state quality gain.

| Purpose | H.264 | HEVC | AV1 | Value |
|---|---|---|---|---|
| LTR capacity | `MaxOfLTRFrames` | `HevcMaxOfLTRFrames` | `Av1MaxNumLTRFrames` | Limited positive count before Init, read back. |
| LTR lifetime | `LTRMode` | `HevcLTRMode` | `Av1LTRMode` | **0 = reset unused**; `core/src/ltr.rs` drops other anchors when recovering, matching the driver rule. |
| Mark surface | `MarkCurrentWithLTRIndex` | `HevcMarkCurrentWithLTRIndex` | `Av1MarkCurrentWithLTRIndex` | Slot index on keyframe/selected every-fourth frames when LTR is active. |
| Recover surface | `ForceLTRReferenceBitfield` | `HevcForceLTRReferenceBitfield` | `Av1ForceLTRReferenceBitfield` | **1 << slot**, not the slot number. Correct. Surface failure disables LTR and requests IDR. |
| Force recovery frame | `ForcePictureType` | `HevcForcePictureType` | `Av1ForceFrameType` | **2 = IDR / 2 = IDR / 1 = KEY** on requested keyframes. Correct. |
| Repeat headers | `InsertSPS`, `InsertPPS` | `HevcInsertHeader` | `Av1ForceInsertSequenceHeader` | True on those keyframes so a reset decoder can start independently. Correct. |

Not written: pre-encode (`RateControlPreanalysisEnable`,
`HevcRateControlPreAnalysisEnable`, `Av1RateControlPreEncode`), filler
(`FillerDataEnable`, `HevcFillerDataEnable`, `Av1FillerData`), and initial
fullness (`InitialVBVBufferFullness`, `HevcInitialVBVBufferFullness`,
`Av1InitialVBVBufferFullness`; SDK scale 0-64). The first two groups are logged
only. PA-off does **not** explicitly switch pre-encode off. Filler can consume
wire budget without picture detail, particularly under a CBR/usage change;
confirm effective values before attributing an actual-bitrate gain to quality.

QP limits are also **unset**, not clamped by the host: AVC `MinQP`/`MaxQP`
and `QPI`/`QPP`/`QPB` (0-51); HEVC `HevcMinQP_I`, `HevcMaxQP_I`,
`HevcMinQP_P`, `HevcMaxQP_P`, `HevcQP_I`, `HevcQP_P` (QP scale 0-51);
AV1 `Av1MinQIndex_Intra`, `Av1MaxQIndex_Intra`, `Av1MinQIndex_Inter`,
`Av1MaxQIndex_Inter`, `Av1MinQIndex_Inter_B`, `Av1MaxQIndex_Inter_B`,
`Av1QIndex_Intra`, `Av1QIndex_Inter`, `Av1QIndex_Inter_B` (SDK 1-255).
Do not reuse an AVC QP number as an AV1 QIndex. These and colour/GOP details
are not all in the settings log; an omitted field is not evidence of a default.

Live bitrate changes also scale positive read-back peak/VBV/AU values by the
new/old bitrate ratio, including driver-derived values, clamped to property
limits. Partial update failure leaves mixed driver state and a warning.
Do rate-control comparisons on fresh sessions at a fixed bitrate.

The session admits two pending pictures; AMF's native ownership limit is
eight. Neither equals the driver's input-queue capacity. `poll` queries only
with work in flight and returns one result promptly. AMD recommends separate
submission/output threads for overlap; this host uses a combined capture/encode
loop with separate sending. A 1 ms query can therefore affect claim timing,
but a threading rewrite is outside this review.
[AMD asynchronous application guide](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/Guide-for-Video-CODEC-Encoder-App-Developers#53-amf-asynchronous-and-synchronous-model).

PA remains a low-priority experiment. AMD documents NV12 analysis and warns
that PA adaptive quantization can supersede VBAQ. The code gates PA and quality
RC on `hdr`, not `ten_bit()`, so optional **ten-bit SDR** can still request PA
with P010; this needs a format-support check before using that combination.
Auto-LTR/TAQ/adaptive mini-GOP are not explicitly controlled by this host.
Keep PA off for the HDR baseline; no broader policy change is justified here.
[AMD PA guide](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_PreAnalysis_API.md).
The [HEVC API guide's rate-control section](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_Encode_HEVC_API.md)
explicitly requires PA for QVBR/HQVBR/HQCBR, explaining the discrepancy with
the older peak-VBR-only header comment; the same guide specifies MaxAUSize
in bits with default zero.

### HDR and colour from capture to Moonlight

`Negotiated::color_matrix()` forces BT.2020 for HDR; `ten_bit()` selects P010
and `full_range()` takes bit zero of negotiated `csc_mode`. The D3D11 and
D3D12 paths use the same `gpu_color::constants` and `shaders/color.hlsl`:
scRGB FP16 (Rec.709 primaries, 1.0 = 80 nits) is converted to Rec.2020 and
absolute ST.2084 PQ before 4:2:0 subsampling. An already-PQ `Rgba10Pq` source
is not PQ-encoded twice. Limited P010 uses Y 64-940 and nominal chroma
64-960; full range uses 0-1023, with neutral chroma 512, stored in the high
ten bits. CPU fallback in `encoder.rs::Convert`/`color.rs` uses the matching
matrix/range and PQ conversion. Input AMF colour properties describe these
**already-converted NV12/P010 surfaces**, not the original scRGB texture.

| Property purpose | H.264 | HEVC | AV1 | Write and assessment |
|---|---|---|---|---|
| Bit depth | No ten-bit write; rejected | `HevcColorBitDepth` | `Av1ColorBitDepth` | 10 for HDR or ten-bit SDR; otherwise SDK default 8. Correct; HEVC also sets Main10. |
| Input profile | `InColorProfile` | `HevcInColorProfile` | `Av1InputColorProfile` | AMF profiles limited 601/709/2020 = **0/1/2**, full = **3/7/8**. Correct. Optional input description is largely redundant with our own conversion. |
| Output profile | `OutColorProfile` | `HevcOutColorProfile` | `Av1OutputColorProfile` | Same values; HDR uses 2 or 8. Correct names, including AV1's Input/Output rather than In/Out. Failed output writes abort HDR creation; SDR warns. |
| Input transfer | `InColorTransferChar` | `HevcInColorTransferChar` | `Av1InputColorTransferChar` | HDR **16 (PQ)**; SDR 601/709/2020 = **6/1/14**. Input failure only debug-logged. |
| Output transfer | `OutColorTransferChar` | `HevcOutColorTransferChar` | `Av1OutputColorTransferChar` | Same values, correct per `ColorSpace.h`; output failure handled as above. |
| Input primaries | `InColorPrimaries` | `HevcInColorPrimaries` | `Av1InputColorPrimaries` | 601/709/2020 = **6/1/9**; HDR always 9. Correct; input optional. |
| Output primaries | `OutColorPrimaries` | `HevcOutColorPrimaries` | `Av1OutputColorPrimaries` | Same values; HDR BT.2020. Correct, required for HDR. |
| Input range | `InputFullRangeColor` | `HevcInputFullRangeColor` | `Av1InputFullRangeColor` | Negotiated bool. Correct names/type; failures tolerated because some drivers reject the input hint. |
| Output range | `FullRangeColor` | `HevcNominalRange` | `Av1NominalRange` | Same bool. These literal names remain correct even though old macro aliases are deprecated. HEVC explicitly permits bool (legacy integer 0/1 also works); no enum/type fix needed. **Failure is only a warning for full range**, so verify actual bitstream range. |
| Matrix coefficient | `InMatrixCoeff`, `OutMatrixCoeff` | `HevcInMatrixCoeff`, `HevcOutMatrixCoeff` | `Av1InMatrixCoeff`, `Av1OutMatrixCoeff` | **Unset**. The driver derives matrix signalling from profile; expected 601/709/2020-NCL = **6/1/9**, distinct from AMF profile numbers. The existing hardware test asserts BT.2020-NCL; no unconditional new property required without driver testing. |
| Static HDR payload | None sent | `HevcInHDRMetadata` | `Av1InHDRMetadata` | AMFBuffer interface containing AMFHDRMetadata, after Init and before first input; updated when changed. AV1 still uses **InHDRMetadata**, not InputHDRMetadata. HEVC's OutHDRMetadata is commented out in the SDK. Correct names. |

`capture.rs::Device::hdr_metadata()` reads the selected output's DXGI
maximum/minimum/full-frame luminance, with bounded finite conversion in
`core/src/hdr.rs`. Primaries are synthesized as Rec.2020 and D65, not copied
from DXGI panel primaries or a game's mastering metadata. Fallback is 1000
nits peak, 0.0001 nit minimum, unknown full-frame luminance. The live path
leaves **MaxCLL and MaxFALL at zero (unknown)**; the quality probe and tests
can supply nonzero values. Display full-frame luminance is not content MaxFALL
and is correctly kept separate. This is valid synthetic desktop metadata,
not preservation of original HDR10/HDR10+ game metadata.

The same `Metadata` value is stored in the session and sent to the encoder
before the first frame (`stream.rs` startup). It is refreshed once per second;
encoder recreation sets `metadata_due` to now, so the first recovery frame
also receives metadata. `set_hdr_metadata` skips SDR/AVC and duplicate values.
On failure it warns and caches the attempted value rather than retrying it
every frame; control metadata can therefore be correct while bitstream
metadata is absent. Even a successful SetProperty/log line does not prove
that every subsequent keyframe carries the metadata. The AV1 header labels
the input HDR property static, while this implementation uses it after Init;
retain the known driver path and check an independent keyframe after updates
on any new runtime before claiming portable dynamic support.

Metadata units are intentionally codec-specific:

| Field | Internal/control value | HEVC AMFHDRMetadata / SEI | AV1 AMFHDRMetadata / metadata OBU |
|---|---|---|---|
| RGB primaries, D65 white | Integer x/y in 1/50000 | Unchanged | floor(value *65536/50000), capped at 65535: 0.16 fixed point |
| Mastering peak | Whole nits (`u16`) | nits *10000 (`u32`) | nits *256 (`u32`): 24.8 fixed point |
| Mastering minimum | 1/10000 nit (`u16`) | Unchanged in 1/10000 nit | floor(value *16384/10000): 18.14 fixed point |
| MaxCLL / MaxFALL | Whole nits, zero unknown | Unchanged | Unchanged |
| Display full-frame peak | Whole nits, zero unknown | No AMFHDRMetadata member; control only | Control only |

The shared AMF header describes HEVC units for AMFHDRMetadata. The retained
AV1 implementation documents that this driver copies those fields straight
into the OBU, so the host pre-scales them to the
[AV1 mastering-display syntax](https://github.com/AOMediaCodec/av1-spec/blob/master/07.bitstream.semantics.md#metadata-high-dynamic-range-mastering-display-color-volume-semantics).
For 1015 nits, minimum 0.005 nit and red x=0.708, the existing ignored test
expects HEVC **10150000/10000**, **50/10000**, **35400/50000**; AV1
**259840/256**, **81/16384**, **46399/65536**. Using HEVC's peak scale on AV1
would signal 39,062.5 nits for a 1000-nit source. The small AV1 truncation is
quantization, not a factor-of-10,000 bug. Hardware tests were not rerun here.

HEVC carries mastering-display and, when known, content-light metadata in
prefix SEI. AV1 carries HDR_MDCV (type 2) and HDR_CLL (type 1) metadata OBUs.
`amf.rs::poll` copies these encoder bytes unchanged for HEVC/AV1; the host
passes them to `VideoPacketizer::encode_recovery`, which frames/FECs/encrypts
the payload without filtering SEI/OBUs. H.264 is SDR-only in this backend,
with colour signalling in its SPS VUI and no HDR buffer.

The retained Moonlight fixture (`performance-probe/moonlight-vrr-common`,
commit `d6a11bc685b41037b352a96f29d08276fe5359ba`) explains the apparent gap
at the receiver: `VideoDepacketizer.c` strips H.264/HEVC leading AUD/prefix-SEI
NALs, including HEVC NAL type 39, before delivering decode units. It does not
parse the AV1 bitstream that way. Thus a Moonlight HEVC decode-unit dump may
lose the metadata that was present on the wire; a dump is not sufficient to
accuse the host of omitting SEI. The client's renderer/decoder can use the
separate control metadata; actual tone mapping is client-specific.

`stream.rs` sends encrypted reliable ENet control type **0x010e** once an
output is selected, and again whenever the 27-byte metadata payload changes.
`Metadata::wire` and Moonlight `ControlStream.c` agree exactly:

| Payload offsets | Field | Representation |
|---|---|---|
| 0 | HDR enabled | 0 or 1 from negotiated `config.hdr`, including 0 for SDR |
| 1-12 | R, G, B x/y | Six little-endian u16, normalized to 50000 |
| 13-16 | White x/y | Two little-endian u16, normalized to 50000 |
| 17-18 | Mastering peak | Little-endian u16, whole nits |
| 19-20 | Mastering minimum | Little-endian u16, 1/10000 nit |
| 21-24 | MaxCLL, MaxFALL | Two little-endian u16, whole nits |
| 25-26 | Display full-frame peak | Little-endian u16, whole nits |

Moonlight saves those fields, updates HDR state and invokes `setHdrMode`;
`LiGetHdrMetadata` exposes the copy. Decode units get `hdrActive` and Rec.2020
from that state (SDR uses negotiated colour space); the payload does not
separately carry transfer/matrix/full-range flags. Bitstream signalling and
negotiation still matter. `tests/moonlight_client.c` checks HDR notifications,
metadata validity and decoded ten-bit/PQ/BT.2020 frames. The existing ignored
`hdr10_metadata_and_range_reach_the_bitstream` checks encoder output directly,
both codecs, both ranges, D3D11/D3D12 and independently decodable keyframes
0 and 4, including nonzero/unknown CLL. It is the post-reboot validation to
use, not a test to execute while the GPU is down.

### Ranked host A/B plan, after reboot

Keep ultra-low usage, speed, latency VBR, PA off, the existing per-codec AQ
default, automatic queue/latency/SAV/split-frame and negotiated references as
the control. Use separate fresh sessions; change one setting at a time.
These rankings are expected opportunities, not claims of measured gains:

| Rank | Concrete A/B values | Expected benefit and required observations |
|---|---|---|
| 1 | `amd_max_frame_size=0,4,2` (1 only if the others help) | Best candidate for recovery/scene-change **picture-age tails on a constrained link**. In amf_quality compare IDR bytes, p99 P-frame bytes, actual bitrate and detail loss; in streaming request recovery and measure sender/FEC time, picture-age p95/p99, decode errors and held pictures. A cap may reduce quality or be ignored; readback alone is insufficient. |
| 2 | `amd_ltr_frames=0,1,2`, only with a client reference budget allowing it, intra refresh off | Potentially large loss-recovery benefit, little expected idle gain. Probe steady-state quality/bytes/encode cost first; then controlled packet loss and repeat recovery on a separate client. Measure recovery-frame bytes, time to clean picture and picture-age p99. One-reference clients cannot test LTR; amf_quality does not simulate loss. |
| 3 | HEVC/AV1 `amd_vbaq=enabled,disabled`; H.264 retain disabled as control | Cheap quality opportunity on **native HDR** gradients, particles, foliage and UI. Earlier synthetic HDR changes were small/mixed, max +0.90/+0.31 VMAF for HEVC/AV1. Compare PSNR/SSIM-style plane metrics, visual temporal stability and actual bytes, with no regression in encode p99 or picture age. Do not assume AV1 CAQ is identical to AVC VBAQ. |
| 4 | First `amd_rc=vbr_latency,vbr_peak,cbr`; then, separately with the selected RC, `amd_peak_bitrate_ratio=0,1,1.5`, `amd_vbv_buffer_frames=0,2,1`, `amd_enforce_hrd=false,true` | Better use of bitrate or smaller bursts, with a quality tradeoff. Existing CBR gains mostly came from spending more bits and produced larger frames. Measure actual bitrate, scene-cut/IDR sizes, PSNR/SSIM/VMAF, encode p99 and on-wire picture-age tails; compare both equal requested and equal actual bitrate. Do not switch all four knobs together. |
| 5 | AV1 `amd_av1_tiles=0,1,2,4`; separately `amd_av1_screen_content=auto,disabled,enabled` | Possible coding-efficiency or client decode improvement, especially if automatic tiles exceed one. Inspect effective tile count and screen-tool use; quality-probe bits/text edges and separate-client decode time/picture age. Extra tiles do not make this full-frame sender transmit early and did not unlock split-frame gain in earlier sweeps. AVC/HEVC slice 1/2/4 requires a negotiated test-client change; no host config knob exists. |
| 6 | `amd_input_queue_size=0,4,2`, then 1 only in an isolated run; separate source-only `QueryTimeout=1` vs 0 | Possible submission/claim-tail gain under game load; near-zero expected idle gain when backlog is already small. Measure INPUT_FULL/NEED_MORE_INPUT counts, claim wait, output gaps, CPU time, encode p99 and picture age. Stop at a stall. `amf_quality` submits one picture at a time and cannot establish a queue-depth win. QueryTimeout has no console setting; an experimental build is needed, not a new shipped flag. |
| 7 | `amd_quality=speed,balanced,quality`, starting at 1080p60/low bitrate; optionally `amd_high_motion_quality_boost=auto,enabled,disabled` in separate cells | Low expected return at the owner's normal rates; quality already cost up to 6.1 ms in AV1. Only keep a slower choice if new native content shows a visible gain and encode/picture-age tails still meet 16.67 ms at 60 or 8.33 ms at 120 fps. This is a frame budget, not a guarantee that the full pipeline fits in one frame. |
| 8 | AVC/HEVC `amd_lowlatency_mode=auto,enabled`; AV1 `amd_av1_latency_mode=auto,lowest,realtime`; split `auto,disabled,enabled` | Mostly redundancy checks: effective internal latency/AV1 lowest and split were already selected by the default usage/driver. Read settings first and skip pairs with identical effective state. Compare encode/picture-age p99 and power. Revisit split primarily after a driver change; two reported VCNs alone predict no gain. |

SAV and PA/HQ are last, separate investigations, not recommended changes to
the HDR baseline. Prior SAV/split sweeps were neutral. Do not combine explicit
SAV-on with forced internal low latency; the existing guard documents past
HEVC HDR resets. An 8-bit SDR-only PA comparison would use
`amd_rc=vbr_peak` with `amd_preanalysis=false,true`, then QVBR at 18/23/28 or
HQVBR separately. Inspect PA's effective tools and reference/reorder behavior.
The current one-picture-at-a-time quality probe may time out if a mode needs
future input; such a timeout does not measure quality or throughput. A
pipelined experiment would be separate work. HDR requests for these quality
RC modes currently become peak VBR, so they are not genuine HDR QVBR A/Bs.

For every feasible cell, use 1968x2184/120 at **80 Mbps** (also 30 for a
quality-stressed cell), 2560x1440/120 at **50 Mbps** (also 20), and
1920x1080/60 at **20 Mbps** (also 10). HEVC/AV1 get HDR and SDR; AVC gets SDR.
At the normal rates one-frame budgets are respectively **666667, 416667 and
333333 bits**, not bytes. E.g. native HDR VBV=1 requests about 83.3 kB and
max-frame=2 requests about 166.7 kB before network/FEC overhead.

Use `amf_quality`'s existing `--codec`, `--width`, `--height`, `--fps`,
`--bitrate` (kbps), `--frames`, `--config`, `--out` and HDR `--hdr 1` options
only after reboot. It writes the bitstream and reports non-IDR encode
mean/p95/p99, frame bytes mean/p99 and separate IDR byte sizes; **it does not
compute PSNR, SSIM or VMAF itself**. Score a matched decode against the exact
converted reference with the existing quality harness/FFmpeg. Preserve
bit depth, matrix, range, chroma siting and native-size cropping in both
inputs. Its SDR path requests CSC 0 (601 limited), while HDR uses CSC 4
(2020 limited); references must match. HDR input is planar linear gbrpf32le
scaled to scRGB at 203-nit SDR white, so SDR clips merely converted to HDR
do not test native highlights or wide-gamut content. Include native HDR
material with known source/reference handling; default VMAF alone is not an
HDR appearance metric. Score several clips and scene transitions, not only
the original two upscaled 180-frame samples.

Then alternate A/B/B/A streaming runs, at least three per cell, with the
same display/stream refresh, bitrate/FEC, client and game load. Record
rendered-picture-to-decoded-picture age **mean/p95/p99**, new pictures/s,
host encode/claim/send timing, IDR/recovery spikes, decoder time and errors.
This fixture age excludes client scanout and input latency. Use a separate
hardware-decoding client under game load: loopback decoder starvation was
already measured here and can dwarf encoder differences. For native AV1,
check the known 1984x2186 padding/crop behavior before comparing scores.
Archive effective settings after Init and independently inspect HDR headers
and keyframes; do not call a lower requested rate or absent metadata a
quality/latency improvement.

### Verification of this review's changes

All cargo commands used `C:\src\cargo-one.ps1`, the supplied `rust-env.ps1`
and `CARGO_TARGET_DIR=D:\bp-build\amfreview-target`.

- `fmt --all -- --check`: passed.
- `clippy --workspace --all-targets --locked -j 2 -- -D warnings`: passed.
- The focused core policy test passed. Both new non-hardware regression
  tests also passed in the workspace run.
- `test --workspace --locked -j 2 -- --skip heartbeat_check_finds_the_monitors`:
  failed only in the existing `encoder::tests::encoders_open_on_the_configured_gpu`.
  It expected an error naming the nonexistent configured adapter but instead
  got `no desktop display or hardware GPU`. It stopped during adapter discovery,
  before opening an encoder. The Windows suite reported 179 passed, one failed,
  41 ignored and one filtered; preceding suites passed. The unrelated test is
  unchanged.
- `test --workspace --locked -j 2 -- --skip heartbeat_check_finds_the_monitors
  --skip encoders_open_on_the_configured_gpu`: passed (550 tests, 44 ignored,
  two filtered out; doc-tests also passed). Both new regression tests passed.

Tuning proposals, encoder/HDR bitstream tests and hardware validation remain
deferred until the GPU is healthy. The requested single-skip command needs
a post-reboot rerun before it can be called green.

## October 9 job 7 split and stall diagnostics: host A/B, then a GPU reset

Same setup as the job 4 A/B below (base `92afcce5`, 1968x2184 HDR 120,
240 Hz virtual display, hardware-decoding receiver), October 9 02:32-02:49.
Picture age mean / p95 / p99 ms:

| Build | HEVC rounds | AV1 rounds |
| --- | --- | --- |
| base | 12.40 / 13.12 / 13.39, 12.41 / 13.16 / 13.42, 13.82 / 17.84 / 19.03, 12.42 / 13.24 / 13.44, 12.27 / 12.92 / 13.36 | 11.90 / 12.52 / 12.80, 12.08 / 12.74 / 13.36 |
| job 7 (display/input split) | 12.31 / 12.92 / 13.33, 12.37 / 13.16 / 13.39 | 11.90 / 12.53 / 13.00, 12.02 / 12.66 / 13.15 |
| stall diagnostics | 13.46 / 17.72 / 18.36, 12.38 / 13.14 / 13.40, 12.38 / 13.18 / 13.39, 12.36 / 13.05 / 13.34 | 13.12 / 17.48 / 17.84, 12.01 / 12.63 / 13.20 |

Both neutral: the 17-18 ms p95 tails appear in single runs of both the base
and the stall build (a phase the fixture sometimes locks into), not
systematically. Host mean was 3.6-3.7 ms (HEVC) and 3.2-3.3 ms (AV1) in every
run. Shipped: job 7 as `66547532` and `68273f43`, the stall injection, tests
and stall-time logging as `c35bcd28`.

GPU reset: 25 s into the fifth base run (02:47:11 and again 02:48:03 local)
the Radeon driver timed out (`C:\Windows\LiveKernelReports\WATCHDOG\
WATCHDOG-20261009-0247.dmp` and `-0248.dmp`; the AMD Bug Report Tool opened).
The host log shows audio late reads of up to 0.8 s at that moment, and the
next run's host could not open an encoder within 60 s. The driver did not
recover: the RX 7900 XT shows Code 31 and both virtual display adapters
Code 43, Windows runs on the basic display, and every build's `--diagnostics`
fails with 0x80070057. Over 60 identical runs earlier that night had no
reset; the previous watchdog dumps on this PC are from October 8 00:31-00:43.
Cause unknown: the base build predates every change measured here. It needs a
reboot before any GPU work; the dumps are kept for analysis.

## October 9 a new virtual display hitches other streams

Owner's observation: RX 7900 XT, Windows 11, extended layout, one client
rendering and streaming on its own virtual display. Creating another client's
display raises the first stream's render-to-decode picture age to 500-600 ms
for about 1.3 s. Removing the second display does not hitch. Windows also
reactivates a deliberately inactive physical TV during creation. This is the
reported measurement, not a measurement from this worktree.

Source audit: `59caf6a0`. **Plan only; no display code changed.** The logs do
not establish how many mode/layout applies actually happened, and the existing
code already skips satisfied settings. Combining the remaining operations
crosses identity checks, HDR settling and arrangement recovery bookkeeping;
it is not a clearly safe, contained change without Windows transition tests.
Single-stream and multi-stream behavior therefore remain unchanged.

### Call trace and scope

Entry: `host/src/display_session.rs::Prepared::create` captures the original
layout, calls `Guard::new_virtual_options`, sets virtual DPI, acquires an
arrangement lease, then calls `Guard::apply_virtual_mode("after layout")`.
`GoldenLease` records a restoration snapshot; it does not apply one at launch.
The physical `Activation::acquire` branch is skipped for an owned VDD.

All `SetDisplayConfig` calls below submit the desired **whole active path
set**, even when the code changes just one source/target. They can disturb
DWM/capture on the first display. This is a plausible source of the hitch,
not proof that every call pauses every display or that the driver hotplug
itself is innocent. A device-scoped request likewise is not a guarantee of
uninterrupted composition on other outputs.

In the table, `A` = `SDC_APPLY`, `U` =
`SDC_USE_SUPPLIED_DISPLAY_CONFIG`, `C` = `SDC_ALLOW_CHANGES`. Counts are
display-changing native call attempts, including fallbacks, per invocation
of the indicated step. Read-only queries are not included in apply totals.

| Order / source | Native operation and flags | Scope and count |
| --- | --- | --- |
| Before arrival: `Snapshot::capture`, `hotplug::Protection::capture` | `GetDisplayConfigBufferSizes` / `QueryDisplayConfig` (`QDC_ONLY_ACTIVE_PATHS` or `QDC_ALL_PATHS`); device-name, color, DPI and current-mode queries | Read only; **0 applies**. `ALL_PATHS` includes inactive alternative routes to active targets; these are not dormant monitors. |
| `display_lease` -> `VirtualDisplay::create_options` | Driver version query `DeviceIoControl(0x900)`; one create request (`0x901` for protocol 3.5, `0x90c` for 3.6+) carrying requested dimensions/rate | **1 driver creation**, separate from Win32 configuration counts. Windows/driver arrival and saved-topology recall can reconfigure the desktop before our first apply. Internal commit count is unknown. Shared identity reuse creates no new display. |
| `VirtualDisplay::resolve` -> `activate_target`, only if Windows leaves the new target inactive | `SetDisplayConfig(A|U|C)`, current active paths/modes plus a free-source route for the new target; retry with *all* mode indices invalid and no modes if refused | Normally **0**; **1-2 per activation attempt**. Whole topology; fallback permits choosing every display's timing. Starts after a one-second connected-but-off grace, retried about once a second within the ten-second resolve deadline. Lease renewals are not topology applies. |
| `check_hotplug("created")` -> `Protection::check` | Remove reactivated dormant targets, compact referenced modes, `SetDisplayConfig(A|U)`; on failure retry `A|U|C` | **0 if none reactivated; 1-2 if the TV reactivated**, regardless of number of pruned targets. Full retained active topology, including the first stream; strict attempt preserves its supplied modes, positions, path order and clone relationships. Fallback can retime survivors. Target identity and a second topology snapshot are checked before applying. |
| `Guard::new_virtual_options` -> `lease_hdr` -> `set_hdr` -> `color_state::set` | `DisplayConfigSetDeviceInfo(SET_HDR_STATE=16)` on supported Windows 11 API, otherwise legacy `SET_ADVANCED_COLOR_STATE` | **0-1 HDR write**, addressed to the new adapter/target. Already satisfied or unsupported enable requests are skipped. Successful writes are polled up to three seconds; a timed-out transition can issue **1 additional rollback write** and fail setup. Modern SET failure does not also invoke legacy SET. |
| `finish_hotplug("after settings")` -> `Protection::settle` | Repeated `Protection::check`, same strict/prune/fallback sequence as above | Normally **0 applies**, despite polling: 500 ms quiet period, 50 ms sleeps, 1500 ms enforcement deadline. Each additional reactivation episode adds **1-2 applies**. Sleeps delay this launch; they are not themselves DWM modesets or a demonstrated cause of the other stream's freeze. |
| End of that same `finish_hotplug` -> `VirtualDisplay::apply_mode("after settings")` -> `Topology::set_mode_rate` | If current dimensions/rate differ, edit new source width/height and path refresh, invalidate its target timing index, then `Topology::restore` -> `SetDisplayConfig(A|U|C)` | **0-1 whole-topology apply**. Exact size and refresh within 500 millihertz already count as satisfied. Unrelated paths/modes are supplied unchanged, but `C` allows Windows to adjust them. |
| Mode-list fallback inside that `set_mode_rate` | If CCD succeeded but readback still mismatches, `ChangeDisplaySettingsExW(new_display, ..., flags=0)` with `DM_PELSWIDTH|DM_PELSHEIGHT|DM_DISPLAYFREQUENCY` | **0-1 additional GDI call** to the new display, rounding requested rate to whole Hz. It is not a NULL-device desktop-wide reset, but can still trigger display-change/capture consequences. An error from CCD returns before this fallback. |
| `virtual_scale` -> `set_dpi_scale` | `DisplayConfigSetDeviceInfo(type=-4)`, adapter/source | **0-1 DPI write** for the new source; default configuration 0 and matching scale skip it. Separate from CCD topology applies. |
| `display_arrangement::Lease::acquire`, existing owner -> `apply` -> `Arrangement::compose_all` | Capture current layout, inherit the first stream's arrangement, journal desired layout; `Topology::set_active` only if active ID sets differ | For the stated extended case after successful TV pruning: **0 active-set applies**. In other layouts/state changes: usually **1-2** `SetDisplayConfig(A|U|C)` attempts, as many as **4** when a source reassignment needs an intermediate switch-off and recursive retry. The loose fallback invalidates every source/target mode. |
| Same arrangement `apply` -> `Topology::set_positions` -> `restore` | Place subsequent streams to the right, aligned with the first; `SetDisplayConfig(A|U|C)` if a source position changes | **0-1 whole-topology apply**. Already-correct positions skip it. For two extended streams the intended edit is the second display's source position; the first display is still included, and `C` still allows retiming. Primary/isolated variants may intentionally move other sources too. |
| `Guard::apply_virtual_mode("after layout")` | Same conditional CCD mode set and GDI fallback as the earlier mode pass | **0-1 CCD + 0-1 GDI**. Needed only if Windows changed/refused the requested mode; commonly just a readback after the earlier mode pass. |
| Verification / optional `hdr_profile::Lease::acquire` | Mode/color queries; optional `ColorProfileAddDisplayAssociation` for a selected differing ICC profile | Queries: **0 applies**. Profile: normally **0-1 association write** to new adapter/source, separate from HDR SET and CCD; failure can restore the prior association. |

The two INFO messages `virtual display mode applied` are emitted by readback
in `VirtualDisplay::apply_mode`, including when `set_mode_rate` took its
no-op return. They are **not evidence of two native mode changes**. The
inactive-target warning is before the strict apply (and its last deadline
check); it does not report whether the strict call succeeded or needed the
permissive fallback. The excerpt alone cannot establish a measured count.

Both mode passes target the new VDD, not the first stream's display. The
arrangement records a restoration snapshot but does not restore every
monitor's pre-arrival timing during this second launch. If arrival or an
`ALLOW_CHANGES` fallback retimes the first display, the new display's final
mode check does not repair it; verify the first display separately.

There is no `SDC_SAVE_TO_DATABASE`, `SDC_NO_OPTIMIZATION`,
`SDC_FORCE_MODE_ENUMERATION`, `SDC_TOPOLOGY_*`, `CDS_UPDATEREGISTRY`,
`CDS_NORESET` or `CDS_RESET` on this launch path. `apply_mode` with
`CDS_FULLSCREEN` exists for the separate public integer-Hz `set_mode` helper;
the virtual launch uses `set_mode_rate`, whose GDI fallback has flags 0.
Snapshot rotation/clone restoration and restoration of every saved monitor
are teardown/recovery operations, not an unconditional second-client launch
step. If arrival actually disables the first VDD, its independent heartbeat
can run activation, HDR/mode and arrangement recovery too; those extra calls
must be counted separately when recovery messages occur.

### Counts per second-display creation

Assume one automatic successful VDD activation, one TV reactivation at
`created`, no later reactivation, an extended arrangement with unchanged
active set, and no error unwind or concurrent recovery. Let `M1` and `M2`
be 0/1 for the two actual CCD mode changes, `P` be 0/1 for a position change,
and `F` be 0/1 for the TV restore's permissive retry:

`SetDisplayConfig attempts = 1 + F + M1 + P + M2`.

- Strict TV restore succeeds: **1-4 CCD applies attempted**. A mode that
  needs one correction plus a position change, with matching final readback,
  gives **3**. If Windows already selected the requested mode and placement,
  only **1** remains. These are conditional source counts, not host measurements.
- TV restore needs its fallback: **2-5 CCD attempts**, including the failed
  strict attempt. Each mode pass can additionally need one new-display GDI
  fallback: **0-2 `ChangeDisplaySettingsExW` calls** in total.
- Add **0-1 HDR**, **0-1 DPI**, **0-1 ICC** write if requested and different,
  and always **1 driver-create IOCTL**. These are different API scopes; do
  not add them up as an equivalent number of global DWM stalls.
- Each extra guard correction adds **1-2 CCD attempts**; an explicit
  activation adds **1-2 per attempt**; a changed arrangement active set adds
  the **1-4** described above. Arrival, errors, concurrent user changes and
  the first stream's own recovery prevent an unconditional fixed count.

Removal is asymmetric: it has no create-time Windows topology recall,
startup guard or new-display HDR/mode setup. It can still change the topology:
`Lease::drop` may deactivate the departing target with `set_active`, then
`VirtualDisplay::drop` sends removal IOCTL `0x902` and lease release `0x904`.
No measured hitch on removal does not imply no global call there.

### Minimum-change proposal and why it is deferred

Two corrections to the suggested one-call approach are essential:

- `SDC_NO_OPTIMIZATION` forces the mode change down to the driver for **each
  active display**. Leave it out. `SetDisplayConfig` enables only the supplied
  active paths; passing just the TV/new-VDD paths would switch off omitted
  displays, including the first stream. Supply all surviving paths and edit
  only the intended ones. Use `A|U` first; neither database persistence nor
  permission to alter other supplied modes is needed for an exact temporary
  layout. [Microsoft: SetDisplayConfig](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setdisplayconfig).
- HDR is a separate device-info request, not a field that can be folded into
  a CCD path/mode array. A GDI call naming the new output is device-scoped,
  but does not promise other outputs will remain uninterrupted.
  [Microsoft: DisplayConfigSetDeviceInfo](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-displayconfigsetdeviceinfo),
  [Microsoft: ChangeDisplaySettingsExW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-changedisplaysettingsexw).

The first candidate should be restricted to a **new, independently sourced
owned VDD joining an existing extended stream arrangement**:

1. Retain creation/resolve and the immediate strict dormant-TV correction.
   Do not leave the TV on through a possibly three-second HDR transition.
   Keep identity checks, mode compaction and the bounded startup guard.
2. Keep required HDR/DPI setup and settling. For this joining-display case
   only, defer the `after settings` mode write to arrangement. Keep the
   first/single-stream, reused-display, exclusive/primary/isolated and
   recovery paths as they are.
3. From one fresh active topology, model the new display at its **requested**
   size/rate before composing its final position. Plan the mode and position
   together, retain every unrelated source/target mode, path order and clone
   relationship, discard unreferenced modes and remap indices. Reject a
   shared/clone source rather than editing another display through it.
   If the modeled result already matches, issue no call. Otherwise apply
   that full surviving configuration once with `A|U`.
4. Journal the same intended arrangement before mutation; recheck ownership
   and topology immediately before apply and verify afterwards. A refused
   strict plan needs a fresh query before using the existing fallback
   sequence, never a replay of a snapshot taken before HDR/hotplug. Keep
   readback and a conditional rate correction if the driver does not honor
   the requested timing; do not drop the final verification just to save a
   log line. `ALLOW_CHANGES` remains an explicit compatibility fallback,
   whose potential to retime the first display must be measured.

For the three-CCD example this aims at **two**: immediate TV prune, then
combined new-display mode/position. It cannot remove Windows' own arrival
reconfiguration or a necessary HDR transition. Folding the prune into the
same final call could reach **one** when HDR is already satisfied and the
final arrangement is known at `created`, but currently that information and
its recovery journal belong to a later lease. Delaying protection or moving
that lease into creation is a larger behavior/lifetime change, not a safe
flag substitution. The one-CCD already-satisfied case has no redundant host
apply to remove while still preserving the TV's inactive state.

Risks requiring host validation: known driver rejection of exact layouts
during arrival (the current guard has a fallback for it); missing/invalid
target timings when changing refresh; cloned/shared source IDs; renumbered
GDI names after pruning; HDR recalling another topology; stale snapshots
overwriting a concurrent user layout change; preserving recovery journals
and lock ordering across `DISPLAYS`, `SETTINGS` and the arrangement lease.
CCD has no atomic compare-and-set, so even a final requery cannot eliminate
the last race. Blindly removing `ALLOW_CHANGES` from `Topology::restore`
would also alter physical-display restoration and single-stream behavior.

Before implementing that candidate, add pure planner tests over synthetic
paths/modes and arrangement nodes: all settings already satisfied -> no
apply; TV reactivation alone -> one prune with other modes preserved;
new mode plus position -> one apply; untouched first-stream rational timing,
primary, negative coordinates and clone relationships; compaction of orphan
modes with shared mode indices; owned/replaced/missing target identity;
reject the new target sharing an existing source; changed snapshot ->
replan; requested dimensions used for placement; unchanged single-stream
and non-extended fallback selection. The existing `display/hotplug.rs`
tests cover pruning, compaction, identities and stale-layout detection,
but cannot prove DWM/driver latency or acceptance of a new combined apply.
No unused planner or speculative runtime change was added here.

Apollo/Vibepollo comparison: this checkout has no `src/` C++ host and no
Apollo/Vibepollo display implementation under `third-party/` (only
`moonlight-common-c` and `nanors`). `docs/butterpollo-cpp.md` records the C++
source removal after rc.23. The Rust `resolve` comment credits Vibepollo's
delayed inactive-target activation, but that is not a source audit of its
current flags or call counts. No comparison numbers are invented.

### Verification and owner's next measurement

Validation used the requested `performance-probe\rust-env.ps1`,
`CARGO_TARGET_DIR=D:\bp-build\hitch-target` and `C:\src\cargo-one.ps1` for
every cargo invocation:

- `fmt --all -- --check`: passed.
- `clippy --workspace --all-targets --locked -j 2 -- -D warnings`: passed.
- `test --workspace --locked -j 2`: **failed**, with 539 passed, 2 failed
  and 44 ignored. The unchanged default suite includes two unignored
  desktop/GPU-dependent tests: `display::tests::heartbeat_check_finds_the_monitors_that_monitors_lists`
  failed with `0x80070057`, and `encoder::tests::encoders_open_on_the_configured_gpu`
  failed with "no desktop display or hardware GPU" in this tool session.
- Rerunning that workspace command with
  `-- --skip display::tests::heartbeat_check_finds_the_monitors_that_monitors_lists --skip encoder::tests::encoders_open_on_the_configured_gpu`:
  **passed**, 539 passed, 44 ignored, 2 filtered out; doc-tests also passed.
- `git diff --check`: passed.

The required unfiltered test gate is **not green**; neither the unrelated
tests nor the machine-wide wrapper were changed. The wrapper returned exit
code 0 even for the failed test run, so results above come from Cargo's test
summary, not that wrapper status. Logs are in
`D:\bp-build\hitch-target\clippy.log`, `test.log` and
`test-without-hardware.log`.

No displays or installed service were changed and no streams were started.
Ignored hardware fixtures stayed skipped. The requested default suite
attempted the two unignored probes above; the follow-up explicitly excluded
them. This commit has no runtime difference to A/B.

For the next host run, keep the first client's renderer, codec, HDR, rate,
resolution and capture backend fixed; repeatedly join/leave the second
client with the TV connected but inactive. Record first-stream picture-age
maximum and p95/p99 in a short window around creation, longest interval with
no new rendered/captured picture, and time to recover to baseline. Whole-run
averages can hide the reported 1.3-second event. Compare creation with removal
and, when convenient, a TV-disconnected control to separate arrival cost
from the corrective prune. Test SDR/HDR separately and verify both displays'
actual refresh/size/position/HDR plus the TV's inactive state.

Correlate arrival, the guard warning, both mode stages and layout with
timestamped native call entry/exit, flags, return codes and before/after
topologies (API tracing or temporary measurement instrumentation). The
current INFO logs cannot supply this. A DWM/DxgKrnl trace can distinguish a
composition/driver pause from capture reinitialization or downstream delay.
After a candidate exists, A/B/A/B the same launch sequence and verify that
actual CCD commits decrease **and** first-stream frame gaps/picture age
improve without retiming the first display or re-enabling the TV. If only
one strict prune already occurs, focus the experiment on driver arrival,
HDR and capture recovery before implementing batching.

## October 9 job 4 split and two latency wakes: host A/B

A/B/A/B on the RX 7900 XT host, October 9 02:05-02:16, against their common
base `92afcce5` (which predates the pacer fix, so some runs claim 121 fps on
both sides): isolated extended virtual display at 240 Hz, motion probe at
240 Hz, 1968x2184 HDR 120 fps at 80 Mb/s, hardware-decoding receiver, 35 s
per run. Picture age is mean / p95 / p99 in ms; detect is the host's capture
detection mean / p95.

| Case | Build | Picture age r1 | Picture age r2 | Host mean ms | Detect ms |
| --- | --- | --- | --- | --- | --- |
| HEVC, DDX | base | 12.16 / 12.67 / 13.16 | 12.10 / 12.63 / 12.95 | 3.64, 3.51 | 0.49 / 0.82, 0.53 / 1.11 |
| | job 4 | 12.22 / 12.83 / 13.31 | 12.77 / 14.78 / 17.08 | 3.59, 3.45 | 0.55 / 1.12, 0.47 / 0.99 |
| AV1, DDX | base | 11.86 / 12.50 / 12.80 | 12.39 / 13.43 / 16.69 | 3.22, 3.09 | 0.56 / 1.12, 0.45 / 0.86 |
| | job 4 | 11.85 / 12.46 / 12.70 | 12.40 / 13.36 / 16.70 | 3.22, 3.09 | 0.55 / 1.12, 0.43 / 0.74 |
| HEVC, WGC | base | 12.89 / 13.65 / 16.90 | 12.86 / 13.51 / 15.16 | 3.43, 3.44 | 0.12 / 0.16, 0.12 / 0.16 |
| | wakes (`dfc865c8`) | 12.88 / 13.56 / 15.24 | 12.89 / 13.56 / 16.94 | 3.43, 3.44 | 0.12 / 0.16, 0.12 / 0.16 |

- Job 4 (`Media::start` split into named setup functions, per-frame code
  moved unchanged): same within noise. AV1 matches to 0.1 ms; the HEVC r2
  tail is a run where both host and receiver ran at 121 fps (the old pacer),
  which shifts phase. Shipped as `7e9f8a22`.
- The WGC helper skipping its 500 µs sleep after a delivery (`dfc865c8`)
  changes nothing measurable here: detection is 0.12 / 0.16 ms on both. With
  one frame per 4.2 ms, the next frame is never ready right after a delivery,
  so the skipped sleep never mattered. The IDR/RFI wake (`81c8fb3a`) needs a
  static screen with packet loss to show; that was not measured. Neither is
  shipped yet: both stay on the local `codex/wake` branch until a loss test
  shows the recovery frame coming sooner.

## October 9 pacer claims at the stream rate (bug hunt item 1): host A/B

`stream_policy.rs` refilled pacing credit at 1.01x the stream rate, so a
source faster than the stream (the default 2x virtual display, a 165 Hz game)
was claimed at 121.2 fps on a 120 fps stream and 60.6 on 60. `3309053` (on
main as `75414f78`) refills at exactly 1.0 when the source interval is under
0.95 periods. A/B/A/B on the RX 7900 XT host, October 9 01:24-01:31,
`0b1967d` against `3309053`: loopback with the hardware-decoding receiver,
isolated extended virtual display at 2x the stream rate with the motion probe
at 2x, DDX, HEVC, 35 s per run, `minimum_fps_target` unset.

| Case | Build | Receiver steady fps | Host fps | Picture age mean / p95 / p99 ms | claim_wait p95 ms | Dropped |
| --- | --- | --- | --- | --- | --- | --- |
| 1968x2184 HDR 120, 80 Mb/s | before r1 / r2 | 121.09 / 121.00 | 120.75 / 120.79 | 12.96 / 13.95 / 17.05, 12.94 / 13.90 / 17.12 | 1.04 / 0.99 | 0 / 0 |
| | after r1 / r2 | 120.02 / 120.00 | 119.72 / 119.79 | 13.07 / 13.72 / 17.14, 12.94 / 15.18 / 17.11 | 0.96 / 0.97 | 0 / 0 |
| 1080p60, 20 Mb/s | before r1 / r2 | 60.52 / 60.30 | 60.32 / 59.97 | 7.95 / 12.10 / 19.76, 7.50 / 8.11 / 11.08 | 2.19 / 0.03 | 0 / 0 |
| | after r1 / r2 | 60.00 / 60.00 | 59.56 / 59.65 | 7.62 / 8.26 / 8.75, 10.54 / 11.11 / 11.49 | 0.02 / 1.76 | 0 / 0 |

Result: the stream now leaves at exactly the stream rate (120.0 and 60.0
instead of 121.0 and 60.3-60.5); picture age is unchanged within run-to-run
noise (the 1080p60 runs differ by phase, in both directions). Shipped for the
rate: a client showing 120 Hz received 1.2 extra frames a second, which it has
to drop or queue (Moonlight's 121.0-121.2 received fps in the laptop runs
above). Not measured: Moonlight's own dropped-frame count on a real client
after the change; the loopback receiver decodes but does not present.

## October 8 repeats past the frame cap (PyroWave, VRR): host A/B

Measured on the RX 7900 XT host on October 9 (01:05-01:16), A/B/A/B,
`5e16d1c` (before; same Rust as `9cf57ce`) against `88d1dec` (after).
Loopback with the independent receiver (decoder plus per-frame size in its
timing CSV), not the laptop: an isolated extended virtual display at 200 Hz
(2x the stream rate), 1920x1080 at 100 fps with VRR requested,
`minimum_fps_target` unset, PyroWave at 400 Mb/s and AV1 at 50 Mb/s, motion
probe at 90 Hz on the 200 Hz display (presents on a 5 ms grid, so 10 and
15 ms gaps alternate), 35 s per run, 3 s warm-up dropped. Sent fps is counted
per one-second window from client arrival times.

| Case | Build | Sent fps p50 / max | New frames KB p5 / p50 | Picture age p50 / p95 / p99 ms | Worst gap ms |
| --- | --- | --- | --- | --- | --- |
| PyroWave, 90 Hz game | before r1 | 144 / 151 | 54.8 / 216.6 | 15.52 / 18.59 / 19.51 | 13.5 |
| | before r2 | 150 / 151 | 63.9 / 130.4 | 15.06 / 17.09 / 17.93 | 13.1 |
| | after r1 | 90 / 91 | 340.2 / 494.2 | 14.87 / 17.94 / 18.56 | 18.6 |
| | after r2 | 90 / 91 | 377.5 / 494.2 | 14.67 / 16.90 / 17.63 | 15.9 |
| PyroWave, still desktop | before r1 / r2 | 97 / 100, 97 / 101 | all 193 KB repeats | - | 20.1, 12.0 |
| | after r1 / r2 | 20 / 67, 20 / 82 | all 193 KB repeats | - | 51.4, 51.3 |
| AV1, 90 Hz game | before r1 / r2 | 90 / 91 | 1.3 / 4.4 | 10.76 / 13.83 / 14.74, 10.26 / 12.70 / 13.39 | 17.6, 15.5 |
| | after r1 / r2 | 90 / 91 | 1.3 / 4.4 | 10.58 / 13.04 / 13.78, 10.47 / 12.92 / 13.64 | 15.8, 17.7 |

Result: the report reproduces on the old build and is fixed. Before, a 90 fps
game on a 100 fps PyroWave VRR stream went out at 144-151 fps, because about
40% of the frames were repeats, and new frames were budgeted from the short
gap after a repeat: median 130-217 KB against 494 KB after. After, exactly the
game's 90 fps goes out, every frame new, at full size; picture age is the
same or 0.3-0.7 ms lower. A still PyroWave desktop goes from 97 fps of repeats
to the 20 fps minimum, so the worst gap there is now 51 ms by design (max 67
and 82 fps are the first second, before the picture settles). AV1 is
unchanged within noise, as expected: it already had a 20 fps minimum and no
repeats while the game ran. Not measured: a real client over Wi-Fi (the
laptop), and a real game.

Report (a user, relayed by the owner; rc.25, PyroWave with VRR at 100 fps):
motion windows at 105 to 136 fps. Saving a minimum frame rate of 30 brought
motion to just under 100, a still screen toward 30 and a 34 ms worst gap.
Latency-relevant code change for rc.27; nothing measured on a host yet.

- An unset `minimum_fps_target` was the full stream rate for PyroWave, a
  mis-port: Vibepollo's default of 20 applies to PyroWave as
  min(20, stream rate), and only a saved 0 means the full rate. It is now 20
  for every codec (`stream.rs`).
- Arrival pacing (VRR, `frame_pacing=arrival`) now spends pacer credit on
  every encode, static repeats and same-picture keyframes included. Only new
  pictures did before, so a game frame that came just after a repeat went out
  at once, past the cap. Grid pacing already counted repeats.
- A new PyroWave picture is budgeted at least min(time since the previous new
  picture, one period), never only the gap since a repeat
  (`pyrowave::Interval`); repeats keep the gap since the last encode, and
  critical FEC still caps every frame at one period.
- Model (`repeats_count_toward_the_stream_rate`: a game at about 90 fps with
  9 to 13 ms gaps on a 100 fps VRR stream): with the minimum at the stream
  rate, 158 fps uncounted and 101 fps counted; counted, game frames after a
  repeat wait up to 9.8 ms and 7 of 180 are replaced by a newer one. With the
  20 fps default no repeat goes out while the game runs, so no frame waits.
- Expected costs: after a stall of 50 ms or more, the first game frame can
  wait up to half a period if a repeat just went out. A still PyroWave
  desktop now sends 20 fps instead of the stream rate, so detail FEC
  (`DetailFec`, parity for unchanged blocks when frames come slower than the
  stream rate) now engages there, as in Vibepollo (inferred from the code).

Host A/B to run (idle host, never exclusive display mode): PyroWave, VRR,
100 fps to the laptop client with a renderer below the stream rate with
uneven frame times, then a still desktop. Compare the commit before this
change with this one: sent fps per one-second window (p50 and max), size of
new frames (p5 and p50), picture age, and the worst gap on a still screen.
Repeat once with AV1 for the counting change alone.

## October 8 user report: exclusive layout ignored, picture freezes

Report (relayed by the owner): with the virtual display layout on exclusive
and Remote Monitor chosen on the client, the first physical display stayed on
and primary; and sometimes the picture stops while the stream and its audio
go on. Code changes only; nothing here has run on a host yet.

- Remote Monitor never applied the layout: the arrangement was taken for the
  Stream role only (`display_session.rs`). It now follows which displays stay
  on and which is primary (exclusive and the primary layouts), keeps the place
  the remote monitor layout gives it, restores the layout when its stream ends,
  and reapplies it when the monitor's lease recreates the display (`2180d8f`).
- WGC capture could go silent for good: when the captured display leaves the
  desktop even briefly (a layout change switching it off and on), its capture
  item closes and WGC returns no frames and no error, while the host re-encodes
  the last picture and audio continues. The capture item's `Closed` event and a
  once-a-second monitor-handle check now raise an error, which reopens capture
  on the display that is there now.
- After a reopen, capture recorded the display identity read after opening,
  so a display recreated during the reopen was never followed. It now records
  the identity it opened.
- A recovered virtual display was handed to capture only once its HDR and mode
  were restored; while that kept failing, capture stayed on the old display.
  Capture now follows at once and the restore is retried.

To confirm on the host (never in exclusive mode on the owner's PC): a stream
on a virtual display while another device's virtual display arrives and
leaves, checking that `capture reopened` follows any `left the desktop` error
and the client keeps fresh pictures. Not addressed: a session thread stuck in
a driver call (encoder terminate or init) would also freeze the picture with
audio running; there is no watchdog for it yet.

Host check, October 9 01:36-01:41, build `9cf57ce` (includes `2180d8f` and
`b193b88`), isolated host on the RX 7900 XT with the extended layout, WGC,
HEVC 1920x1080 at 60 fps to the hardware-decoding receiver on a per-client
virtual display at 120 Hz with the motion probe at 120 Hz, 60 s:

- Freeze fix: while that stream ran, three other clients each paired,
  launched Desktop on their own 1280x720 virtual display, streamed 5 s and
  disconnected without quitting the app. The first stream kept running to the
  end: no capture error, no "left the desktop", nothing to reopen. Host fresh
  fps stayed 59.9-60.5 per 5 s window with `wgc_stamp_frames` 242-308, except
  one window at 40 fps with a 195 ms send gap. No run of 20 fps repeats.
  Pass for the freeze.
- But each new virtual display hitches the other stream: picture age rose to
  0.5-0.6 s for about 1.3 s while the second and third displays were created
  (receiver p99 122 ms and max 612 ms over the run, 54 intervals over 1.5
  periods, 46 repeated pictures). The clients leaving caused none. The hitch
  is the age of what the renderer on the first display had presented, so it
  is likely DWM pausing presents during the topology change rather than the
  host's capture; not yet compared with Vibepollo. A first try, where the
  extra clients quit the app on leaving (as the test receiver does by
  default), ended the first stream with them, as expected for a quit.
- Remote Monitor (`2180d8f`): a launch with `remote_monitor=1` and the host
  layout extended logged `role=RemoteMonitor ... layout="extended"
  arrangement=None` and made no display changes beyond creating its own
  display. It also warned "Display refresh is 60.000 Hz for a 60.000 fps
  stream": the remote monitor runs at the stream rate, not 2x as a stream's
  virtual display does. Exclusive and primary were not tried on this PC (the
  owner's monitor would switch off), nor a per-device layout.

## October 8 real-client picture age over Wi-Fi (rc.24, laptop)

First render-to-decode measurement from a second machine instead of loopback.
Host: RX 7900 XT, 2.0.0-rc.24, Desktop app on the Extended virtual display,
`motion_probe` drawing a QPC barcode (240 Hz source on the 120 fps runs, 120 Hz
on the 60 fps runs). Client: IdeaPad, Radeon 780M, driver 32.0.31035.1003,
**on Wi-Fi only** (802.11ax, 5 GHz, 2402 Mbit/s link); host on Ethernet.
Decoder: the independent `rust/tests/moonlight_client.c` built on the host from
`e5520ac0` (FFmpeg D3D11VA for HEVC and AV1, decoding on its own thread, reading
back only a 640x8 barcode strip), launched by a separately paired test client
("lap-measure", Extended layout). Clock: the host pings the laptop every 200 ms
over UDP; the 20% of pings with the lowest round trip give a linear
host-to-laptop QPC map (1,682 pings, minimum RTT 1.16 ms so offset error is
within about +-0.6 ms, fitted drift 17.5 ppm, fit residual at most 0.3 ms).
One 30 s run per row, the first 3 s dropped, first decode of each picture only.
HDR requested on every row; all decoded as 10-bit P010, BT.2020, PQ.

"Received" is when the client's decode thread starts on the picture (fully
received, after its queue). "Decoded" adds the client's hardware decode plus the
synchronous strip readback; Moonlight-qt reports 0.3-0.7 ms hardware decode on
the same laptop, so a Moonlight user's picture age lies between the two columns.

| Config (bitrate) | Fresh fps | Render to received avg / p95 / p99 | Render to decoded avg / p95 / p99 | Host processing avg / p95 | Client decode avg |
| --- | ---: | --- | --- | --- | ---: |
| 1968x2184@120 AV1 HDR (80 Mbps) | 120.0 | 13.54 / 14.33 / 14.93 ms | 18.76 / 19.61 / 20.28 ms | 3.04 / 3.3 ms | 5.21 ms |
| 1968x2184@120 HEVC HDR (80 Mbps) | 120.0 | 14.13 / 14.97 / 15.90 ms | 19.32 / 20.35 / 21.35 ms | 3.70 / 4.1 ms | 5.19 ms |
| 2560x1440@120 AV1 HDR (50 Mbps), repeat 1 | 120.0 | 13.34 / 14.15 / 16.12 ms | 18.58 / 19.76 / 21.49 ms | 2.77 / 3.0 ms | 5.24 ms |
| 2560x1440@120 AV1 HDR (50 Mbps), repeat 2 | 120.0 | 13.28 / 14.07 / 15.63 ms | 18.10 / 18.92 / 20.91 ms | 2.76 / 3.0 ms | 4.82 ms |
| 2560x1440@120 AV1 HDR (50 Mbps), first run (noisy) | 119.5 | 14.78 / 21.87 / 34.85 ms | 20.25 / 29.51 / 45.16 ms | 2.74 / 3.0 ms | 5.47 ms |
| 2560x1440@120 HEVC HDR (50 Mbps) | 120.0 | 13.67 / 14.69 / 19.87 ms | 18.80 / 20.22 / 24.97 ms | 3.11 / 3.2 ms | 5.13 ms |
| 1920x1080@60 AV1 HDR (20 Mbps) | 60.6 | 12.91 / 15.05 / 17.89 ms | 16.88 / 19.40 / 22.18 ms | 2.16 / 2.5 ms | 3.97 ms |
| 1920x1080@60 HEVC HDR (20 Mbps) | 60.5 | 12.99 / 14.15 / 16.62 ms | 16.51 / 17.69 / 20.28 ms | 2.39 / 2.7 ms | 3.51 ms |

Every run received all frames with zero decode failures; unique fresh pictures
matched the stream rate (1440p AV1 skipped 17 source frames and had 183
intervals over 1.5 frame periods, which is its wider p95/p99). Two repeats
at 15:42Z and 15:43Z (14 and 24 late intervals, no skipped source frames)
measured 13.3 ms average and 14.1 ms p95 to received; the host side of the
first run was clean, so its late frames came from Wi-Fi or the client. AV1 is 0.6 ms faster than HEVC to
the client at native resolution, mostly host processing (3.0 vs 3.7 ms).

A first batch at 15:04Z (older client build) is discarded for 120 fps and AV1:
the client decoded and read back whole frames on its receive thread (8-10 ms a
frame), shed about 11-15% of frames and queued for seconds, and its FFmpeg chose
libdav1d without D3D11VA for AV1. Its valid 1080p60 HEVC row (12.91 / 14.82 /
17.22 ms to received) agrees with the table.

Not measured here: a game load on the host, Ethernet on the client, display
scanout, PyroWave (stock and fixture clients lack it on this laptop), and
Vibepollo on the same fixture. Moonlight-qt averages on the same laptop and host
(rc.21-rc.24): host processing 2.6-3.6 ms, network 1-3 ms, decode 0.3-0.7 ms.
Raw files: laptop `C:\Users\ramaz\bp-measure\phase2b`, host clock pings and
rendered-frame maps in the project files.

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
Follow-up: an automatic AMF "safe profile" after repeated stalls (low-latency
usage, no SmartAccess Video, LTR or forced queue) was tried and removed: it
raised latency for the rest of the session on an unproven RDNA4 link, and
overrode the owner's settings. Compute conversion still moves to the
graphics queue only after a second separate failure, as before.

Second follow-up, from the reporter's log: the last timings before the
stall were healthy (59.4 fps, encode 4.6 ms mean, 5.4 ms p99, host max 6.1
ms), the GPU was near 99% with a game, and `network_fec` had fired. The
stall was sudden, not a slow encoder, and Sunshine on the same PC only got
choppy when the GPU was saturated. So a stall now waits 250 ms before the
first recreation (then 500 ms, 1 s, 2 s), the budget is 20 s, and a frame
too large for full FEC in Moonlight's four blocks keeps the parity that
still fits instead of none (849-1019 shards; 900 shards at 20% now get
13%). `wgc_stamp_future_frames` 302/303 with WGC's stamp about 4 ms ahead
is a capture clock offset the timings keep signed, not a cause.
Not measured: the RX 9070 XT is not available, and the RX 7900 XT has not
reproduced the stall. The debug soak's encoder-failure fault exercises the
encode-error path, not this one; a stall injection is still to be written.

October 9 follow-up: the debug soak fault directory now accepts `encoder
stall`, containing a duration in milliseconds (for example `300`), or an
empty `encoder stall.persistent`, which withholds output until removed.
The finite request is consumed once. The fault holds completed frames at
the session's encoder boundary, counts them as backlog, and survives
recreation; pictures from the old encoder are discarded on recreation.
It is compiled only with debug assertions or tests, uses the existing
`BUTTERPOLLO_TEST_FAULT_DIR`, and has no release setting or CLI switch.

GPU-free tests in `host/src/stream/encoder_tests.rs` use a mock at the
encoder-output trait boundary, a virtual clock, the session's actual
recovery helpers and IDR request, and a packetized frame sink. They cover
300 ms of silence followed by a keyframe and more packets, a permanent
stall, the 250/500/1000/2000 ms recreation waits (then capped at 2000), a
short stall released by polling, resetting backoff for a later failure,
and output progress with an unchanged backlog. The 20 s budget starts at
the first recovery attempt, not at the start of the fault. Polling normally
ends it with `the encoder did not produce a frame during recovery`; if the
stall deadline check runs first, the existing message is `the encoder
stopped returning frames`. Both paths are tested, including the failed
session's `0x80004005` termination reason. The sink verifies packet delivery
resumes; this does not exercise a real GPU, network stream or decoder.

Code and local SDK audit (AMF headers version 1.5.2 in
`%BUTTERPOLLO_FFMPEG_ROOT%/include/AMF`; `third-party/` has no AMF docs in
this worktree):

- `amf.rs::poll` calls `QueryOutput` once while an input is outstanding,
  handing one completed frame to the sender immediately. REPEAT, EOF,
  NEED_MORE_INPUT and OK with null data produce no output; it never drains
  a running encoder. `QueryTimeout` is requested as 1 ms (failure ignored;
  the effective settings line shows the actual value). The session polls
  pending output at 100 us where pacing permits, and stops submitting at
  two outstanding pictures. AMF independently drops tracked submissions
  older than two seconds and requests an IDR. These are separate limits.
  At the capped two-second wait, that expiry can lower the backlog first
  and reset the backlog timer; it does not reset the no-output recovery
  budget. Actual recreation spacing also includes initialization and
  refilling the queue: `wait_ms` reports the stall threshold.
- The SDK's `VideoEncoderHEVC.h` documents `HevcInputQueueSize` default 16
  and `HevcQueryTimeout` in milliseconds. `amd_input_queue_size=0` leaves
  the driver queue alone; a positive setting requests 1–32. That is not
  the session's two-picture backlog or the native eight-surface ownership
  limit. Both CPU and GPU SubmitInput loops poll and yield on INPUT_FULL,
  failing after 100 ms; the GPU capacity wait has its own 100 ms limit.
  INPUT_FULL therefore normally gives an **encoding error**, distinct from
  successfully accepted input that never produces output.
- Defaults remain ultra-low-latency **usage**, speed and latency VBR;
  `LowLatencyInternal` is a separate optional setting. PA is normally off
  (quality rate controls can enable it with one-frame lookahead); pre-encode
  can depend on the driver's usage preset. Forced low latency plus explicitly
  enabled SmartAccess Video is already guarded. None of these settings was
  changed. The existing `AMF encoder settings` log reads back queue size,
  timeout, usage, PA/pre-encode, SAV, LTR and hardware-instance count.
- `compute.rs` normally converts RGB to NV12/P010 on D3D12 compute, asking
  for high priority, then normal if denied (global realtime is opt-in).
  Its queue waits for the capture-ready fence before conversion. AMF gets
  the texture in COMMON and a per-texture fence via the GUID contract in
  `core/D3D12AMF.h`. The D3D11 fallback uses the graphics context. Only a
  second *separate* encoder failure disables compute, as before. The host
  requests DXGI thread priority 7 and WDDM process scheduling class 5,
  falling back to 4; these are not an AMF VCN-priority or reserved-capacity
  setting. No encoder-priority property is set by this path.

The SDK contracts agree with AMD's [asynchronous encoder guide](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/Guide-for-Video-CODEC-Encoder-App-Developers)
and [HEVC header](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/public/include/components/VideoEncoderHEVC.h).
Ranked hypotheses below are inferences, not an RX 9070 XT reproduction:

| Rank | Hypothesis | Evidence that would distinguish it |
|---|---|---|
| 1 | AMF/driver or D3D12 handoff wedges under contention after accepting input. Healthy timings followed by abrupt silence fit better than a steady throughput limit. | `submit_result=AMF_OK`, repeated REPEAT/OK-null with two old inputs, healthy devices, conversion fences completed but AMF's surface fence/ownership not advancing. If all fences complete, the wedge is farther inside AMF/output processing. A WPR/GPUView trace and driver-version comparison are needed to separate these. |
| 2 | Capture-copy or colour-conversion work is starved by the 99% game load, so VCN has no ready input. High queue priority still shares GPU execution/memory resources. | `conversion_completed < conversion_submitted` or a surface's `completed < conversion_value`. Trace the capture fence dependency and compute scheduling with GPUView; a ready conversion weakens this hypothesis. D3D11 input instead needs a graphics-queue trace. |
| 3 | A queue/latency/analysis configuration exposes driver buffering or a driver bug. A two-picture host cap can also conflict with a mode needing more input. | Compare effective InputQueueSize, Usage, LowLatencyInternal, SAV, PA/lookahead, pre-encode and LTR against defaults. Repeated NEED_MORE_INPUT/EOF is distinct from ordinary REPEAT. INPUT_FULL with the explicit 100 ms submit error identifies submission backpressure. The earlier forced-queue RDNA4 report is a lead, not proof about this reporter's settings. |
| 4 | The single VCN is delayed by scheduling, another encoder, or expensive recovery keyframes/pre-encode. | All input fences ready, devices healthy, output eventually resumes as the waits grow. Correlate VCN engine occupancy/other sessions in GPUView and the first recovered IDR; collect under-load encode latency. The pre-stall 4.6 ms mean weakens sustained 4K60 throughput exhaustion. One hardware instance leaves no second instance for split-frame relief. |
| 5 | Device removal/TDR, or surface retention/accounting failure. | Non-success `d3d11_removed`/`d3d12_removed`, a fence completed value of `u64::MAX`, Windows display-driver reset events, or native eight-surface capacity errors. `retained` far above `in_flight` and completed surface fences with excess owners suggest retention; correlate with `encoder_dropped` and queue-drain errors. |

New diagnostics, only on startup, failure/recreation or recovered output:

- `AMF encoder settings` additionally reports `input_memory`,
  `compute_priority` and the read-back `d3d11_gpu_priority`.
- `encoder stall recovery`: `wait_ms`, `recreation`, `recovery_ms`.
- `AMF stall snapshot`: last SubmitInput/QueryOutput numeric codes and
  symbolic `submit_result`/`query_result`, lifetime `input_full` retry count,
  `in_flight`, `retained`, oldest PTS/age, next PTS and D3D11 device status.
  A nonzero lifetime count alone does not locate the current failure.
- `AMF compute progress at stall`: compute priority, last conversion
  submission/completion fence values and D3D12 device status.
- `AMF surface fence at stall`: at most eight target rows with conversion,
  AMF and completed fence values plus texture owner counts. These are
  observations at different instants, not an atomic GPU snapshot.
- `encoder output resumed`: elapsed recovery time. The finite debug
  injection also logs `soak injected encoder stall` with `duration_ms`.

The release frame path only retains AMF result codes and a retry counter
for these snapshots; it adds no timer reads, GPU waits, property writes or per-frame
logs. Timing, queue limits, defaults and recovery decisions remain unchanged.
The software deadlines still require AMF calls to return: a driver blocking
inside QueryOutput, SubmitInput, Init or Terminate can overrun them. If logs
stop entirely, capture the streaming thread's stack/ETW trace; the synthetic
output fault does not simulate a hung driver call. No hardware/ignored tests,
streams or installed-service changes were used for this investigation.

Validation: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings` and
`cargo test --workspace --locked -j 2` all passed through
`C:\src\cargo-one.ps1`, using `D:\bp-build\stall-target`.
Workspace result: 546 passed, 0 failed, 44 ignored; all six new stall tests
passed. No ignored tests were run.

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

The laptop's smoke test on the installed rc.24 (13:17-13:18 UTC, motion on
the streamed display, Extended layout) received 118.7 fps for 1968×2184 AV1
HDR and 120.1 fps for HEVC, with 3.0 and 3.6 ms average host processing.
Moonlight's AV1 maximum was 20.6 ms against 13.4 ms on rc.23. It is again
the stream's first frames: every 5 s host window of that AV1 session peaked
at 3.82-3.93 ms (p99 3.36-3.81 ms), and the HEVC session's at 4.51-4.53 ms.
A first keyframe's time varies from start to start (12.7-17.7 ms in the
loopback runs), so one start does not show a regression.

### rc.24 against Vibepollo 2.0 on the same GPU

The loopback fixture with the pinned Vibepollo 2.0 build
(`day-work-20261002\vibepollo-baseline-build`, `VIBEPOLLO_ISOLATED_BENCHMARK`)
and the installed rc.24, alternating in one batch on October 8 (16:40-16:57
local), two runs per cell, identical settings: native AMF at ultra-low
latency, Desktop Duplication capture for both (as on October 4), a virtual
HDR display, Extended layout, hardware decoding in the client. No game load:
the large October 4 gap (42.4 against 96.4 ms) was beside a GPU load, where
Vibepollo fed its encoder about 24 fresh pictures a second; its idle gap was
13.8 against 16.0 ms, in line with these. The owner's monitor now runs at 2560×1440
120 Hz, and the virtual display came up at 120 Hz instead of twice the stream
rate in every run of both hosts, so the source drew at 120 fps; these numbers
compare only with each other. The monitor's mode was checked before and after
(2560×1440 at 120 Hz both times); Vibepollo's display recovery changed nothing
this time.

| Picture age mean / p95 / p99 (ms), host latency | rc.24 | Vibepollo 2.0 |
|---|---|---|
| 1968×2184 HEVC HDR 120 fps, 80 Mbps | **17.81 / 18.94 / 19.56**, 3.50 ms | 19.97 / 20.43 / 20.87, 4.04 ms |
| 1968×2184 AV1 HDR 120 fps, 80 Mbps | **17.42 / 18.10 / 18.62**, 3.20 ms | 18.88 / 19.54 / 19.90, 3.89 ms |
| 2560×1440 HEVC HDR 120 fps, 50 Mbps | 17.09 / 17.99 / 18.46, 3.08 ms | 17.19 / 17.68 / 17.93, 3.52 ms |
| 1920×1080 HEVC HDR 60 fps, 20 Mbps | **14.67 / 15.50 / 15.82**, 2.46 ms | 21.05 / 21.25 / 21.43, 1.30 ms (one run) |

At the owner's native size rc.24 delivers the picture 1.5-2.2 ms sooner at
every percentile, with 0.5-0.7 ms less host latency; at 1440p the two are
equal. At 1080p60 one Vibepollo run's renderer never started and the other
was 6.4 ms slower, although Vibepollo reports less host latency there
(its host latency excludes part of the path Butterpollo's includes). Every
run delivered all source pictures except one rc.24 AV1 run: 9.5 s in, WGC's
shared texture failed (`0x887A0026`, keyed mutex abandoned), capture
reopened after a 258 ms gap, and afterwards the client read only 53% of the
pictures while the host still sent 119 fps. Its picture age (17.41 ms) is
from the pictures it did read. That recovery path needs a look: the picture
after reopening may not have been the virtual display's. [Later traced to the
fixture: the virtual display moved, Desktop Duplication lost access and
reopened correctly, but `motion_probe` stayed at the old coordinates; fixed in
`aff04350`.]

Artifacts: `bench-rc21\vp-*`, `displays-before-vibepollo.json`.

#### Beside a game

The same comparison with the 60 fps-capped game beside the stream
(`gpu_load 45 1000 60 200`, which leaves the client's decoder room), two
alternating runs per cell, 18:41-18:55 local, Desktop Duplication for both:

| Beside the game, picture age mean / p95 / p99 (ms), host latency | rc.24 | Vibepollo 2.0 |
|---|---|---|
| 1968×2184 HEVC HDR 120 fps | **19.61 / 21.56 / 23.10**, 3.4 ms | 26.96 / 37.62 / 39.25, 8.3 ms |
| 1968×2184 AV1 HDR 120 fps | **17.15 / 18.96 / 22.18**, 3.1 ms | 24.53 / 36.77 / 38.05, 7.4 ms |
| 1920×1080 HEVC HDR 60 fps | 17.84 / 21.78 / 22.84, 2.2 ms | 20.55 / 22.55 / 22.66, 5.7 ms (one run) |

Beside the game rc.24 delivers the native picture 7.4-7.8 ms sooner on
average and about 16-18 ms sooner at the 95th and 99th percentiles; its host
latency stays at 3.1-3.4 ms while Vibepollo's rises to 7-10 ms. Vibepollo also
delivered fewer new pictures (103-118 a second for HEVC, 106-111 for AV1,
against 117-120). At 1080p60 the two are close. One Vibepollo 1080p run is
left out: its source ran at 60 Hz instead of 120. This matches the October 4
result that the gap opens under GPU load, though the load and absolute values
differ. Artifacts: `bench-rc21\vl-*`.

### AV1 at 1968×2184: padding, and why the render-size rewrite is not shipped

The RX 7900 XT (driver 32.0.31041.1004) encodes a 1968×2184 AV1 stream as
1984×2186 even with `AlignmentMode` set to no restrictions, and the frame
header says `render_and_frame_size_different = 0` (FFmpeg `trace_headers` on
a native rc.22 keyframe). Every client therefore receives the padding unless
it crops to the negotiated size, as Moonlight-qt does. The host now logs a
warning once per encoder at such sizes.

Setting the render size in the frame header is possible without touching the
tile data: the flag gains `render_width_minus_1` and `render_height_minus_1`,
32 bits, so everything after it moves by exactly four bytes and the byte
alignment before the tiles is unchanged. A prototype that parses the
sequence header and the uncompressed frame header up to `render_size()`
(`bench-rc21\av1-render\av1_render.py`) rewrote the keyframe correctly:
`trace_headers` reads 1967 / 2183, and libdav1d decodes a picture
byte-identical to the original. But FFmpeg 9.0.2 still reports and outputs
1984×2186 for it with libdav1d: it does not crop to the render size. The
fix would only help a decoder that honours `render_size()`, and none
available here does, so it is not shipped. Check whether the Android
client's MediaCodec AV1 decoder crops to it, or crops to the negotiated
size itself, before revisiting.

### AMF settings per codec: quality at a given bitrate

`windows/examples/amf_quality.rs` encodes clip frames with the stream's
encoder (compute conversion, the host's default settings plus one change) and
writes the bitstream; FFmpeg 9.0.2 then scores it with VMAF and PSNR-Y
against the same frames (`bench-rc21\quality\run-quality.ps1`). Each frame is
encoded on its own, so encode time is submission to output without queueing.
Clips: a 2D arcade game (Teenage Mutant Ninja Turtles HD, 1080p60) and a 3D
shooter (GoldenEye XBLA, 720p30), scaled to 1968×2184 or 2560×1440, 180
frames each, SDR 8-bit. The rows are 1968×2184 at 30 and 80 Mbps (HEVC,
AV1) and 2560×1440 at 20 and 50 Mbps (HEVC, AV1, H.264), all at 120 fps.
Defaults reach VMAF 96.4-99.0 except H.264 at 20 Mbps (93.5 and 79.7).

Range of VMAF change against the default per codec, and the change in mean
and 99th-percentile encode time and in 99th-percentile frame size, first clip
(second clip in brackets where run):

| Setting | HEVC ΔVMAF | AV1 ΔVMAF | H.264 ΔVMAF | Encode mean / p99 | p99 frame size |
|---|---|---|---|---|---|
| `amd_quality=balanced` | -0.56 to +0.11 | -0.29 to 0 | -0.05 to 0 | same | same |
| `amd_quality=quality` | -0.32 to -0.05 | -0.31 to +0.07 | -0.03 to +0.02 | +0.7 / +1.1 ms HEVC, +6.1 ms AV1, +1.9 ms H.264 | same |
| `amd_rc=cbr` | +0.31 to +0.52 (-0.05 to +0.03) | -0.04 to +0.34 (-0.02 to +0.24) | +0.34 to +0.98 (-0.75 to +0.04) | same | +37-67% (+30-58%) |
| `amd_vbaq=disabled` | -0.13 to +0.24 (-0.03 to +0.40) | -0.22 to +0.07 (+0.01 to +0.19) | **+0.25 to +1.03 (+0.40 to +9.42)** | same | +1% (+6%) |
| `amd_high_motion_quality_boost=enabled` | -0.22 to 0 | -0.29 to +0.22 | 0 to +0.12 | +0.2 to +1.1 ms | same |
| `amd_usage=lowlatency` | -0.22 to +0.23 | -0.29 to 0 | -0.11 to +0.10 | +6 to +8 ms | same |

Only one change pays at no cost: **adaptive quantization (VBAQ) off for
H.264**. It raised VMAF in all four H.264 rows, by 1.0 and 9.4 at 20 Mbps,
with the same encode time. H.264 now defaults to it off; HEVC and AV1 keep
it on, where the effect is within ±0.4 either way. Constant bitrate scores
slightly higher on the first clip only because it spends more of the budget
(VBR with low latency stays 5-20% under the target), and its largest
frames grow by a third to two thirds, which costs pacing time on the
network; it stays off. The slower quality presets and the low-latency usage
buy nothing at these bitrates and cost up to 8 ms an encode.

Limits: SDR 8-bit sources (the owner streams 10-bit HDR; the encoder tools
are the same, but the gains were not measured there), two clips upscaled
from 720p and 1080p, and VMAF's default model. A 1 to 2 VMAF difference is
barely visible; the H.264 row at 20 Mbps is the one a viewer would notice.

The same probe then ran in 10-bit HDR (`--hdr 1`, `run-quality-hdr.ps1`):
the clips become linear light with SDR white at 203 nits, are uploaded as
scRGB FP16 like a captured HDR desktop, and the stream's encoder writes PQ
BT.2020; the reference is FFmpeg's zscale conversion of the same frames.
HEVC and AV1 at 1968×2184 (30 and 80 Mbps) and 2560×1440 (20 Mbps), 120 fps,
both clips, 48 encodes. Defaults reach VMAF 95.8-98.8.

| HDR, against the default | HEVC ΔVMAF | AV1 ΔVMAF | p99 frame size |
|---|---|---|---|
| `amd_vbaq=disabled` | -0.30 to +0.90 | -0.03 to +0.31 | -5% to +5% |
| `amd_quality=balanced` | -0.40 to -0.13 | -0.07 to +0.29 | same |
| `amd_rc=cbr` | -0.23 to +0.48 | -0.11 to +0.36 | +10% to +42% |

Encode time did not change with any of them (native HEVC 5.5 ms mean, AV1
4.9 ms). No change is consistent in HDR either, so HEVC and AV1 keep their
defaults; VBAQ stays on for both.

### rc.23 against rc.22

Main at `bad6076c` (`c4a2d51393496851`) against the installed rc.22, the
same fixture, alternating in one batch, Extended layout, hardware decoding.
Since the morning batches the owner's physical monitor runs in a half-width
mode and the fixture adds its display beside it rather than alone; every
cell sits 4-6 ms above the morning's and the runs spread over about 2 ms, so
compare only within this table.

| Picture age mean / p95 / p99 (ms) | rc.22 | rc.23 candidate |
|---|---|---|
| 1968×2184 HEVC 120 fps (3 runs) | 19.69 / 20.77 / 21.75 | 19.75 / 20.92 / 21.65 |
| 1968×2184 AV1 120 fps (3) | 19.00 / 20.23 / 21.06 | 19.22 / 20.26 / 21.01 |
| 2560×1440 HEVC 120 fps (7) | 17.70 / 18.80 / 19.33 | 18.14 / 19.18 / 20.63 |
| 1920×1080 HEVC 60 fps (3) | 15.40 / 17.56 / 19.08 | 14.49 / 15.76 / 17.28 |
| 1968×2184 HEVC beside the 60 fps game (7) | 21.94 / 24.82 / 30.01 | 23.61 / 36.11 / 40.40 |

Run means at 1440p fall into two groups, about 16.6 and 18.6 ms, for both
builds. Beside the game one rc.23 run averaged 32.2 ms; its host log is
healthy (claim wait at most 1.1 ms, encode p99 4.2 ms), so the time went in
the client's decoding next to the load, and without it rc.23 averages 22.2
ms against rc.22's 21.9. No regression is visible on the host side; every
run decoded all received frames without failures. Artifacts:
`bench-rc21\r23-*`.

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
