# Troubleshooting

[Documentation](README.md) · [Getting started](getting-started.md) · [Configuration](configuration.md)

Start with the current stream's settings, the console and the logs. Record the time of a failure before changing anything, and change one setting at a time. Capture and performance probes add work to the GPU; use an idle session for those comparisons.

**Find a symptom:** [Pairing](#moonlight-cannot-find-or-pair-with-the-pc) · [Console or port](#the-console-will-not-open-or-a-port-is-occupied) · [Black screen](#black-picture-no-display-or-wgc-fails) · [Audio](#audio-cuts-out-or-lags) · [Blurred PyroWave](#pyrowave-shows-blurred-grey-blocks) · [HDR colour](#hdr-looks-washed-out-too-bright-or-different-between-clients) · [Stutter](#low-latency-is-reported-but-motion-still-stutters) · [Playnite](#playnite-does-not-launch) · [RTSS](#rtss-does-not-start-or-the-game-ignores-the-cap) · [Display restoration](#monitors-stay-on-or-the-display-layout-does-not-return) · [Controllers](#a-controller-or-other-input-does-nothing) · [Steam Deck](#a-steam-deck-has-no-gyro-trackpads-or-back-grips) · [Updates](#an-update-stays-queued) · [Logs and support](#logs-and-a-useful-report)

## What the stream card warnings mean

The stream card shows the active encoder and warnings for the current session. Each warning also writes a `WARN` log with its cause and a next step. Capture recovery and audio capture warnings clear when those paths recover. Dropped packets, frames and input, and audio device changes, stay on the card for 30 seconds after they last happened. A warning describes a fallback or limitation, not a measured latency penalty.

- **Encoder fallback, software or CPU frame copies:** if AMF (AMD) or NVENC (NVIDIA) cannot start, Automatic uses another hardware encoder when one works, such as Quick Sync on an Intel integrated GPU, and says so on the card. It never falls back to software: with no working hardware encoder the stream fails with the AMF/NVENC error in the log. Check that error and the graphics driver. Software must be selected explicitly, preferably at a lower resolution and frame rate; CPU frame copies and software encoding can limit fps.
- **Graphics queue conversion/copies:** compute setup failed. A busy game can delay capture or conversion. Lower game GPU load or check the AMD driver.
- **Desktop Duplication / capture interrupted:** WGC or its helper could not capture. UAC and lock screens can cause a temporary change. Unlock the desktop; if the warning persists, check the helper error. A display size change causes capture to reopen.
- **Physical display, refresh, HDR or limiter:** the requested setup is unavailable, unsupported or owned by another stream. Check the actual display mode and RTSS. A 60 Hz source cannot supply 116 fresh pictures per second.
- **Audio:** a missing virtual sink can send sound to the host's speakers. Device changes and capture failures can pause sound. The host-loss warning names what was lost: sound Windows dropped because the host's audio sender read too late (a busy CPU or a host problem, not bitrate), or audio packets the network adapter refused (lower the video bitrate or check the adapter).
- **Bitrate, pacing or FEC:** the encoder/wire budget was limited, packets were dropped, or large frames lost FEC protection. Check Maximum bitrate and the client bitrate. Runtime bitrate updates have a 500 Mbps cap. Leave headroom for audio, packet headers and FEC.
- **Input:** the selected driver or Windows input API is unavailable. Video continues; repair the named driver or choose a supported profile.
- **PyroWave:** the client negotiated another codec, or bitrate/recovery protection is insufficient. Use a compatible Nonary Moonlight client and a fast wired network, or choose HEVC/AV1.

NVIDIA users should use [Vibepollo](https://github.com/Nonary/Vibepollo).

## Moonlight cannot find or pair with the PC

- Open **https://localhost:47990** on the host. If it does not open, check the host/service first.
- Confirm both devices are on the same local network, then try adding the host's local IP address in Moonlight. Guest networks or access-point isolation can prevent devices from reaching each other.
- Check **Settings → General → Allow pairing**. Enter the four-digit PIN **shown by Moonlight** into the matching pending request on **Devices**.
- If discovery alone fails, check **Settings → Network** for the discovery and bind-address settings. An address restricted to `127.0.0.1` is reachable only from the host itself.
- If setup reported a firewall-rule failure, read the setup log for the exact error and check Windows Firewall's allowance for the installed `butterpollo.exe` on your local network. Avoid disabling the entire firewall to diagnose one application.

If the device pairs but cannot launch Desktop, open **Devices → Edit** and check its enabled state and **List apps**, **View streams** and **Launch apps** permissions. A device missing mouse or keyboard permission can receive video without that input working.

## The console will not open, or a port is occupied

The default base port is `47989`; the console uses `47990`. A changed **Base port** moves the console to the next port up. Check the active profile's `sunshine.conf` if the launcher reports another address.

**Start Butterpollo.exe** reopens the correct console when that profile is already running. If it reports another streaming host on the port, close the conflicting Sunshine, Apollo, Vibepollo or second Rubylight instance. Restarting another copy on the same port does not solve the conflict.

For an installed host, check **Rubylight** in Windows Services. Its internal service name is `ApolloService`. If it is stopped, read `service.log` before starting it again. The [log locations below](#logs-and-a-useful-report) distinguish service and portable profiles.

## Black picture, no display, or WGC fails

1. On **Overview → Host readiness**, inspect **Screen capture**, **Video encoder** and **Virtual display**. A physical monitor must be active, or the configured virtual display must be available.
2. Check **Settings → Display → Display** and any app/device override. Confirm that the selected display is the one containing the desktop or game.
3. For service-mode WGC, keep a Windows user signed in. Rubylight starts its WGC capture worker in that user's session. A locked or UAC desktop uses the Desktop Duplication recovery path; WGC is retried when the normal desktop returns. A stream started while Windows is locked sets up its display on the lock screen; if the virtual display cannot be set up there, the stream shows the physical display and the stream card says so. Start the stream again after signing in.
4. Read the `capture backend opened` log entry for the backend that actually opened. `requested_capture=wgc` describes the request and can appear even when capture falls back.
5. If the virtual-display status reports access denied, use the installed Windows service and check that the bundled driver is ready. A portable host having administrator rights is not equivalent to the driver's service access.

For a controlled comparison, disconnect first, set **Settings → Video → Capture method** to **Desktop Duplication**, reconnect at the same resolution/rate, and record whether the symptom changes. Return to **Automatic** afterward unless the explicit choice is needed. Keep the original failure log so a successful fallback does not hide the WGC error.

## HDR looks washed out, too bright, or different between clients

Separate the host's HDR source, the negotiated stream and the client's display output:

- Check **Windows Settings → System → Display** for the **selected streamed display**. HDR on another monitor does not establish HDR on this one; wide-gamut SDR is also different from HDR.
- Check the client's HDR option and the Overview stream card's **HDR** badge. In the host log, `hdr=true`, the codec and `source_pixel=RgbaF16` identify a negotiated HDR stream with floating-point capture. FP16 capture alone does not prove that the game is producing HDR highlights.
- Start with **Settings → HDR → Display HDR: Match the stream** and **HDR request: Follow the device**. A display-only override does not change what the client negotiated. Check app/device overrides too.
- Confirm the game's HDR setting and the receiving display's HDR mode. For a comparison between two clients, keep the codec, scene and host display the same; AV1 HDR on one device versus HEVC SDR on another is not an equivalent check.
- Report whether black levels, mid-grey menus, bright highlights or colour saturation are wrong. Include the client app/version, device and display model, codec, stream resolution/rate and the host log time.

The Xbox HDR colour-appearance report remains under investigation. A TV detecting HDR10 does not by itself prove correct colour rendering. rc.10's native HEVC/AV1 tests verify FP16 capture, ten-bit BT.2020/PQ decoding and reference pixels; they do not calibrate the TV or validate every client's output. See the [native HDR evidence](../rust/PERFORMANCE.md#final-native-virtual-hdr-pixels-excluding-physical-panel-calibration).

## Audio cuts out or lags

Audio and video share the network path. On Wi-Fi, a video burst can fill the adapter or access point's queue even when the average bitrate looks reasonable. Lost HEVC or AV1 frames can then require a larger recovery frame.

1. Lower the client's bitrate and compare the same scene. On AMD, HEVC or AV1 is a better starting point for Wi-Fi than PyroWave. Compare a wired connection when possible.
2. Leave `pacing_max_bitrate_kbps` at `0` for automatic pacing. Conventional codecs use about twice the encoder bitrate only on confirmed Wi-Fi or mobile broadband routes. Ethernet, loopback, VPN, Tailscale and unknown routes keep the faster 800 Mbps default, subject to a known physical Ethernet link's speed. Hyper-V vSwitches use the bound physical adapter when Windows exposes an unambiguous binding; otherwise they keep the wired default. A host connected by cable cannot automatically account for Wi-Fi between the access point and client.
3. For that wired-host/wireless-client case, try a positive pacing override around twice the stream bitrate: for example, `pacing_max_bitrate_kbps = 120000` for a 60 Mbps stream. Save and reconnect. The setting is in kbps, has a floor of 110% of the stream bitrate, and does not reduce the stream's encoded bitrate. If loss continues, lower the client's bitrate too; pacing cannot create wireless capacity.

Compare Moonlight's loss statistics and the host log before and after. `reference_invalidations` counts reference-loss feedback separately from `idr_requests`; neither is a count of lost packets. PyroWave has no inter-frame references, so its recovery feedback does not force another encode or increase FEC. A steadily rising request counter alone does not establish Wi-Fi loss. [Pacing details](configuration.md#capture-and-video) and [transport evidence](../rust/PERFORMANCE.md#october-7-pyrowave-recovery-feedback) describe the limits of the local tests.

## PyroWave shows blurred grey blocks

A very low bitrate can leave PyroWave with only coarse brightness and little colour or fine detail. The old one-bit-per-pixel warning was too low for the measured desktop pictures. The stream card and log now distinguish `PyroWave bitrate is too low` (red, below the severe-loss floor) from `PyroWave bitrate is below recommended` (amber, text and textures may lose detail).

At 60 fps the floor/recommendation are **139/277 Mbps for 720p**, **187/399 Mbps for 1080p** and **747/1593 Mbps for 4K**, rounded up. These come from synthetic desktop and game tests on AMD, including SDR/HDR and 4:2:0/4:4:4; they are not a guarantee for every picture. The one-pixel text at 720p needed more bits per pixel than the larger text at higher resolutions. [Criteria, rates and limits](configuration.md#capture-and-video) explain the two levels.

Raise the bitrate in Moonlight on a fast wired network with headroom for packet overhead and recovery data. 4K60 at the recommended rate needs more than gigabit Ethernet. If the client or network cannot carry it, use HEVC or AV1. Compare the same scene after changing the rate; the warning does not diagnose packet loss or HDR rendering. NVIDIA users should use [Vibepollo](https://github.com/Nonary/Vibepollo).

## Low latency is reported, but motion still stutters

Compare the same moving scene at the same resolution, frame rate, codec and bitrate. A static desktop may legitimately send repeated or fewer frames. Begin with 1080p60 SDR, then change one variable.

| Statistic | What it helps distinguish |
| --- | --- |
| **Overview → Frame rate** | Frames sent by the host. Repeated pictures can still count toward this rate. |
| **Encode** (p95) | Slower conversion/encoding completions, including asynchronous encoder work. |
| **Host processing** | Time after the host claims a frame through preparation for transmission. |
| **Frame age** | Waiting from the capture/presentation timestamp until the host claims the frame. |
| Moonlight's network and decoder statistics | Loss, jitter or client decoding delays after host processing. |

None of the host timings measures the complete input-to-screen delay. Smoothness also depends on distinct pictures arriving regularly. The [performance guide](performance.md) separates host timings, measured picture age, fresh-picture rate and arrival gaps.

If the problem starts when the GPU is fully occupied, compare with a lower game frame cap or lighter graphics settings. If client network loss rises, reduce bitrate or compare a wired connection. If decoder time rises, reduce resolution/rate or select another hardware-decoded codec. Keep WGC and compute defaults for the baseline; use capture/compute overrides only for a recorded comparison. The published local tests do not establish results for every Radeon model or Wi-Fi connection.

## Radeon RX 9000 (RDNA4)

Reviewed on **7 October 2026**. These are public reports and a code review; Rubylight's local test GPU is an RX 7900 XT, so this does not establish RDNA4 stability or performance.

**Start with AV1 when the client can decode it in hardware.** AMD describes increased AV1 throughput and improved coding efficiency on RDNA4; Foundation's RX 9070 freeze reporter also found AV1 unaffected by their H.264/HEVC failure. Those are reasons to try AV1 first, not a promise that every AV1 configuration works. HEVC remains useful for clients without AV1 decoding. [AMD's architecture presentation, slides 3–4](https://hc2025.hotchips.org/assets/program/conference/day1/8_amd_pomianowski_final.pdf), [Foundation #666](https://github.com/AlkaidLab/foundation-sunshine/issues/666).

For a baseline, leave `amd_usage=ultralowlatency`, `amd_lowlatency_mode=auto`, `amd_input_queue_size=0`, `amd_preanalysis=disabled`, `amd_split_frame=auto` and `compute_queue_realtime=false`. Use `amd_smart_access_video=disabled` when diagnosing freezes. The usage preset and the separate `LowLatencyInternal` override are different settings. Zero input queue size leaves the driver's queue property untouched; it does not create a zero-length queue.

For a fresh driver comparison, start with AMD's current Recommended package for the card. **26.8.1** is the WHQL Recommended baseline and **26.9.2** is the newer Optional comparison, not a confirmed AMF-freeze fix. Avoid **26.5.x with forced H.264/HEVC low latency or a queue of one**: #666 reproduced on 26.5.2. There is no source-backed universally safe RDNA4 streaming driver or verified first-fixed version for that freeze. AMD's recommendation to use **26.3.1** in some later notes concerns Blender/game regressions, not a validated streaming workaround. [RX 9070 downloads](https://www.amd.com/en/support/downloads/drivers.html/graphics/radeon-rx/radeon-rx-9000-series/amd-radeon-rx-9070.html), [26.9.2 notes](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-9-2.html), [26.5.2 notes](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-5-2.html), [#666 diagnosis and correction](https://github.com/AlkaidLab/foundation-sunshine/issues/666#issuecomment-4531120037).

### What AMD's 26.x release notes actually say

The relevant recording, streaming and media mentions in the reviewed RX 9000 Windows releases are below. None names a fix for AMF `LowLatencyInternal`, `InputQueueSize`, SmartAccess Video, VCN streaming freezes, Moonlight or Sunshine. A removed known-issue entry alone does not establish a fix version.

| Driver notes | Relevant change or known issue |
| --- | --- |
| [26.1.1](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-1-1.html) | Fixes RX 9000 audio dropouts during minimized video playback on certain Xiaomi/Hisense TVs, and slow Vulkan window resizing on RX 7000/9000. Lists Battlefield 6 corruption with Record and Stream as known. These are not AMF encode-freeze fixes. |
| [26.2.1](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-2-1.html), [26.2.2](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-2-2.html) | Both retain the Battlefield recording issue. 26.2.2 fixes desktop corruption with Instant Replay and Record Desktop on **RX 7000**, not an RX 9000 AMF issue. |
| [26.3.1](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-3-1.html), [26.5.1](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-5-1.html), [26.5.2](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-5-2.html) | Continue listing Battlefield 6 corruption with Record and Stream. No AMF/streaming freeze fix is listed. |
| [26.6.1](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-6-1.html), [26.6.2](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-6-2.html), [26.6.4](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-6-4.html), [26.7.1](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-7-1.html) | Still list that recording issue. The 26.7.1 Smart Access **Memory** installation issue is not SmartAccess **Video**. |
| [26.8.1](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-8-1.html) | Fixes recording/replay failing to save without an audio output device on RX 9000 under Windows 10. Also fixes named games failing in multi-adapter configurations; this does not establish a capture/encoder adapter fix. |
| [26.9.1](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-9-1.html) | No encoding/streaming fix named. |
| [26.9.2](https://www.amd.com/en/resources/support-articles/release-notes/RN-RAD-WIN-26-9-2.html) | Fixes War Thunder DX12 timeouts with Instant Replay and Borderless Region Capture on **RX 6000**. Its RX 9000 HITMAN timeout/system-freeze fix is game-specific, not a stated streaming fix. |

### AMF and streaming-host reports

Issue closure is distinguished from a verified driver fix here. Settings from another host are not necessarily Rubylight settings.

| Source and affected case | Fix or workaround evidence |
| --- | --- |
| [AMF #587](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/587#issuecomment-3824327719), [#548](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/548#issuecomment-2810798182): RX 9000 engine count | AMD confirms one VCN shared by decode and encode. Do not infer two independent HEVC engines from “dual media engine” marketing. Background hardware video decoding and other encoding sessions can compete with the stream. |
| [AMF #548](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/548#issuecomment-2861639560): RX 9070 XT hardware decode/encode slowdown on 25.3.x | Reporter says **25.5.1** fixed the VCEEnc-specific performance problem. The shared-engine hardware limit remains. |
| [AMF #562](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/562): RX 9070 XT AV1 PreAnalysis backpressure on 25.6.1 | Fixed in FFmpeg and [OBS #12161](https://github.com/obsproject/obs-studio/pull/12161), with reporter confirmation. These are application buffering/settings changes, not a named AMD driver fix. Keep PreAnalysis off for the latency baseline. |
| [AMF #352](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/352): lookahead stalls | A later RX 9070 report on 25.3.2 still describes AV1 lookahead-depth-40 stalls after the older issue was closed. No RDNA4 first-fixed driver is established. Rubylight's optional PA requests one frame, not 40. |
| [AMF #423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423#issuecomment-2729665337): AV1 padded dimensions | AMD identifies an RX 9000 hardware fix for the older alignment limitation. Do not force an RDNA3 1080-to-1082 workaround onto RDNA4. On an RX 7900 XT with driver 32.0.31041.1004 (October 2026), 1968×2184 still encodes as 1984×2186 despite the no-restrictions alignment mode, with no render size in the frame header, so only clients that crop to the negotiated size (Moonlight-qt does) hide the padding. The host logs a warning once per stream at such sizes. Writing a render size into the header was prototyped and decodes identically, but FFmpeg ignores it, so it is not shipped ([measurements](../rust/PERFORMANCE_WORK.md)). |
| [AMF #540](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/540): RX 9070 XT HEVC range differences, initially 25.3.1 | Still open; later observations differ between players/editors. No confirmed first-fixed driver. Check limited/full range and decoded colour, not just the HDR badge. |
| [AMF #605](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/605), [#610](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/610): RX 9070 XT filter interop/capabilities | Reports use Windows drivers 32.0.31021.5001 and 32.0.31041.1004. Shader-readable textures work around #605; #610 is marked fixed in a future release without a public version. These concern AMF filters, not Rubylight's shader converter. Successful `Init` alone is insufficient evidence of working output. |
| [Foundation #666](https://github.com/AlkaidLab/foundation-sunshine/issues/666#issuecomment-4531120037): RX 9070, 26.5.2, frozen H.264/HEVC picture while audio/input continue | Initial discussion mentions queue size one; later diagnosis identifies an unconditional `LowLatencyInternal=true` write that overrode the UI. Candidate removal shipped in **v2026.525.100901.杂鱼**. The issue does not isolate queue size as the sole cause or prove a firmware root cause. AV1 worked for the reporter. |
| [Foundation #675](https://github.com/AlkaidLab/foundation-sunshine/pull/675), [#702](https://github.com/AlkaidLab/foundation-sunshine/issues/702): long-running HEVC and recurring RX 9070 XT stalls | #675 merged HEVC GOP=60 on 25 May to close #666. #702 subsequently reports stalls on 26.5.2 even with forced low latency disabled; a maintainer suggests `lowlatency_high_quality`, without confirmation. These are host changes/suggestions, not driver fixes. |
| [Foundation #1098](https://github.com/AlkaidLab/foundation-sunshine/issues/1098): static-scene oversized AV1/HEVC frames, RX 9070 XT, 32.0.31041.3013 | Reporter found the fork's FFmpeg compatibility mode plus CBR/HRD useful. [#1117](https://github.com/AlkaidLab/foundation-sunshine/pull/1117) proposed frame-sized VBV but was **closed without merging** after its author could not reproduce the original problem and saw periodic quality loss. No driver fix established. |
| [Vibepollo #354](https://github.com/Nonary/Vibepollo/issues/354): RX 9070 XT, 26.6.4, HDR AMF error 10 with QVBR/HQ modes | Fixed in **v1.18.3-beta.6** by disabling PA and changing incompatible rate control for ten-bit sessions. Application fix; `vbr_latency` with PA off was the confirmed workaround. Rubylight already demotes these modes for HDR. |
| [Vibepollo #417](https://github.com/Nonary/Vibepollo/issues/417): RX 9060 XT plus iGPU, 26.7.1, black video on macOS | Phone worked; suggested legacy encoding did **not** help. Closed without a documented driver fix. Include the client and both adapters when reporting a similar failure. |
| [Sunshine #4065](https://github.com/LizardByte/Sunshine/issues/4065): RX 9070 startup timeout, plus RX 9060 report on 25.11.1 | Delaying host startup or changing Fast Startup helped some reporters but not all; still open. The original report's “23.3.1” comparison predates these GPUs and is not a usable RX 9000 recommendation. |
| [Sunshine #5321](https://github.com/LizardByte/Sunshine/issues/5321), [#5399](https://github.com/LizardByte/Sunshine/issues/5399): RX 9070 XT resets on 26.6.2; RX 9070 incomplete frames on 26.6.4 | Neither establishes a driver fix. #5321 has a short successful retest after Windows/host changes and later recurrence reports. #5399 was closed without confirming its proposed packetization diagnosis. |

### What the Rubylight review covers

| Path | Finding and remaining limit |
| --- | --- |
| `amf.rs`, `encoder_policy.rs` | `LowLatencyInternal` and input queue size are opt-in. The Rust guard now disables explicitly requested SmartAccess Video when H.264/HEVC forced low latency is also enabled, and requires the driver to accept that disable. This restores the safeguard recorded in the [earlier C++ backend](https://github.com/RamazanKara/Butterpollo/blob/2.0.0-rc.23/src/amf/amf_lifecycle.h). It does not make forced low latency alone safe on RDNA4. No driver gate is guessed from an AMF runtime version or issue closure. Rate-control policy is unchanged. |
| B-frames and HEVC GOP | H.264 and AV1 request `BPicturesPattern=0` through their codec prefixes; the pinned HEVC SDK has no such property. Effective supported values are logged. HEVC GOP remains zero with requested recovery keyframes. Foundation's periodic-IDR workaround above conflicts with that policy and needs an RDNA4 long-run comparison before changing it, especially with LTR recovery and frame-size limits. |
| `amd_split_frame`, `ENCODER_BACKLOG` | The hint queries codec capabilities and writes nothing for one or unknown engine count, even for On/Off. The host's backlog limit of **two frames** pipelines submissions to one encoder; it is not an engine count and does not set AMF's input queue to one. An encoder holding a full backlog that returns nothing for 250 ms is recreated; each further silent recreation doubles the wait, up to 2 s, and the session gives up after 20 s. A driver returning valid-looking repeated/skip pictures can evade a no-output watchdog. |
| `compute.rs`, capture | Devices are selected by adapter LUID, shared textures/fences stay on that adapter, and CPU fence waits have a two-second bound. Device removal's `UINT64_MAX` fence value now errors instead of permitting allocator/texture reuse; the shared compute cache rejects a removed device. This follows [Microsoft's fence contract](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12fence-getcompletedvalue). Queue creation falls back from High to Normal; global realtime is off by default. |
| `pyrowave.rs` | Vulkan selection uses the capture LUID and checks texture/fence interop support. Colour conversion can fall back to D3D11. The synchronous PyroWave encode/destroy calls have no host-enforced timeout: a hung driver call can outlast stream recovery. There is no safe thread-cancellation patch for that here; process isolation would be a separate change. |
| `codec_probe.rs`, `gpu_priority.rs` | Optional PyroWave probing runs in a child with a 15-second deadline and owned-process cleanup. It tests output, but short probes cannot establish long-run stability. GPU process priority applies to Rubylight only, with a denied realtime request falling back to High; no evidence supports inventing an RDNA4-specific priority rule. Process scheduling class and D3D12 global-realtime queue priority are separate. |

### RX 9070 XT test checklist and logs

1. Record Rubylight/client versions, Windows build, **Adrenalin version and Windows DriverVersion**, GPU names, selected display/adapter, physical or virtual display, HDR/VRR, resolution/rate and bitrate. On dual-GPU machines include the second adapter and which GPU owns the display. Read-only driver inventory: `Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion`.
2. Start one wired **1080p60 SDR AV1** session with the baseline above. Run a moving scene and leave a menu/desktop still for several minutes. Repeat with HEVC, then H.264, for **at least ten minutes each**; also run the normal gaming workload for **30–60 minutes**. Record whether audio/input continue during any frozen picture and whether reconnecting restores video. Do not deliberately re-enable the reported freeze/reset combinations.
3. If those pass, compare AV1 and HEVC at the actual target resolution/rate, first SDR then HDR, including a dark/grey/bright chart and text. Include **1920×1080** when checking AV1 dimensions. Keep the scene, bitrate and client identical. High-rate 4K and simultaneous streams are separate tests, not assumed single-engine capacity.
4. For a failure or performance comparison, change one existing setting per reconnect: WGC versus Desktop Duplication; then `gpu_compute_conversion=true` versus `false` with capture held constant. Leave global realtime off. Run **A/B/A/B**, at least three minutes per leg, and report the range of each leg's timings rather than only the best run. Note other streams, GPU benchmarks, recording or video playback competing for the GPU.
5. On a dual-GPU host, repeat with the RX 9070 XT explicitly selected and confirm the intended display. Test disconnect/reconnect and a normal display-mode change; record capture/encoder recreation. If testing PyroWave, first establish a working AV1/HEVC baseline and use the [required bitrate](#pyrowave-shows-blurred-grey-blocks).

Send the host log **from `CLIENT CONNECTED` through `CLIENT DISCONNECTED`**, plus startup/probe failures and the client log around the failure time. Use `min_log_level=debug` for a diagnostic run if the priority/probe details are needed. Preserve whole lines, including fields. In particular include:

- `stream configured`, `capture backend opened`, `compute queue ready for colour conversion` and, at debug level, `GPU process scheduling configured`.
- `AMF encoder settings`, `AMF split-frame encoding left to the driver` or `AMF split-frame encoding requested`, and every `AMF setting unavailable` / `AMF split-frame request not accepted` warning. The HEVC engine field should report `HevcNumOfHwInstances=1` on the reported hardware; send the actual value. Include `LowLatencyInternal`, `HevcInputQueueSize`, `HevcEnableEncoderSmartAccessVideo`, `HevcEnablePreAnalysis`, and, for AV1, `Av1InputQueueSize`, `Av1EnableEncoderSmartAccessVideo`, `Av1EnablePreAnalysis` and `Av1BPicturesPattern`. `?` means a property could not be read, not that it is disabled.
- `AMF SmartAccess Video disabled because forced low latency can reset the GPU` if an imported configuration triggered the guard.
- All `stream timings` lines, especially `fps`, `encode_p95_ms`, `encode_p99_ms`, `frame_age_p95_ms`, `send_interval_p99_ms`, `idr_requests`, `reference_invalidations` and `bitrate_kbps`, alongside Moonlight's network loss and decoder statistics.
- Any `D3D12 device removed; GPU fence completion is invalid`, `GPU work did not finish within two seconds`, `capture copies stopped completing`, `Encoding failed (…); recreating the same encoder`, `Encoder output failed (…); recreating the same encoder`, `The encoder returned no frame for … ms; recreating the same encoder` or `Encoder recreation failed` lines. Include `AMF compute conversion unavailable (…); converting on the graphics queue` and `PyroWave compute conversion unavailable (…); converting on the graphics queue` if present.
- For PyroWave, include `optional codec probe` errors; for an OS/driver reset, include the matching Reliability Monitor/Windows event time and LiveKernelEvent code. A frozen picture with continuing `stream timings` and no recovery warning is useful evidence too.

## Playnite does not launch

Keep a Windows user signed in, enable Playnite in **Settings → Game library**, and check that the game starts in Playnite locally. If Rubylight cannot find a portable copy, leave Playnite open in that user's session when starting the stream.

In **Library**, check Playnite's plugin status. Use **Install plugin** or **Update plugin** if offered, fully exit and reopen Playnite, then try **Sync now**. Use the connector bundled with Rubylight; newer Vibepollo connectors may require different host support. A `Playnite CLI fallback requested` warning means automatic game-exit tracking is unavailable; quit that stream manually.

Send the failure time, installed or portable location, Desktop or Fullscreen mode, and the host log from `Playnite launch prepared` and `Playnite plugin check` through the pipe, startup, fallback or exit messages. Include any `Moonlight session launch failed`, `Playnite library sync failed`, `Playnite startup failed`, `Playnite did not confirm startup` or `Playnite game exit confirmed` lines. If available, include Playnite's `playnite.log` and the matching `%APPDATA%\Sunshine\logs\sunshine_playnite-*.log`.

## RTSS does not start, or the game ignores the cap

Open **Maintenance → Frame limiter** during the affected stream. Record **Configured**, **Active now**, RTSS detection/running state and **RTSS folder**. A missing tray icon alone does not establish whether the limiter is active.

In **Settings → Frame limiting**, check **Limiter**, **Frame limit**, **Virtual display refresh** and **RTSS folder**. **None** disables limiting. Physical-monitor streams need **Limit every stream** if you want a cap on every stream; virtual-display policy can apply its own cap. A frame limit of `0` follows the stream rate, including fractional values such as 59.94.

The folder must contain the RTSS executable and its hook library, not just a shortcut. If the log says RTSS requires administrator privileges, start RTSS as administrator before streaming or use Rubylight's installed service. If it is found and running but one game ignores the limit, check that game's RTSS profile and include the game/API in the report. If RTSS's window keeps your own limit during a stream, Rubylight could not reach the elevated RTSS: use the installed service, or run a portable copy as administrator. RTSS 7.3.7 ignores edits of its settings file and takes a new limit only through its own interface, which Rubylight uses from rc.27; the log line "RTSS frame limit applied and verified" names the RTSS version.

Rubylight restores limiter values after the last stream owning the limit disconnects. A game retaining its display must not keep the cap active. If a cap remains, check for another pending or connected stream and keep the before/during/after values with the log.

## Monitors stay on, or the display layout does not return

Check **Settings → Display** and app/device overrides first. **Extended** intentionally keeps physical displays alongside the virtual display. Exclusive and isolated layouts have different behavior. A client may also request its own virtual-display arrangement.

Check **Restore displays on disconnect**, **Restore delay** and **Keep a disconnected display for** against the expected lifecycle. An app or retained remote-monitor session may still own a display after video disconnects.

When all streams have ended, use **Maintenance → Displays** to inspect whether the current layout matches the saved one. **Restore saved layout** applies that saved layout; **Save current layout** replaces it with the arrangement you currently want. Saving, restoring and resetting are unavailable during streaming.

**Disconnect virtual displays** stops every stream and removes Rubylight-created virtual displays. **Reset display settings memory** forgets pending display changes that Rubylight would otherwise undo. Use those recovery actions deliberately after recording the problem; resetting memory is not the same as restoring a layout.

rc.10 has a brief startup guard for a reproduced case where creating a virtual display reactivated a dormant monitor. That guard is not continuous enforcement and does not establish that every phone/client display report is fixed. Record the client, chosen layout, active monitors and log time when reporting another case.

## "Virtual display did not become active before the deadline"

Up to rc.10, a launch failed with this error when Windows left the new virtual display switched off, typically because it recalled a layout saved for the other connected displays (two TVs in duplicate mode, for example). rc.11 switches the display on itself after a second, without changing the other displays, and logs `Windows left the new virtual display switched off; switched it on beside the current displays`. Update before trying workarounds such as unplugging the TVs. If a launch still fails, the error says whether Windows reported the display as connected; include that line and the log around it in the report.

## The stream disconnects after a second with "os error 10035"

`video sender stopped: A non-blocking socket operation could not be completed immediately. (os error 10035)` comes from rc.1, which ended the stream when the Windows send buffer was momentarily full. Since rc.2 the host waits briefly, drops only those packets and keeps streaming, as Vibepollo does; the log then shows `UDP send failed; packets dropped` at most every five seconds. Install the current release. Frequent drop warnings mean the network cannot carry the bitrate: lower it, or use a standard codec rather than PyroWave over Wi-Fi.

## A controller or other input does nothing

Check the stream card in the console first.

- **"…its device permissions do not allow it"**: the device lacks that input permission. Every device paired after the first starts view-only, as in Apollo and Vibepollo. Turn on **Controllers** (or Touch, Pen, Mouse, Keyboard) for it under **Devices**, then reconnect.
- **"Virtual gamepad driver unavailable"**: the VHF gamepad driver could not be opened. Run the installer again with the gamepad driver selected.

## Steam shows two controllers

Some Steam builds can list one VHF Xbox controller twice. Steam's SDL controller discovery races its XInput and GameInput backends for the same device. Start+Select may then open both Xbox Game Bar and Steam's keyboard. SDL has an [upstream fix](https://github.com/libsdl-org/SDL/commit/c4cfb739). Users report that the Steam beta, which includes it, lists the pad once; switch to it under Steam → Settings → Interface → Client Beta Participation. The fix will reach Steam's stable client in a later update.

Steam's `logs/controller.txt` shows each arrival. Rubylight no longer uses ViGEmBus, which earlier releases offered as a workaround; an installed copy can be uninstalled.

## A Steam Deck has no gyro, trackpads or back grips

The host only follows what Moonlight announces when a controller connects. With Steam Input on for Moonlight, Steam on the Deck gives Moonlight a virtual pad, which has no gyro, trackpads or back grips. On the Deck, disable Steam Input in Moonlight's controller settings in Steam, then reconnect. The stream card says this when the paired device is named Steam Deck and its controller has no gyro. In Rubylight's logs, a Deck that Moonlight sees directly connects with `client_type=4`.

- **Steam on the host doesn't show a Steam Deck:** the host attaches one only with usbip-win2 installed and, with **Automatic**, while Steam is running on the host when the controller connects; otherwise the log says `Steam is not running` and the Deck gets a virtual DualSense. The log line `Steam Deck controller attached through usbip-win2` names the usbip-win2 port, and `usbip.exe port` lists it. If the stream card says the attach failed, its message is usbip.exe's own.
- **No trackpads with Steam Input off:** Moonlight builds whose SDL predates SDL 3 do not read the Deck's trackpads, so they send none.
- **Back grips do nothing on the virtual pad:** choose what each grip presses under **Settings → Input → Steam Deck and back grips**; the virtual pads have no back buttons of their own. A Deck attached as a Steam Deck keeps its own grips, and Steam Input on the host decides what they do.
- **A Steam Deck controller left behind after a crash:** usbip-win2 detaches it when Rubylight's connection closes, then keeps trying to attach it again. `usbip.exe attach --stop-all` stops that.

See [Steam Deck](configuration.md#steam-deck).

## An update stays queued

In **Maintenance → Updates**, read the current phase and any error. Installation needs the installed Windows service and one minute without streams, pending connections, remote monitors or host apps. Quit a retained Desktop/game session as well as disconnecting its video. A new connection defers the update.

**Install updates automatically** is opt-in. **Include pre-releases** controls whether candidates appear, and an update-check interval of `0` disables automatic checks and installation. Manual checks remain available. Portable builds use the release-page download.

## Logs and a useful report

Use **Logs → Download log** for the host log or **Download support bundle** for a ZIP containing logs, configuration diagnostics and an available crash dump. Review the contents before posting publicly.

| File | Default location |
| --- | --- |
| Installed host log | `%PROGRAMDATA%\Butterpollo\config\logs\butterpollo.log` |
| Installed service log | `%PROGRAMDATA%\Butterpollo\config\logs\service.log` |
| Portable host log | `%LOCALAPPDATA%\ButterpolloRust\config\logs\butterpollo.log` |
| Setup log | `%TEMP%\butterpollo-setup-<timestamp>.log` |

A custom `--config-dir` changes the profile location; `log_path` can override the host log path. The support-bundle download is named `butterpollo-support.zip`.

The release includes `tools\collect_environment.ps1`. From PowerShell in the installed or extracted package folder, collect read-only environment evidence with:

```powershell
.\tools\collect_environment.ps1 -InstallDirectory . -OutputFile "$env:TEMP\butterpollo-environment.json"
```

Choose a new output filename if that file already exists. The collector records Windows, GPU/driver, physical network-adapter counters, host version/hash and service state. It does not start capture, change settings or upload the report. A source checkout has the same script at [`rust/tests/collect_environment.ps1`](../rust/tests/collect_environment.ps1).

For a display-specific report, `butterpollo.exe --diagnostics` prints the detected displays and virtual-driver status without starting a stream. In a portable/user context, its driver-access result may differ from the installed service.

Open an [issue](https://github.com/RamazanKara/Butterpollo/issues) with the exact symptom and time, host/client versions, GPU and driver, wired or Wi-Fi connection, resolution/FPS/codec/HDR settings, and the relevant log or support bundle. Include the environment report when the result seems hardware-specific. [Compatibility](../rust/PARITY.md) and [Performance](../rust/PERFORMANCE.md) show what has already been tested.
