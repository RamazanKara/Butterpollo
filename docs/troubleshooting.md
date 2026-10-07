# Troubleshooting

[Documentation](README.md) · [Getting started](getting-started.md) · [Configuration](configuration.md)

Start with the current stream's settings, the console and the logs. Record the time of a failure before changing anything, and change one setting at a time. Capture and performance probes add work to the GPU; use an idle session for those comparisons.

**Find a symptom:** [Pairing](#moonlight-cannot-find-or-pair-with-the-pc) · [Console or port](#the-console-will-not-open-or-a-port-is-occupied) · [Black screen](#black-picture-no-display-or-wgc-fails) · [Blurred PyroWave](#pyrowave-shows-blurred-grey-blocks) · [HDR colour](#hdr-looks-washed-out-too-bright-or-different-between-clients) · [Stutter](#low-latency-is-reported-but-motion-still-stutters) · [RTSS](#rtss-does-not-start-or-the-game-ignores-the-cap) · [Display restoration](#monitors-stay-on-or-the-display-layout-does-not-return) · [Updates](#an-update-stays-queued) · [Logs and support](#logs-and-a-useful-report)

## Moonlight cannot find or pair with the PC

- Open **https://localhost:47990** on the host. If it does not open, check the host/service first.
- Confirm both devices are on the same local network, then try adding the host's local IP address in Moonlight. Guest networks or access-point isolation can prevent devices from reaching each other.
- Check **Settings → General → Allow pairing**. Enter the four-digit PIN **shown by Moonlight** into the matching pending request on **Devices**.
- If discovery alone fails, check **Settings → Network** for the discovery and bind-address settings. An address restricted to `127.0.0.1` is reachable only from the host itself.
- If setup reported a firewall-rule failure, read the setup log for the exact error and check Windows Firewall's allowance for the installed `butterpollo.exe` on your local network. Avoid disabling the entire firewall to diagnose one application.

If the device pairs but cannot launch Desktop, open **Devices → Edit** and check its enabled state and **List apps**, **View streams** and **Launch apps** permissions. A device missing mouse or keyboard permission can receive video without that input working.

## The console will not open, or a port is occupied

The default base port is `47989`; the console uses `47990`. A changed **Base port** moves the console to the next port up. Check the active profile's `sunshine.conf` if the launcher reports another address.

**Start Butterpollo.exe** reopens the correct console when that profile is already running. If it reports another streaming host on the port, close the conflicting Sunshine, Apollo, Vibepollo or second Butterpollo instance. Restarting another copy on the same port does not solve the conflict.

For an installed host, check **Butterpollo Rust** in Windows Services. Its internal service name is `ApolloService`. If it is stopped, read `service.log` before starting it again. The [log locations below](#logs-and-a-useful-report) distinguish service and portable profiles.

## Black picture, no display, or WGC fails

1. On **Overview → Host readiness**, inspect **Screen capture**, **Video encoder** and **Virtual display**. A physical monitor must be active, or the configured virtual display must be available.
2. Check **Settings → Display → Display** and any app/device override. Confirm that the selected display is the one containing the desktop or game.
3. For service-mode WGC, keep a Windows user signed in. Butterpollo starts its WGC capture worker in that user's session. A locked or UAC desktop uses the Desktop Duplication recovery path; WGC is retried when the normal desktop returns.
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

## PyroWave shows blurred grey blocks

The bitrate is far too low for PyroWave. Unlike HEVC or AV1 it spends its bits on speed, not compression: below about one bit per pixel per frame only its coarsest brightness layers fit, without colour or fine detail. Moonlight's default for 720p60, 10 Mbps, gives it a fifth of a bit. Raise the bitrate in the client to at least about 55 Mbps for 720p60, 125 Mbps for 1080p60 or 500 Mbps for 4K60, on a wired network, or use HEVC or AV1. The console's stream card and the log (`PyroWave has too little bitrate`) say when a stream is below that.

## Low latency is reported, but motion still stutters

Compare the same moving scene at the same resolution, frame rate, codec and bitrate. A static desktop may legitimately send repeated or fewer frames. Begin with 1080p60 SDR, then change one variable.

| Statistic | What it helps distinguish |
| --- | --- |
| **Overview → Frame rate** | Frames sent by the host. Repeated pictures can still count toward this rate. |
| **Encode p95** | Slower conversion/encoding completions, including asynchronous encoder work. |
| **Host processing** | Time after the host claims a frame through preparation for transmission. |
| **Frame age** | Waiting from the capture/presentation timestamp until the host claims the frame. |
| Moonlight's network and decoder statistics | Loss, jitter or client decoding delays after host processing. |

None of the host timings measures the complete input-to-screen delay. Smoothness also depends on distinct pictures arriving regularly. The [performance guide](performance.md) separates host timings, measured picture age, fresh-picture rate and arrival gaps.

If the problem starts when the GPU is fully occupied, compare with a lower game frame cap or lighter graphics settings. If client network loss rises, reduce bitrate or compare a wired connection. If decoder time rises, reduce resolution/rate or select another hardware-decoded codec. Keep WGC and compute defaults for the baseline; use capture/compute overrides only for a recorded comparison. The published local tests do not establish results for every Radeon model or Wi-Fi connection.

## RTSS does not start, or the game ignores the cap

Open **Maintenance → Frame limiter** during the affected stream. Record **Configured**, **Active now**, RTSS detection/running state and **RTSS folder**. A missing tray icon alone does not establish whether the limiter is active.

In **Settings → Frame limiting**, check **Limiter**, **Frame limit**, **Virtual display refresh** and **RTSS folder**. **None** disables limiting. Physical-monitor streams need **Limit every stream** if you want a cap on every stream; virtual-display policy can apply its own cap. A frame limit of `0` follows the stream rate, including fractional values such as 59.94.

The folder must contain the RTSS executable and its hook library, not just a shortcut. If the log says RTSS requires administrator privileges, start RTSS as administrator before streaming or use Butterpollo's installed service. If it is found and running but one game ignores the limit, check that game's RTSS profile and include the game/API in the report.

Butterpollo restores limiter values after the last stream owning the limit disconnects. A game retaining its display must not keep the cap active. If a cap remains, check for another pending or connected stream and keep the before/during/after values with the log.

## Monitors stay on, or the display layout does not return

Check **Settings → Display** and app/device overrides first. **Extended** intentionally keeps physical displays alongside the virtual display. Exclusive and isolated layouts have different behavior. A client may also request its own virtual-display arrangement.

Check **Restore displays on disconnect**, **Restore delay** and **Keep a disconnected display for** against the expected lifecycle. An app or retained remote-monitor session may still own a display after video disconnects.

When all streams have ended, use **Maintenance → Displays** to inspect whether the current layout matches the saved one. **Restore saved layout** applies that saved layout; **Save current layout** replaces it with the arrangement you currently want. Saving, restoring and resetting are unavailable during streaming.

**Disconnect virtual displays** stops every stream and removes Butterpollo-created virtual displays. **Reset display settings memory** forgets pending display changes that Butterpollo would otherwise undo. Use those recovery actions deliberately after recording the problem; resetting memory is not the same as restoring a layout.

rc.10 has a brief startup guard for a reproduced case where creating a virtual display reactivated a dormant monitor. That guard is not continuous enforcement and does not establish that every phone/client display report is fixed. Record the client, chosen layout, active monitors and log time when reporting another case.

## "Virtual display did not become active before the deadline"

Up to rc.10, a launch failed with this error when Windows left the new virtual display switched off, typically because it recalled a layout saved for the other connected displays (two TVs in duplicate mode, for example). rc.11 switches the display on itself after a second, without changing the other displays, and logs `Windows left the new virtual display switched off; switched it on beside the current displays`. Update before trying workarounds such as unplugging the TVs. If a launch still fails, the error says whether Windows reported the display as connected; include that line and the log around it in the report.

## The stream disconnects after a second with "os error 10035"

`video sender stopped: A non-blocking socket operation could not be completed immediately. (os error 10035)` comes from rc.1, which ended the stream when the Windows send buffer was momentarily full. Since rc.2 the host waits briefly, drops only those packets and keeps streaming, as Vibepollo does; the log then shows `UDP send failed; packets dropped` at most every five seconds. Install the current release. Frequent drop warnings mean the network cannot carry the bitrate: lower it, or use a standard codec rather than PyroWave over Wi-Fi.

## Steam shows two controllers

Some Steam builds can list one VHF Xbox controller twice. Steam's SDL controller discovery races its XInput and GameInput backends for the same device. Start+Select may then open both Xbox Game Bar and Steam's keyboard. SDL has an [upstream fix](https://github.com/libsdl-org/SDL/commit/c4cfb739), but a Steam build may not include it yet.

Install [ViGEmBus from nefarius](https://github.com/nefarius/ViGEmBus/releases) separately and keep **Settings → Input → Controller type** on **Automatic**. Butterpollo prefers ViGEmBus when available. Its USB-style Xbox 360 pad avoids that duplicate discovery path. ViGEmBus is retired but widely used; Butterpollo does not bundle its installer.

Automatic can choose DS4 for a client with motion sensors or a touchpad, including Steam Deck. Select **Xbox 360 (ViGEmBus)** (`gamepad = x360`) explicitly if you need the Xbox 360 path and do not need motion or touchpad input. Disconnect and reconnect after changing the setting. Check Butterpollo's logs for `backend="ViGEmBus"` and `profile="x360"`, and Steam's `logs/controller.txt` for the new arrival. An explicit VHF choice continues to use VHF even when ViGEmBus is installed.

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
