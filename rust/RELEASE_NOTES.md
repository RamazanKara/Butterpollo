# Butterpollo 2.0.0-rc.12 release candidate for Windows

[Documentation](../docs/README.md) · [Install and migrate](../docs/getting-started.md) · [Configuration](../docs/configuration.md) · [Compatibility](PARITY.md)

**Release history:** [rc.12](#new-in-rc12) · [rc.11](#new-in-rc11) · [rc.10](#new-in-rc10) · [rc.9](#new-in-rc9) · [rc.8](#new-in-rc8) · [rc.7](#new-in-rc7) · [rc.6](#new-in-rc6) · [rc.5](#new-in-rc5) · [rc.4](#new-in-rc4) · [rc.3](#new-in-rc3) · [rc.2](#new-in-rc2)

Butterpollo's host, native helpers, service and setup are written in Rust, with a Svelte web console. The rc.12 installer is named `butterpollo-setup-2.0.0-rc.12.exe` and upgrades an existing Vibepollo or Butterpollo installation in place, keeping settings, paired devices, the app library and covers. Codec SDKs and Windows drivers remain external components; the setup installs the drivers.

## New in rc.12

- **Fixes HEVC on Radeon RX 6000 cards** ([#5](https://github.com/RamazanKara/Butterpollo/issues/5)). The RX 6800 XT's HEVC encoder rejects the input colour-range property, which the host treated as fatal: AMF was dropped and the stream stayed black. The range properties are now optional; limited range is AMF's default, and a full-range stream logs a warning if the driver refuses them. This affected every release since rc.1.
- **Fixes doubled and repeating input** reported on rc.10:
  - `key_repeat_delay = 0`, which a Vibepollo profile uses to turn host key repeat off, made the host repeat a key immediately, so one tap typed two or three characters. 0 now turns repeat off and a negative value keeps the 500 ms default, as in Vibepollo.
  - A repeated press of a key or button that is already held is no longer sent to Windows again; with the host's own repeat it doubled the repeat rate, and a second button-down could become a double-click.
  - A key released without the extended-key flag it was pressed with is now released, instead of staying down and repeating.
  - An Xbox controller stays an Xbox pad even when it reports motion sensors or a touchpad, as in Vibepollo. A PlayStation pad is also claimed by Steam Input, so games could see the controller twice.
  - rc.11 already stopped discarding delayed key releases as replays.
- Input that arrives before capture starts is no longer dropped when the stream's display is selected by device ID, as the console stores it.
- Other devices' requests no longer wait for a launch to finish preparing its display, audio and app.
- Adds a display self-test for support and development: run as SYSTEM, `butterpollo-service.exe --display-self-test REPORT.json` creates virtual displays, switches one off and back on, duplicates two of the driver's permanent displays as stand-in TVs, starts a stream display beside them, restores a cloned layout, and then puts the desktop back.
- Measured, not changed: on an RFI-capable client under bursty loss, long-term reference recovery on AMD (`amd_ltr_frames`) reduced the worst arrival spikes at heavy loss but produced more frame gaps than keyframe recovery, so it stays off by default. Wired 1080p60 at 80 Mbps showed no difference between the default packet pacing and a limit of twice the bitrate. For a client on Wi-Fi, setting `pacing_max_bitrate_kbps` to about twice the stream bitrate smooths the bursts.

## New in rc.11

- **Fixes "virtual display did not become active before the deadline"** ([#4](https://github.com/RamazanKara/Butterpollo/issues/4)). Windows decides whether a newly connected display joins the desktop, and a layout it saved for the same displays (for example two TVs in duplicate mode) can leave the virtual display connected but switched off. The host waited for Windows and failed the launch after ten seconds; unplugging the other displays was the only workaround. When the virtual display stays off for a second after connecting, the host now switches it on itself beside the current displays, as Vibepollo's display helper does. The other displays keep their modes, positions and clone groups; only if the driver refuses that does Windows choose the modes, which the stream's layout restore undoes. A launch that still fails now says whether the display was connected but kept off.
- **Codecs no longer stay unavailable after Windows starts.** The host checks its encoders once at startup. When it started with Windows, the PyroWave check could run before anyone was signed in (`helper needs a signed-in user`, or `optional codec probe exited before replying (0xc0000142)` while the desktop was still being set up), and on this test PC the AMD encoder itself failed to open (`AMF error 1`), so the host offered no video codec at all until it restarted. If no standard codec is found, the host now probes again once a user is signed in, with growing pauses and at most six times; a PyroWave check that failed only for want of a session is repeated the same way, never during a stream. A probe that fails for any other reason, such as a crashing overlay, is not repeated.

**Display layouts and recovery**

- Restoring a saved layout with duplicated displays (two cloned TVs, for example) no longer fails every time. Clone groups are rebuilt before positions, and displays are found by device instead of by a Windows display name that changes when they rejoin their group.
- A setting that cannot be restored after an interrupted stream no longer stops the host from starting. Recovery attempts every step, logs what failed and continues; before, the same failure repeated on every start.
- A TV or monitor that is off or unplugged when a stream ends keeps its HDR, colour profile and mode entries until it is back, instead of keeping the stream's settings.
- The layout restored after a stream is taken before the virtual display is created, so what Windows changes when it arrives (a TV switched off, another primary display, a retimed display) is undone too.
- A display that disappears during a stream no longer keeps the rest of the layout from being restored.
- Displays that stay on keep their timing and position when the layout changes, such as a retained remote monitor during a restore.
- An expired startup guard on a virtual display no longer makes its recovery fail for the rest of the session.
- A TV that takes a resolution but refuses the refresh rate is put back after a failed launch.
- Disconnecting a remote monitor can no longer hang the host while its display is being recovered.

**Streaming**

- The control and input connection survives network errors such as a Wi-Fi roam, an ICMP reply or an oversized datagram. Before, its worker stopped and every later session had no input or control until the host restarted; it now also restarts itself.
- Controller and motion input get their own channels again (Moonlight asks for 48) instead of sharing one with keep-alive messages.
- Delayed keyframe requests and key releases are no longer discarded as replays behind many motion reports (a 4096-message window instead of 64).
- Moonlight's reference frame invalidation requests are read correctly instead of each becoming a full keyframe.
- A frame too large for Moonlight's packet format at very high bitrates is dropped and followed by a keyframe, instead of ending the session.
- Audio delay no longer grows after a scheduling hiccup: a backlog over 30 ms is trimmed back to two packets.
- PyroWave honours the configured pacing limit (`pacing_max_bitrate_kbps`), which helps a client behind Wi-Fi, and no longer sends a catch-up burst after a send had to wait. Wired clients keep pacing at 95% of the host's Ethernet link.
- The video clock wraps instead of freezing after 13 hours.

**Moonlight compatibility**

- Joining or resuming a running game from Moonlight for Android or iOS works: `/launch` always answers with a game session.
- A wrong PIN no longer blocks pairing for every Moonlight device until the host restarts.
- A device that forgot this PC can pair again; it was refused as a duplicate until removed in the console. Vibepollo profiles that list a device twice now load.
- Quitting a game that is slow to exit no longer stalls other devices' requests.
- Odd client resolutions such as 2556x1179 are rounded down instead of refused.
- A device with the launch permission may resume its own game, as in Vibepollo.
- The app list reports HDR only when a codec has a 10-bit mode.

## New in rc.10

- **Correct HDR state on newer Windows versions.** Wide-gamut SDR color management is no longer mistaken for HDR support or active HDR. The host uses Windows' dedicated HDR state and toggle APIs where available, retains the legacy path on older systems, waits for the requested state, and cancels its own pending request after a failed transition. The capture and pacing defaults from rc.9 remain unchanged.
- Restores previously inactive displays when Windows reactivates them during owned virtual-display startup and recovery. A brief guard removes only reactivated targets with the same identity, retains current modes and clone relationships, rejects an observed concurrent layout change, and expires after startup. It does not continuously enforce the desktop or establish that the reporter's phone issue is resolved.
- Verified native virtual-HDR pixels with HEVC and AV1 at 720p60 on the final RX 7900 XT host. Both captured FP16, decoded every frame and passed the unchanged color/freshness gates, with display restoration confirmed. HEVC had no long arrival gaps; AV1 had 31 intervals above 25 ms despite complete picture coverage. This validates the tested pixel path and reproduced display fix, not gap-free AV1 delivery, physical-panel calibration or client scanout.
- Adds a read-only environment collector under `tools` in the portable package. It records the exact host hash, GPU and driver versions, Windows build and network-adapter type without changing settings or uploading anything. This makes hardware-specific reports reproducible; it does not replace a test on the reporter's machine.
- Validates DS4 and DualSense touch input through the installed signed driver: production packet decoding, two contacts, movement, release, cancellation, isolation of unsupported secondary pads and device cleanup. A true second touch surface still needs a new signed driver and device profile.
- Resolves the previous CLI test limitation. The unmodified Moonlight PC 6.2.0 client successfully lists apps and quits normally against rc.9 when its window and polling lifecycle are allowed to complete. Cold-cache CSV listing remains a separately reproduced client artwork-shutdown issue; plain listing or cached artwork is the workaround. An optional client source patch under `compatibility/moonlight-6.2.0` passed eight focused Qt tests; it still requires a custom client build and Windows validation.
- Keeps queue draining disabled after another saturation comparison: it reduced frame age but delivered fewer distinct pictures and more gaps. No additional performance setting was enabled on that evidence.

Current automated validation: 263 ordinary tests passed, with 27 environment-dependent tests excluded by default; formatting, Clippy with warnings denied and release builds passed. This includes eleven HDR-state tests and six virtual-display hotplug tests. Hardware test scope and package verification are recorded separately in PERFORMANCE.md and the release's VALIDATION.json.


## New in rc.9

- **Automatic capture now prefers Windows Graphics Capture.** This applies to physical and virtual displays. Existing explicit capture choices are preserved, including imported `dxgi` and `wgcc` aliases. WGC uses the signed-in-user capture worker under the Windows service, with Desktop Duplication fallback when WGC cannot open and during lock/UAC desktop transitions. Compute copies remain enabled by default on supported AMD GPUs.
- **Smoother 60 FPS delivery with lower measured picture age.** WGC-selected streams now align encoding to a stable source update when capture reports surplus updates. The guard falls back to ordinary pacing for irregular or slower sources and resets on recovery; explicit Desktop Duplication keeps its previous pacing default. In controlled local 720p60 AV1 runs, guarded pacing delivered 59.862–60.000 distinct FPS versus 57.650 and reduced estimated source-presentation-to-software-decode age by about 3.8 ms on average. This excludes remote network transit, client display scanout and input latency. `frame_pacing_source_phase=false` restores the previous WGC pacing for comparison.
- Removes the automatic 1 ms WGC update limit above 60 FPS. Explicit zero restored approximately 121 FPS delivery in the earlier controlled 120 FPS comparison; an explicit override retains the previous interval for diagnosis. Loaded 120 FPS and slow-source checks passed with the phase guard inactive. An uncapped GPU stress test still fell to about 53–54 distinct FPS with either pacing setting, so this is not a cure for GPU saturation. See [PERFORMANCE.md](PERFORMANCE.md) for the complete comparisons and failed cases.
- Avoids sending an unchanged frame just before a predicted fresh capture when a high minimum frame rate is configured. The wait has a fixed, short deadline; slower and static sources continue receiving repeats. In the local HEVC comparison with a 60 FPS minimum, fresh delivery improved from 57.700 to 59.793–59.860 FPS. The regular minimum-rate setting also passed at 60 and 120 FPS.
- **Fixes H.264 decoding with Moonlight PC 6.2.0.** Its Windows D3D11 decoder requests one reference frame. AMF previously enabled four long-term reference slots independently, causing repeated decoder failures and about 52.8 decoded FPS from a 60.3 FPS incoming stream. AMF now reserves long-term references only within the negotiated budget and uses IDR recovery when that budget is one. An incompatible H.264 intra-refresh request also falls back to IDR recovery instead of raising the client's limit. Clients with an unrestricted budget retain long-term-reference recovery. The FFmpeg NVENC fallback now sets its retained-picture buffer limit explicitly; NVIDIA execution still needs hardware validation.
- **Tested the exact released Moonlight PC 6.2.0 application.** Ten local 1280×720/60 sessions on the RX 7900 XT covered two connections each for H.264, HEVC, AV1, HEVC HDR and AV1 HDR. All selected the expected formats and D3D11 hardware rendering. Incoming and decoded rates matched, rendering remained about 60 FPS, and there were no decoder errors. Streaming windows closed and disconnected normally, and reconnecting worked. These are compatibility checks, separate from the distinct-picture and latency fixture.
- HDR requests and ten-bit codec decoding passed from an SDR desktop. This matrix does not validate native HDR source capture, displayed HDR brightness or colour accuracy. Stock Moonlight 6.2.0 supports the standard codecs; PyroWave and the host's 1000 Hz low-latency VRR mode still require [Nonary's client](https://github.com/Nonary/moonlight-qt). Moonlight's own client-side VRR rendering is a separate feature.
- Three additional AV1 checks verified Moonlight 6.2.0's existing AMD-padding compensation: 1080p SDR/HDR and 1968×2184 SDR. Client logs confirm the requested crop, hardware decoding and rendering without decoder errors. The underlying AMD bitstream still has padded dimensions; these checks verify the client's crop path, not pixel-edge or HDR display accuracy.
- Handles Moonlight PC 6.2.0's extended-key modifier, distinguishing keypad Enter from ordinary Enter through holds, repeats, release and disconnect cleanup. Existing key remaps and older clients retain their behavior. Controller touch parsing and batching now preserve the touchpad index. The bundled driver supports one touch surface with two contacts; secondary-pad events are safely ignored with a warning and cannot corrupt primary-pad state. Native second-touchpad support remains unavailable. Input was disabled during the streaming matrix, so live input on that application has not been newly validated.
- Follow-up checks on the unchanged rc.9 host verified natural exit of official Moonlight 6.2.0's plain `list`, CSV listing with cached artwork, three successive app quits and an idle quit. Paired HTTPS `/serverinfo` and the web API agreed that each app had stopped. The earlier quit failure came from the harness hiding its window and allowing too little time for its next status poll. Cold-cache `list --csv` still has an upstream artwork-shutdown bug, reproduced without Butterpollo; use plain `list` for automation or CSV after artwork is cached. Streaming-window close and reconnect evidence remains separate.
- Validation of the final runtime passed 246 ordinary tests, with 26 environment-dependent tests excluded by default, and Clippy with warnings denied. Four selected native checks separately passed: WGC reconnect/COM teardown, a 64-frame H.264 one-reference SPS/VUI and decode check, packet-loss reference recovery for the three AMD codecs, and exact GPU texture comparison. The ten exact-client connections and three crop checks above are additional checks. Final default-path motion results and capture-worker recovery are recorded separately in PERFORMANCE.md; package integrity and build provenance are recorded with the release assets.

## New in rc.8

- Fixes "adding the firewall rule failed" when reinstalling after an automatic update. Internal extended-length paths no longer leak into Windows installation entries or firewall commands. Setup updates an existing rule in place and removes legacy allowances only after Butterpollo's rule succeeds, so a rejected replacement keeps the previous rules.
- Fixes RTSS automatic startup when Windows requires administrator privileges (error 740 / `0x800702E4`). The installed service first tries a normal user launch, then retries that specific error with the signed-in user's elevated token. RTSS stays in the user's desktop session; the SDK helper remains unelevated.
- The same startup path is used during limiter recovery, addressing the elevation failure that left restoration pending in the reporter's rc.6 log. Unrelated launch failures do not trigger an elevated retry. Portable hosts report how to start RTSS manually as administrator.
- Keeps a service-owned RTSS process alive when reconnecting after a failed restoration, and recognizes equivalent installation paths containing dot components or directory junctions.
- Reduces repeated CPU work in WGC's timing predictor by caching statistics when a new frame arrives. Pacing decisions remain unchanged; this is a small CPU optimization.
- Removes an unintended WGC capture throttle at 60 FPS and below by explicitly setting a zero minimum update interval. The untouched Windows default measured 16 ms and delivered only 55–57 capture updates/sec; explicit zero delivered about 217 in the same native background comparison. A brief AV1 background stream then passed the delivery-rate check at 60.585 FPS, versus 58.039 before, with zero decode errors. These checks do not establish distinct-picture delivery or reduced latency. Streams above 60 FPS retain the tested 1 ms request in rc.8, and WGC compute stays enabled by default. Later distinct-picture comparisons are recorded under rc.9 and in PERFORMANCE.md.
- Corrects a diagnostic timestamp that could describe an older frame after memory-address reuse. Adds optional presentation statistics to the motion probe and a polling comparison without frame notifications; these do not change normal streaming.
- Reproduced error 740 locally with RTSS 7.3.5, then verified an earlier rc.8 candidate's automatic startup through the installed Windows service, a measured 59.94 FPS cap, and restoration to 120 FPS on disconnect while Desktop stayed open. All 208 ordinary automated tests and the explicitly selected native WGC interval test passed. Isolated recovery checks cover user edits, absent settings, a pending journal after elevation denial, and reconnection after SDK failure. The final executable is now installed locally: its hash matches the tested candidate, setup completed successfully, the service reports rc.8, and the firewall rule targets the correct executable. The final build's complete limiter lifecycle and confirmation on the reporter's RX 9070 XT remain pending.

## New in rc.7

- Updates notify first by default. Maintenance offers **Install when idle**, with automatic installation available as an opt-in under General settings. Release candidates follow the existing prerelease setting.
- The host downloads only the exact official Windows installer and verifies its size and GitHub SHA-256 digest. Downloads yield to new sessions; installation waits for one minute without streams, pending connections, remote monitors or host apps. New connections are held off during the installation handoff.
- The service updater stages and verifies the package, backs up replaced files, checks that the requested host version starts, and restores the previous package files after ordinary copying or startup failures. Settings, devices and drivers stay in place. Failed versions are not retried automatically.
- The update panel now displays release candidates correctly, along with download progress, cancellation and the last installation result. The basic HTML console also exposes update actions.
- Validation covers idle/session admission, cancellation, official HTTPS downloads, checksum failures, authentication and CSRF, isolated file recovery, and desktop/mobile UI states. The 199 ordinary automated tests and a separate official-download test passed. A full live service upgrade still needs validation; automatic installation remains opt-in.

## New in rc.6

- A client's `virtualDisplay=0` request now inherits the host's virtual-display setting, matching Vibepollo. Previously it disabled the configured virtual display, leaving the physical monitor active or failing capture when that monitor was off. A positive request also preserves the configured shared-display mode. Explicit app display choices still apply, and the device's "Always use a virtual display" option takes precedence.
- Optional PyroWave capability checks now run in a separate user process. A crash or stall there leaves H.264, HEVC and AV1 available; the worker has a 15-second deadline and is cleaned up on failure. This contains the startup failure investigated in an rc.5 dump: RTSS called into the Vulkan loader during a Wallpaper Engine window callback after the probe's Vulkan modules had unloaded. It does not modify either overlay.
- The earlier AV1 fresh-frame shortfall occurred while Warhammer 3 occupied 92–99% of the GPU. With the game closed, the released rc.5 build delivered 59.84 fresh FPS at a 60 FPS target, decoding all 1,047 received frames without errors. This corrects the earlier performance finding; the RX 9070 XT tester's separate latency comparison remains unverified.

## New in rc.5

- Fixes RTSS helper communication in service installations. The helper runs as the signed-in user and could not write its reply into the service's protected configuration folder. It now exchanges bounded messages over a private local pipe, with both process identities checked. The service retains responsibility for writing the RTSS profile; no folder permissions or helper privileges are changed.
- RTSS profile updates and restoration can proceed while the RTSS window holds the profile open without delete sharing. Unknown profile settings and keys that were originally absent are preserved. A failed write no longer leaves recovery pending when the profile never changed. Failures now log their actual cause and RTSS path, instead of only reporting that no limiter provider was available.
- Limiter ownership now follows pending and connected streams, independently of retained game displays. Disconnecting the final stream restores the original cap even when the desktop remains available to resume. Reconnecting reapplies the cap before streaming starts; an abandoned launch also releases its changes when it expires.
- Display logs now record the client's virtual-display request, selected display and layout. A capture startup timeout identifies the selected display and suggests a virtual display when streaming with the physical monitor off. The reported Artemide monitor wake-up issue remains under investigation; these diagnostics do not claim to fix it.
- Verified the installed SYSTEM service applying a 59.94 FPS RTSS cap on an RX 7900 XT. A separate SDK reader confirmed the fractional profile and enabled limiter, and a renderer requesting 240 FPS on a 120 Hz virtual display presented about 59.94 FPS. The service also captured a native HDR AV1 stream through the WGC user helper with compute enabled. The RTSS fixture additionally covers restoration, unrelated flags, helper crashes and a bounded timeout for a stalled SDK.

## New in rc.4

- WGC selected by the installed service now runs capture in a hidden process belonging to the signed-in user. This addresses the `CreateForMonitor` error `0x80070424` observed when SYSTEM tried to open the per-user Windows capture broker. Three synchronized GPU textures transfer frames to the host without CPU readback; compute copies remain enabled on supported AMD GPUs.
- The helper has bounded frame queues, peer identity checks and automatic cleanup. A helper exit reopens capture through the existing recovery path. Lock and UAC desktops select Desktop Duplication, with WGC retried when the normal desktop returns. Logs distinguish requested capture from the actual backend.
- Verified through the installed SYSTEM service on an RX 7900 XT: a signed-in-user helper captured a native HDR virtual display at 2184×1968 for an AV1 120 FPS session, with compute enabled at both ends. The client confirmed correct pictures and responsive input. This confirms the service fix; it is not a controlled 120 FPS performance measurement.
- Portable helper validation passed seven local 720p60 motion cases, including HEVC, AV1, HDR output from an SDR source, and compute disabled. Each sustained 60 fresh pictures per second with no decode errors. Forced helper termination recovered WGC in 357 ms and all 1,249 received pictures decoded successfully. The helper's performance under heavy GPU load remains to be measured.

## New in rc.3

- A stream configured for WGC now falls back to Desktop Duplication if WGC cannot open at startup, matching capture recovery. Logs preserve the Windows error and identify the fallback. This keeps the stream available; it does not provide service-mode WGC or establish equivalent VRR/frame-generation capture.
- **WGC compute copies are now on by default on supported AMD GPUs.** On the RX 7900 XT fixture, mean decoded picture age fell from 14.8 to 11.5 ms at idle and from 52.9 to 34.6 ms under heavy GPU load. Loaded fresh-picture delivery was about 3% lower (51.4 to 50.0 FPS); neither path sustained 60 FPS under that stress. Unsupported sharing falls back to graphics copies. Set `wgc_compute_copy=false` in the configuration, restart the host and reconnect to compare; `gpu_compute_conversion=false` disables all compute copies and conversion. These measurements do not establish the RX 9070 XT result.
- **Capture recovery no longer adds a fixed 150 ms pause.** Each active stream releases its old encoder, frames and filters before capture reopens. Failed reopen attempts retain a bounded retry delay.
- If compute cannot share a captured texture, fallback now copies that same frame. Previously a static desktop could stall while waiting for another update. Desktop Duplication also logs its actual format and dimensions on each open to help diagnose restarts.
- Active capture now keeps the display awake, matching Vibepollo. The request ends with the capture worker and restores the thread's previous power requirements. This addresses capture being starved when Windows turns off an idle display.
- Video packet pacing no longer produces catch-up bursts after a late send. It accounts for wire overhead and caps known local Ethernet routes at 80% of link speed. A wired host still cannot infer a Wi-Fi client's capacity; the reported RX 9070 XT latency difference remains under investigation.

## New in rc.2

- **The mouse pointer no longer disappears.** Desktop Duplication reports the pointer's shape only when it changes, so every capture restart (a mode change, the secure desktop, another client's display arriving) lost the pointer until it changed shape. It is now carried into the new capture.
- **HDR10 metadata in AMF streams.** HEVC and AV1 streams from AMD now carry the mastering display and content light level metadata and signal their colour range, as FFmpeg-based hosts do. Verified in the bitstream for HEVC and AV1, limited and full range.
- **Host errors are no longer silent.** A stream that ends on a host error used to return Moonlight quietly to the app list, as if the stream had been closed on purpose. Moonlight now shows an error with a code, and the host log says why (`session failed`).
- **A packet Windows can't send right away no longer ends the stream.** Video and audio packets are dropped instead, as Vibepollo does, and the video socket gets Vibepollo's 1 MB send buffer.
- **Measured against Vibepollo 2.0.** At 1080p60 HEVC HDR on the same PC with the same encoder settings, the picture reached the client in 13.8 ms against 16.0 ms idle, and in 42.4 ms against 96.4 ms beside a game-like GPU load, with 51 rather than 24 new pictures a second. Decoded HDR colours match the expected values on both hosts.

## Install

- `butterpollo-setup-2.0.0-rc.12.exe` installs or upgrades the host, the `ApolloService` service, the virtual display and gamepad drivers, firewall rules and shortcuts, and can uninstall them. Settings, paired devices, the library and covers are kept.
- For a portable copy, extract `butterpollo-rust-2.0.0-rc.12-windows-x64.zip` and open **Start Butterpollo.exe**. The first launch offers to import a Vibepollo or Apollo profile and leaves the original untouched. Install the drivers separately in that case.
- These are unsigned test builds. Keep a copy of your configuration and the previous installer for rollback.

## Lower latency

- On supported AMD GPUs, captured frames are copied and converted on D3D12 compute queues, and AMF encodes from D3D12. This reduces waiting on the game's graphics queue. In the historical full AV1 HDR 1968×2184 120 fps comparison beside a heavy GPU load, the picture arrived 7-9 ms sooner, and 50 rather than 45 new pictures reached the client each second. Idle, it arrived about 0.6 ms sooner; encoding was already at the hardware limit (2.8-3.3 ms). `gpu_compute_conversion` (Settings › Capture) turns this off.
- Current defaults use WGC capture with guarded source-phase pacing, AMF `speed` quality, and a virtual display at twice the stream rate. The earlier Desktop Duplication comparisons remain in PERFORMANCE.md as historical measurements; explicit Desktop Duplication remains available.
- Video FEC uses 21-29% less CPU time than the C++ host, with identical output; encrypted packets avoid extra copies.

## Vibepollo 2.0 features

- Moonlight pairing (PIN and one-time PIN), per-device permissions, display modes and overrides, `/bitrate`, `/unpair`, and the virtual display, permission and frame limiter replies of Vibepollo.
- H.264, HEVC, AV1, HDR, 10-bit SDR, 4:4:4 where supported, PyroWave and VRR with Nonary's Moonlight client, and reference frame invalidation.
- Per-device virtual displays with exclusive, extended, primary and isolated layouts, golden layout restore and the restore hotkey.
- Steam library sync with covers, and streams that end when the Steam game exits. Playnite through Vibepollo's plugin (shipped in the package), including the fullscreen app. Lossless Scaling profiles and frame generation per app.
- Frame limiting through RTSS and NVIDIA profiles, NVIDIA Smooth Motion and RTX HDR.
- A new web console: overview with performance history, library with Steam and Playnite, devices, settings, logs, maintenance and API tokens.
- Tray notifications for pairing requests and new versions; release checks that skip streams.

Left out on purpose: WebRTC streaming, session history and host statistics pages, the ViGEm and SudoVDA fallbacks, and Linux and macOS hosting.

## Known limits

- The final native 720p60 AV1 HDR run had 31 steady arrival intervals above 25 ms. All pictures arrived and decoded, but that cadence issue remains under investigation; the pixel and average-freshness pass is not a smoothness pass.
- Guarded WGC pacing passed the controlled 60 FPS freshness checks on the 2560×1440/120 Hz desktop; earlier 5120×1440 comparisons had fresh-frame losses and have not been repeated with the new guard. Both pacing alternatives still failed the 60 FPS freshness gate under uncapped GPU saturation. See [PERFORMANCE.md](PERFORMANCE.md); successful decoding alone is not a smoothness pass.
- The Artemide reporter's phone and RX 9070 XT are unavailable locally. The virtual-display precedence fix addresses a reproduced protocol bug, but confirmation on that phone is still needed.
- The RX 9070 XT report of 4.7 versus 3.9 ms on Wi-Fi remains open pending the tester's comparison. Packet pacing and recovery fixes address observed problems, but are not proof that this latency difference is resolved.
- DDX startup from an inactive desktop remains under investigation. Some local tests received blank pictures and required two capture restarts before valid content arrived; a standalone snapshot test can receive no initial picture. Keeping the display awake fixes continued capture through idle time, but does not resolve this startup condition.
- WGC lock/UAC transitions remain unverified. Isolated helper and synthetic-load checks do not replace final-package service recovery or sustained gameplay validation. Earlier installed-service WGC and native HDR capture were confirmed on the RX 7900 XT; that does not establish the RX 9070 XT result.
- AMD's AV1 encoder pads some sizes: 1968×2184 decodes as 1984×2186 ([AMF issue 423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423)); Vibepollo has the same result. HEVC is exact.
- Verified on an AMD RX 7900 XT. NVIDIA and Intel encoders, RTX HDR, a real Playnite and Lossless Scaling, the secure desktop during a stream and streaming the sign-in screen after a reboot are not yet verified on hardware. NVIDIA and Intel keep the graphics-queue capture path.
- Still missing from Vibepollo: choosing the GPU that renders the virtual display, reclaiming virtual displays after a host restart, Playnite focus retries and fullscreen relaunch, `/api/browse`, `/api/apps/{uuid}/icon`, and tray app notifications and state icons.

Details: [PARITY.md](PARITY.md) for each feature and its evidence, [PERFORMANCE.md](PERFORMANCE.md) for measurements and how to reproduce them.
