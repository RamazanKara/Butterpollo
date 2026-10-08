# Butterpollo 2.0.0-rc.23 release candidate for Windows

[Documentation](../docs/README.md) · [Install and migrate](../docs/getting-started.md) · [Configuration](../docs/configuration.md) · [Compatibility](PARITY.md)

**Release history:** [rc.24](#new-in-rc24) · [rc.23](#new-in-rc23) · [rc.22](#new-in-rc22) · [rc.21](#new-in-rc21) · [rc.20](#new-in-rc20) · [rc.19](#new-in-rc19) · [rc.18](#new-in-rc18) · [rc.17](#new-in-rc17) · [rc.16](#new-in-rc16) · [rc.15](#new-in-rc15) · [rc.14](#new-in-rc14) · [rc.13](#new-in-rc13) · [rc.12](#new-in-rc12) · [rc.11](#new-in-rc11) · [rc.10](#new-in-rc10) · [rc.9](#new-in-rc9) · [rc.8](#new-in-rc8) · [rc.7](#new-in-rc7) · [rc.6](#new-in-rc6) · [rc.5](#new-in-rc5) · [rc.4](#new-in-rc4) · [rc.3](#new-in-rc3) · [rc.2](#new-in-rc2)

Butterpollo's host, native helpers, service and setup are written in Rust, with a Svelte web console. The rc.23 installer is named `butterpollo-setup-2.0.0-rc.23.exe` and upgrades an existing Vibepollo or Butterpollo installation in place, keeping settings, paired devices, the app library and covers. Codec SDKs and Windows drivers remain external components; the setup installs the drivers.

## New in rc.24

- **Controllers work with an Xbox 360 or DualShock 4 choice and no ViGEmBus.** Choosing Xbox 360 (ViGEmBus) or DualShock 4 (ViGEmBus) as the controller type, or bringing `gamepad = x360` or `ds4` over from Sunshine, Apollo or Vibepollo, needs ViGEmBus, which the installer doesn't include. Without it, every controller did nothing, for example from an Android phone. These choices now use the bundled VHF Xbox One or DualShock 4 pad instead and the stream card says so; install ViGEmBus or choose Automatic to remove the warning. With ViGEmBus installed nothing changes. Checked with tests only, not yet with a real Android client.
- **Steam listing one controller twice:** the Steam beta fixes it, as users report; [troubleshooting](../docs/troubleshooting.md#steam-shows-two-controllers) has the details and the ViGEmBus workaround for Steam's stable client.
- **Steam Deck: back grips, both trackpads and a hint when Steam Input hides them.** With Steam Input off for Moonlight, a Deck gets a virtual DualSense with its gyro, and now:
  - **Back grips work.** No virtual controller has back buttons, so L4, R4, L5 and R5 were dropped, as in Sunshine, Apollo and Vibepollo. **Settings → Input → Back grips** now sets what each one presses: a face button, bumper, fully pulled trigger, stick click, Back, Start, Guide, d-pad direction, touchpad click or Share. They default to nothing, and pressing an unmapped grip says where to set it. Xbox Elite paddles use the same settings.
  - **The right trackpad is no longer ignored.** Both trackpads share the virtual pad's touch surface, left and right halves, when Moonlight sends them (builds with SDL 3).
  - **"No gyro" explained.** With Steam Input on, Moonlight only sees a virtual Xbox controller. When a device named Steam Deck connects that way, the stream card says to disable Steam Input for Moonlight.
  - Checked with unit tests only: no real Steam Deck has been tried. [Steam Deck](../docs/configuration.md#steam-deck) has the setup.
- **Input a device isn't allowed to send is no longer dropped silently.** Devices paired after the first start view-only, as in Apollo and Vibepollo, so their controller, touch, pen, mouse and keyboard input was ignored without a word. The stream card now names the permission to turn on under **Devices**.

## New in rc.23

- **Streams ride out a saturated GPU instead of ending.** An RX 9070 XT streaming 4K60 HEVC beside a game froze and then lost the stream with "The encoder returned no frame for 100 ms": the host recreated a stalled encoder about ten times a second, and a fresh encoder's first 4K keyframe on a busy GPU never got time to arrive. A stall now waits 250 ms before the first recreation, then 500 ms, 1 s and 2 s, and the session gives up after 20 s instead of 10 s. Large frames keep the error correction that fits instead of none. Your encoder settings are never changed behind your back.
- **PlayStation controllers can have adaptive triggers with ViGEmBus installed.** Automatic now gives a PlayStation-type client the VHF DualSense instead of a ViGEmBus DualShock 4, because ViGEmBus cannot emulate a DualSense. Xbox-type clients, including a Steam Deck, stay on ViGEmBus, so Steam still lists one controller. Without the VHF driver, the DualShock 4 on ViGEmBus is used as before; `ds4` and `vhf_ds5` still pick a driver explicitly. Checked with selection tests and a compile check only: not yet tried with a real DualSense on a client or in a game.
- **Sturdier connections:**
  - A client resetting its connection while connecting no longer shuts the host down and ends the running game.
  - A session whose control connection is lost now ends after the ping timeout instead of holding the encoder, capture and display until the host restarts.
  - A Wi-Fi drop of 5-10 s no longer ends the stream: the network library's own 5 s timeout now follows the 10 s ping timeout.
- **Streaming before sign-in and while Windows is locked (issue #6):**
  - A launch on the sign-in screen no longer fails with error 503 while looking for old Sunshine display files in a user folder that doesn't exist yet.
  - A launch while Windows is locked no longer fails with error 503. Windows allows display changes then only from the lock screen's own desktop, and the host made them from the normal one, so every launch was refused whatever the capture method. Setting up, keeping and restoring the stream's display now happens on the lock screen's desktop, as capture and input already did. If the virtual display still can't be set up while locked, the stream shows the physical display and the stream card says so, as Vibepollo does. Not yet verified on a locked PC.
- **Better H.264 picture at low bitrates.** AMF's adaptive quantization is now off by default for H.264: on two game clips at 1440p120 and 20 Mbps, turning it off raised VMAF by 1.0 and 9.4 points at the same encode time. HEVC and AV1 keep it on, where it made no clear difference. Set `amd_vbaq` to turn it back on. The other AMF settings (quality presets, constant bitrate, low-latency usage, high-motion boost) were measured too and stay as they are: none improved the picture without slower encoding or larger frames.
- **AV1 at sizes AMD pads:** at sizes such as 1968×2184, AMD encodes AV1 with extra rows and columns (1984×2186) that clients have to crop. The log now says so once per stream; HEVC avoids it.
- **Playnite, as Vibepollo does it:** games are brought to the front using the existing focus settings, Playnite fullscreen starts again after a game instead of ending the stream, a cover saved in the console is set in Playnite too, and the console can restart Playnite. Not verified with a real Playnite installation.
- **Tray:** notifications when an app starts, pauses, resumes and stops; a green or amber dot on the icon while an app streams or is paused; "Quit <app>"; and "Check for updates".
- **Console file picker:** Browse buttons for an app's command and working folder and for the Lossless Scaling and RTSS paths.
- **No latency change from rc.22.** On the RX 7900 XT, rc.22 and this release alternated in one batch at 1968×2184 HDR 120 fps (HEVC and AV1), 1440p120 and 1080p60, idle and beside a game; picture age matched within the spread of the runs. Details in `rust/PERFORMANCE_WORK.md`.

## New in rc.22

- **PyroWave holds 60 fps at high bitrates.** The sender's pacer counted every send call and late wake-up on top of the paced wire time, and PyroWave's automatic rate left no room for headers and error correction. A frame that filled its budget took 20-22 ms to send, so 1080p60 at 400 Mbps, 1440p and 4K dropped to about 57 fps. Pacing now credits up to 0.5 ms of send time per batch, and PyroWave paces at 1.25× its demand when the link speed is unknown. The same frames now take 12.8-13.1 ms, about 78% of a 60 fps frame period, at 1080p, 1440p and 4K. The release check now also fails PyroWave streams that replace more than 1% of frames.
- **No more silent "no codec" host after a GPU reset.** If the host started before the graphics driver was ready, after a crash or a driver timeout, its encoder check gave up after six tries and every stream failed without saying why. It now keeps retrying until an encoder works, checks again when a client asks, and the console shows "No video encoder available … Retrying" in the meantime. Retries never run while a stream is starting.
- **Stream display problems:**
  - **VRR streams no longer settle at 60 Hz:** the virtual display gets the stream's resolution and refresh after it is created and after every recovery. Before, Windows picked a saved mode, and for some 4K VRR streams that was 60 Hz, so a 116 fps game streamed at about 60 fps. The log now shows `virtual display mode applied requested=… actual=…`, and the stream card warns if Windows kept a different mode.
  - **The stream no longer silently shows your physical desktop:** when Windows switched the virtual display off mid-stream (for example when a game reset display settings on exit), capture fell back to the main monitor, so the stream showed the host's desktop at its own resolution. Capture now waits for the stream's display to come back. A temporary "display busy" error no longer rebuilds the display, and a display Windows switched off is switched back on with the exclusive layout and HDR.
  - **Saved 60 Hz modes no longer cap VRR:** a 60 Hz mode saved for a device also limited the game to 60 fps under VRR. The frame limiter now follows the stream rate, and a VRR stream never reuses a display prepared without VRR.
- **No silent downgrades:**
  - Automatic picks the encoder by GPU vendor (AMD → AMF, NVIDIA → NVENC). If it can't start, it tries other hardware encoders and says so on the stream card; it never falls back to software.
  - The console's stream card shows the encoder in use, plus warnings for capture fallback, display modes or frame limits that weren't applied, audio route changes and loss, network limits, and missing input features.
  - The console no longer shows NVENC settings on AMD hosts under Automatic.
- **Install and update safety:**
  - A failed update keeps the previous installation working and keeps its backup for a retry.
  - Manual reinstalls and upgrades use the same rollback as in-app updates.
  - Incomplete packages are rejected before anything changes, and downgrades are refused.
  - Migrating from Vibepollo, Apollo or Sunshine checks the old profile before replacing the old host.
  - Driver setup works with non-ASCII user folders and treats "reboot needed" as success.
  - A setup interrupted earlier can't run driver tasks later.
- **Controllers:**
  - DualShock 4 rumble and lightbar now work on ViGEmBus.
  - Repeated feedback failures show up in the log.
- **Fewer ways for a stream to end:**
  - A PyroWave frame that can't be prepared drops that frame instead of ending the session.
  - A handle leak in the capture threads is fixed.
- **Wi-Fi pacing only where it helps:** 2× pacing now applies only to confirmed wireless routes. Wired hosts behind a Hyper-V switch, VPN or Tailscale keep full-speed pacing again.
- **Playnite plugin installed as you:** the plugin is installed and removed as the signed-in user instead of SYSTEM, so a link in a user folder can't redirect a SYSTEM write or delete.

## New in rc.21

- **Security: the Playnite connection can no longer act as the host.** When the host connected to Playnite's connector pipe, a program posing as Playnite could take on the host's identity, which is SYSTEM when it runs as a service. The host now allows the pipe server only to identify it, not to impersonate it. This issue predates rc.20.
- **PyroWave bitrate guidance from measured picture quality.** 6,048 decoded comparisons across desktop, game, dark and HDR scenes set two levels. At 60 fps the picture breaks down below about 139 Mbps (720p), 187 Mbps (1080p) and 747 Mbps (4K). It is clean from about 277, 399 and 1593 Mbps. The console's stream card shows red below the first level and amber below the second, the log warns, and the guides give the same numbers. At 4K, a clean PyroWave picture needs more than a gigabit link can carry; use HEVC or AV1 there.
- **Crash reports can no longer hang the host.** The host used to write its own crash dump, which Windows documents as able to deadlock; under load a test hung 1 run in 30. A separate reporter process now writes it. If that process can't start, the host still starts and logs a warning.
- **Optional AMF limits for recovery frames.** On Wi-Fi a lost frame triggers a full keyframe, several times a normal frame. A new opt-in frame-size cap cut the largest recovery frames at 4K60 and 40 Mbps from 266 to 90 KB (HEVC) and from 154 to 82 KB (AV1), at a small encode-time cost. The defaults are unchanged. AV1 bitrate changes during a stream now use the right AMF property.
- **Fixes from a review of rc.20:**
  - A DualShock 4 battery level above 100% from a client no longer corrupts the controller report.
  - After a quick change of the stream's display, absolute mouse input can no longer land on the previous display.
  - One client disconnecting no longer releases another client's network acknowledgements before its input is applied.
  - A ViGEm controller keeps its rumble if unplugging it fails.
- **Release checks cover PyroWave.** Every release now has to decode PyroWave end to end at 1080p60, SDR and HDR 4:4:4, with picture, motion, cadence and audio checks. The test tone no longer starves under CPU load, so audio checks measure the host rather than the test machine.

## New in rc.20

- **Controller input no longer waits behind a busy gamepad driver.** Every controller update was a blocking call into the virtual gamepad driver, on the thread that also handles keyboard, mouse and touch. When a game loaded every CPU core, that call could stall for up to 287 ms and everything else waited behind it. Gamepads now run on their own thread, and a mouse move behind a stalled controller waits at most about 2 ms. The input thread also keeps its multimedia priority boost: behind time-critical load, input waited a median of 3.5 ms and now waits 17 µs.
- **Snappier keyboard and mouse.** Moonlight's acknowledgement goes out after its input is applied, saving about 25 µs per event. A burst of keyboard and mouse events reaches Windows in one call: 8 events take 38 µs instead of 180 µs. Setting up input when a session starts no longer blocks it for 3-4 ms.
- **One controller in Steam again: ViGEmBus is back.** Steam on the host often listed Butterpollo's virtual Xbox controller twice. Its bundled controller library races its XInput and GameInput backends, and when it happened, Start+Select could open Game Bar and the Steam keyboard. When ViGEmBus is installed, Automatic now uses it, as Vibepollo does: an Xbox 360 pad, or a DualShock 4 for PlayStation-type controllers. Steam lists it once. Without ViGEmBus, the virtual gamepad driver is used as before. The new `x360` and `ds4` choices pick ViGEm explicitly.
- **PyroWave sends sooner and is verified end to end.** The first protected block of a frame is ready in 0.06 ms instead of 0.49 ms at 1080p120, and in 0.21 ms instead of 1.75 ms at 4K60, with identical bytes on the wire. 1,152 frames decoded with PyroWave's own decoder match between the compute and graphics conversion paths. Loss feedback from the client no longer forces extra frames.
- **PyroWave warns when its bitrate is too low.** At HEVC-like bitrates PyroWave shows only blurred grey blocks. Below one bit per pixel per frame (about 55 Mbps at 720p60, 125 Mbps at 1080p60, 500 Mbps at 4K60), the log and the console's stream card say so, and the setting's description gives the numbers.
- **Steadier streams on Wi-Fi.** Video on wireless and unknown network routes is now paced at twice the stream bitrate instead of bursting at up to 800 Mbps, so bursts don't overrun the access point; audio shares that path. Ethernet is unchanged, and `pacing_max_bitrate_kbps` still overrides it. The log now also reports audio the host loses before sending it.
- **Playnite launches more reliably.** Playnite is found and started in the right Windows session, a missing or partial plugin is repaired, failed library syncs retry promptly, and streams survive Steam and Epic launcher handoffs and Fullscreen mode. Failures now say exactly where the launch stopped. See "Playnite does not launch" in the troubleshooting guide.
- **RX 9000 safety.** SmartAccess Video combined with a forced low-latency mode, a combination that has caused GPU resets, is now prevented. A lost GPU is no longer treated as finished work, which could have reused frames unsafely. The troubleshooting guide has an RX 9000 driver section and a test checklist.
- **AMF settings tell the truth.** The script-free console saved the low-latency switch under a key nothing read; it now saves `amd_lowlatency_mode` and offers the AV1 latency mode. With the default ultra-low latency usage the driver already applies both, so they stay on Driver default. A new `amd_split_frame` setting asks the driver to split frames across two encoders where it ships that off, as the original host did. On an RX 7900 XT the driver never splits, so nothing changes there.
- **Honest capture timing.** The stream log now reports WGC's own delivery delay (about 1 ms on a virtual display), which the old numbers hid.
- **Clearer limits.** AMD encoders produce 4:2:0 only, RX 9000 included; 4:4:4 on AMD means PyroWave. The docs and the error message now say so.
- **A new introduction film.** It explains what Butterpollo is, follows one frame through the Radeon path, and shows the measured comparison with Vibepollo 2.0 beside a game.

## New in rc.19

- **Lower latency when the encoder cannot keep up.** At 4K with a high refresh rate, or on an RX 9070 XT with its single HEVC encoder, one encode can take longer than a frame. The host kept claiming new pictures anyway, and up to eight queued in the encoder, each older when it came out. It now waits while two are in the encoder, enough to keep both encoder instances of a Radeon busy. At 5120x1440 HEVC 240 fps on an RX 7900 XT, whose encoder manages 220 fps there, the time from the game's frame to the packet fell from 42.7 to 11.1 ms, at the same 220 fps. Streams the encoder keeps up with are unchanged.
- **PyroWave converts colour on the Radeon compute queue.** Its conversion was the last one still running on the graphics queue, behind the game: beside a game using the whole GPU, a 1080p HDR 4:4:4 frame took 5.6-5.8 ms instead of 0.47 ms. It now runs on the same high-priority compute queue as the AMF path, at 0.54-0.57 ms beside that game, with identical output. At high bitrates PyroWave also starts sending up to 2 ms sooner, after a slow byte-by-byte check of every frame was made eight times faster.
- **AMF no longer drops frames to stay on its bitrate.** Frame skipping was left to the driver; a skipped frame shows on a VRR client as a held picture. It is now off for H.264 and HEVC, as in Vibepollo.

## New in rc.18

- **Two GPUs: encoding on the Radeon works.** With a Radeon for games and encoding beside an NVIDIA card that drives the monitor, the encoder check at startup ignored the GPU chosen in the settings and tested the monitor's card instead. On automatic it found NVENC there; forced to AMF it offered Moonlight no codecs, and the RTSP handshake failed. The check and the encoder now use the selected GPU, and automatic tries that GPU's own encoder first: AMF on a Radeon. A Radeon PC with an Intel iGPU no longer risks picking Quick Sync either. Desktop Duplication, which can only capture on the GPU the monitor is connected to, now says so and points to WGC capture, which works across GPUs.
- **A new launch film** follows one frame through the Radeon path, shows the same-build compute result and what rc.17 brought players, with a new original score.

## New in rc.17

- **Steam Deck and other controllers with motion sensors get a virtual DualSense, as in Vibepollo.** An Xbox-type controller with a gyro or touchpad, such as the Steam Deck, stayed an Xbox pad: it lost its gyro and touchpad, and Moonlight's quit combo opened Game Bar and Steam's keyboard on the host. With `motion_as_ds4` and `touchpad_as_ds4` both off it stays an Xbox pad. A controller that connected while the stream's display was still being set up also lost its motion sensors, because input was dropped until then; keyboard and controllers now work from the start.
- **The first launch no longer fails when an app's prep commands take a while.** A launch expired 30 s after Moonlight asked for it, even while its prep commands were still running, so Moonlight then found no launch to connect to ("no authorized RTSP launch") although the game was running, and only a retry worked. The host also keeps answering while prep commands run, instead of showing as offline to every client.
- **Lower latency:**
  - With the default install, the host learns of a captured frame the moment the capture helper has it instead of polling for it: about 0.7 ms less from the game's present to the packet.
  - Captured audio is sent as soon as Windows delivers it (event mode, as in the C++ host), and the encoder's frame deadlines are met more precisely.
  - Colour conversion on the compute queue runs in one pass instead of two.
  - Taps and clicks with touch or a pen are released on time; they were 4 ms late on average, and key repeat had up to 8 ms of jitter.
  - On a local network, video and audio are tagged for priority as Moonlight asks (DSCP 40 and 56), as Vibepollo does: Wi-Fi sends them ahead of other traffic. Moonlight does not ask for this over the internet.
- **Smoother pacing:**
  - A keyframe the client asks for after packet loss, or a repeat of a still picture, no longer holds back or skips the game's next frame.
  - With Desktop Duplication, mouse-look in a game (a hidden pointer that moves) no longer costs frames.
  - VRR follows a game's uneven frame times more closely.
  - The capture helper now gets its first frame on a still desktop, where it could fall back to Desktop Duplication for the whole session (seen on a Legion Go).
  - The check that keeps a stream's virtual display alive costs 27 µs a second instead of up to 31 ms.
- **One encoder hiccup no longer slows the rest of the session.** Since rc.14 any encoder failure moved colour conversion to the graphics queue, which waits behind the game: 7.2 ms instead of 0.9 ms per frame beside a GPU-bound game. That now happens only after a second failure. An encoder that keeps failing no longer leaks memory, and a recovery that produces no frames ends after 5 s instead of continuing.
- **Several clients on one PC:**
  - A second stream no longer changes the resolution, refresh rate or HDR of a display another stream is using.
  - The saved display layout is restored when the last stream ends, not under a stream still running.
  - The layout is also restored when a remote monitor joined during the stream; the physical monitors had stayed off.
  - Clients sharing the virtual speakers with different speaker setups (a stereo phone and a 5.1 PC) no longer keep losing each other's audio.
  - Quitting an app closes only the game the launching client's store client started, and never a program someone opened on the PC.
- **Displays and HDR:**
  - An HDR client on a display without HDR streams in SDR instead of failing to launch.
  - HDR is switched back on the display as it is connected at the stream's end, also after a driver reset or a TV that reconnected.
  - A TV that was in standby when a stream ended gets its original HDR, mode and colour profile back even when the next stream uses it first.
- **Your speakers come back after a stream.** If they could not be set when a stream ended (a Bluetooth headset turned off, a TV's audio not back yet), every later stream restored Steam Streaming Speakers instead. The host now keeps the original devices and restores them once possible.
- **Settings import from Sunshine, Apollo, Vibeshine and Vibepollo:**
  - It reads files those hosts write and load: sunshine.conf in UTF-16 or the ANSI code page, state files with a byte order mark or left empty, paired devices stored with `""` lists, and device commands in Apollo's format.
  - A profile naming files that no longer exist, or holding large logs, links or oversized files, imports with warnings instead of failing.
  - The imported profile is checked with the host's own loaders, so setup no longer reports success for a profile the host then refuses to start with.
  - When an import fails, setup shows why and restarts the previous host's service, including Sunshine and Vibeshine.
  - A previous host whose installer recorded no install folder is found anyway.
  - One unusable device or app override is skipped with a warning instead of failing every stream of that device or app.
- **Updates:**
  - A host set to a specific `bind_address` is no longer rolled back as "did not start".
  - If the service cannot be stopped, it is started again and the update is reported as failed.
  - An update cut off by a power loss or crash is rolled back at the next start.
  - Only the newest two folders of unfinished updates are kept.
- **Pairing:** a second device asking to pair no longer takes over the PIN of the first, and nobody on the network can keep others from pairing.
- **The web console's "Remember this device" keeps you signed in for 7 days.** It signed you out after two hours. Releases now ship the console built from their own source; before, it was carried over from an earlier package.
- **Games:** a game that restarts itself through Steam (after an update or a settings change) no longer ends the stream, and a browser or store client that a game opens is no longer closed with it.
- **Touch and pen:** held touches and a resting pen are no longer cancelled while the other is in use, Shift held during key repeat is no longer released, pen pressure and buttons are sent as the C++ host sends them, and contacts are lifted when a stream ends.
- PyroWave follows `pacing_max_bitrate_kbps` when it is set.

## New in rc.16

- **Audio without dropouts.** Since rc.14 the host sent silence as soon as Windows had delivered no sound for 10 ms, but Windows delivers sound in chunks of about that size, so ordinary scheduling jitter put short gaps of silence into the stream. Silence now starts only after 50 ms without sound.
- **The uninstaller removes only Butterpollo's files.** Run outside an installation, it could take the folder it ran from (Downloads, for example) for the installation and delete all of it, and a custom install folder lost everything else in it. It now refuses when Butterpollo is not installed, deletes only the files the installation lists, and removes a folder only when nothing else is left in it.
- **Touch, pen and absolute mouse land where you tap on a letterboxed stream.** When the PC's display has another shape than the client (a 16:9 PC on a 4:3 iPad), taps were off by up to a few hundred pixels and the bottom of the screen, taskbar included, could not be reached. Input now skips the black bars as the original host did.
- **Password guessing over the network is limited.** Sign-ins sent with each request (Basic credentials) were not counted, so a device on the network could try passwords at full speed. They now share the login page's limit of 10 attempts a minute per address, this PC is never locked out, and a flood of addresses can no longer block sign-in.
- Two crashes that ended every stream are fixed: a controller whose profile changed after it connected, and one malformed cover-image request.
- **Settings that stopped the host are refused or worked around.** One bad value already in sunshine.conf no longer blocks every later save; a value the file would cut short (an unquoted `#`) or that would swallow the settings after it (an unclosed `[`) is refused with the key named; and a network interface name in place of an address, or a folder as the log file, no longer keeps the host and its console down.

## New in rc.15

- **Fixes blurry VRR streams and fringed text.** The host could encode faster than the stream's requested frame rate, spreading its bitrate across too many frames. VRR now caps encoding at the negotiated rate while taking new frames as they arrive.
- **Restores duplicated displays more reliably** when Windows needs to move a display to another desktop source.
- **Uses the Butterpollo icon** throughout the executables, Start menu and tray.

## New in rc.14

- **Streams survive encoder and GPU hiccups.** An encoder error mid-stream ended the session, which Moonlight reports as error -7fffbffb; a game holding the GPU or a driver reset was enough. The host now recreates the encoder, keeps the hardware encoder the stream started with, converts on the graphics queue if the compute path failed, and asks for a keyframe. Only failures that last five seconds end the stream. A wait for GPU work that never finishes no longer freezes the picture for good; it gives up after two seconds and recovers the same way.
- **Fewer frozen pictures on Radeon RX 9000 cards.** The encoder input queue was forced to one frame for every client asking for VRR low latency, which exposes a known RDNA4 driver freeze where video stalls while audio plays. It is now set only on request, as in the original backend. A keyframe requested after a client resets its decoder now carries the AV1 sequence header, so the client can start again, and a frame the encoder loses is followed by a keyframe instead of damaged pictures.
- **GPU safety.** Captured textures could be released while the GPU was still copying or converting them, the kind of fault that hangs a driver; they are now kept until the GPU is done, and stale ones no longer pin gigabytes of video memory. The queues that copy and convert captured frames ran at global realtime priority, ahead of the desktop compositor every captured frame comes from; they now run at high priority, still ahead of a game's normal work (`compute_queue_realtime = true` restores it). An HDR stream on an RX 7900 XT had ended in an AMD driver timeout that took the GPU offline until a reboot; its cause is not proven, and these are the two likeliest contributors.
- **Quitting closes games that a store client started.** The Xbox app, Epic, EA, Ubisoft Connect and Battle.net start games themselves, outside the processes an app launches, so quitting left them running. The host now notes the fullscreen game on the stream's display when it started after the launch, and quitting closes it like the app's own processes. Store clients themselves, the plain desktop and Steam or Playnite games are left alone.
- **Radeon RX 6000 cards:** colour settings a driver rejects no longer cost the encoder, the same kind of rejection as the HEVC fix in rc.12; a bitrate the driver refuses keeps the current one instead of failing every frame.
- **Lower audio and input latency.** Audio goes out as soon as Windows captures it instead of on a fixed 5 ms tick, which added 5 to 20 ms and could insert short silences. Input is handled the moment it arrives instead of after a 1 ms sleep, and a finished video frame is sent without waiting for the encoder's next one.
- **The cursor shows on a PC without a mouse.** Windows hides the pointer when no mouse is connected; while streaming, Mouse Keys are turned on so it appears, as in Sunshine. Reported with Moonlight on macOS.
- **Hosts on Wi-Fi:** connected Wi-Fi adapters run in media streaming mode while streaming, with fewer background scans and the stutter they cause. The desktop compositor is scheduled with multimedia priority.
- **RTSS frame limiter applies again.** Before changing a frame limit or a display, the host starts a helper that undoes it if the host dies. Where the host runs in a job that forbids that helper to leave it (a scheduled task, a launcher), starting it failed and the limiter gave up, so RTSS never got the stream's rate. The helper now starts inside the job in that case.
- **The tray icon shows after a reboot.** Windows keeps Explorer's "taskbar created" message from the elevated host, so an icon the taskbar refused at boot never appeared after sign-in. The host now accepts that message and retries until the icon is in.
- **Firewall check:** the installer allows the host on every network, but a block rule, which Windows leaves when its "allow access" prompt is dismissed, overrides it and makes manual port exceptions look necessary. The host now logs such rules by name at startup, and `--diagnostics` lists them.
- **Input fixes:**
  - Absolute mouse, touch and pen follow the stream's display when it is created, recreated or changes resolution, instead of the display at the first input event.
  - A key or button release that Windows refuses (a UAC prompt, the lock screen) no longer leaves the key repeating, or dead in later sessions.
  - A long press with touch or a tablet right-clicks without left-clicking first, as in Sunshine.
  - Print Screen works; a missing virtual gamepad driver no longer slows other input; unplugging a controller no longer leaves a phantom pad; each virtual pad logs the profile it got.
- **Displays:** when Windows switches a TV on together with the stream display, the host no longer hands Windows the TV's leftover modes while switching it off again, the likely reason Windows refused; if it still refuses, the stream starts with the TV on, as in rc.13. Restoring duplicated displays after a stream falls back to letting Windows choose a mode the group can show when it refuses the original one.
- Releases are built, checked, installed and published by one script, and each release tag gets its own CI run.

## New in rc.13

- **Fixes streams that would not start** on rc.12 when Windows also switched on a TV or monitor that was off as the stream display arrived. Switching that display off again failed with "invalid parameter" (error 87), and the launch was cancelled. The host now lets Windows adjust the layout when it refuses the exact one, and if it still refuses, the stream starts with that display left on.
- Duplicated displays are put back after a stream even when Windows refuses the first display's exact mode for the group (error 31): Windows picks a mode the group can show, then every display gets its saved mode back. The display self-test found this with two duplicated stand-in TVs.
- Release tags get their own CI run, and releases are built, checked, installed and published by one script (`rust/release/release.ps1`).

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

- `butterpollo-setup-2.0.0-rc.23.exe` installs or upgrades the host, the `ApolloService` service, the virtual display and gamepad drivers, firewall rules and shortcuts, and can uninstall them. Settings, paired devices, the library and covers are kept.
- For a portable copy, extract `butterpollo-rust-2.0.0-rc.23-windows-x64.zip` and open **Start Butterpollo.exe**. The first launch offers to import a Vibepollo or Apollo profile and leaves the original untouched. Install the drivers separately in that case.
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

Left out on purpose: WebRTC streaming, session history and host statistics pages, the SudoVDA fallback, and Linux and macOS hosting.

## Known limits

- The final native 720p60 AV1 HDR run had 31 steady arrival intervals above 25 ms. All pictures arrived and decoded, but that cadence issue remains under investigation; the pixel and average-freshness pass is not a smoothness pass.
- Guarded WGC pacing passed the controlled 60 FPS freshness checks on the 2560×1440/120 Hz desktop; earlier 5120×1440 comparisons had fresh-frame losses and have not been repeated with the new guard. Both pacing alternatives still failed the 60 FPS freshness gate under uncapped GPU saturation. See [PERFORMANCE.md](PERFORMANCE.md); successful decoding alone is not a smoothness pass.
- The Artemide reporter's phone and RX 9070 XT are unavailable locally. The virtual-display precedence fix addresses a reproduced protocol bug, but confirmation on that phone is still needed.
- The RX 9070 XT report of 4.7 versus 3.9 ms on Wi-Fi remains open pending the tester's comparison. Packet pacing and recovery fixes address observed problems, but are not proof that this latency difference is resolved.
- DDX startup from an inactive desktop remains under investigation. Some local tests received blank pictures and required two capture restarts before valid content arrived; a standalone snapshot test can receive no initial picture. Keeping the display awake fixes continued capture through idle time, but does not resolve this startup condition.
- WGC lock/UAC transitions remain unverified. Isolated helper and synthetic-load checks do not replace final-package service recovery or sustained gameplay validation. Earlier installed-service WGC and native HDR capture were confirmed on the RX 7900 XT; that does not establish the RX 9070 XT result.
- AMD's AV1 encoder pads some sizes: 1968×2184 decodes as 1984×2186 ([AMF issue 423](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/issues/423)); Vibepollo has the same result. HEVC is exact.
- Verified on an AMD RX 7900 XT. NVIDIA and Intel encoders, RTX HDR, a real Playnite and Lossless Scaling, the secure desktop during a stream and streaming the sign-in screen after a reboot are not yet verified on hardware. NVIDIA and Intel keep the graphics-queue capture path.
- Still missing from Vibepollo: choosing the GPU that renders the virtual display and reclaiming virtual displays after a host restart.

Details: [PARITY.md](PARITY.md) for each feature and its evidence, [PERFORMANCE.md](PERFORMANCE.md) for measurements and how to reproduce them.
