# Getting started

[Documentation](README.md) · [Configuration](configuration.md) · [Troubleshooting](troubleshooting.md)

Butterpollo is a Windows x64 streaming host written in Rust, with a GPU path tuned and measured on AMD Radeon. Moonlight runs on the device you play from. Start with one client and a 1080p60 SDR stream, then add HDR, higher frame rates or a virtual display.

**On this page:** [Install](#install-or-run-portable) · [Import a profile](#bring-an-existing-profile) · [Upgrading](#upgrading) · [Open the console](#open-the-console) · [Pair Moonlight](#pair-moonlight-and-start-desktop) · [Stream formats](#choose-your-stream-format) · [Displays and updates](#virtual-displays-and-updates)

## Install or run portable

Download the Windows package from [Butterpollo Releases](https://github.com/RamazanKara/Butterpollo/releases). For rc.24, choose:

| Package | How to start | Best fit |
| --- | --- | --- |
| `butterpollo-setup-2.0.0-rc.24.exe` | Run the installer, then open **Butterpollo** from the Start menu. | Normal use, automatic service startup and virtual displays. |
| `butterpollo-rust-2.0.0-rc.24-windows-x64.zip` | Extract the whole ZIP and open **Start Butterpollo.exe**. | Trying the host with a physical display. |

The installer sets up the host, the Windows service, the virtual display driver and, unless you untick it, the virtual gamepad driver that every emulated controller uses. An upgrade preserves the existing Butterpollo settings and paired devices and can migrate a detected Vibepollo installation. Disconnect streams and close host-launched games before installing.

Portable mode does not install a service. Virtual displays and emulated controllers need their Windows drivers; use the installed service for the bundled virtual-display driver's service access. Keep the extracted libraries and `assets` folder beside the executables.

Run one streaming host on the default ports. If Sunshine, Apollo or Vibepollo is already serving those ports, close that host before starting the portable copy.

## Bring an existing profile

On the first portable launch, **Yes** in the import prompt copies an existing Vibepollo or Apollo profile. Select the folder containing `sunshine.conf`, or the installation folder containing `config\sunshine.conf`. **No** starts a fresh profile.

Import brings across settings, paired devices, identity, the app library and covers. The original profile remains in place. The destination must be empty; Butterpollo refuses to overwrite a populated profile. The prompt appears only when the portable profile has not already been created.

The default profiles are separate:

| How it runs | Profile folder |
| --- | --- |
| Installed Windows service | `%PROGRAMDATA%\Butterpollo\config` |
| Portable launcher | `%LOCALAPPDATA%\ButterpolloRust\config` |

Opening the launcher again returns to the running profile's console. When launched from the installed package, it uses that package's Windows service profile.

## Upgrading

- Disconnect streams and remote monitors, then quit host-launched apps. Setup refuses to stop a Butterpollo host that reports an active stream or app. Vibepollo, Apollo and Sunshine cannot report this, so setup stops them without checking.
- Back up the whole profile folder from the table above (or the previous host's `config` folder). Include `sunshine.conf`, `apps.json`, covers, both identity files in `credentials`, `sunshine_state.json`, `sunshine_credentials.json` if present, and `vibeshine_state.json`. Also back up files named by custom paths in the configuration. Keep this backup private: it contains credentials and device certificates.
- Run the new installer in the existing installation folder, or use **Maintenance → Updates → Install when idle**. Settings, unknown configuration keys, paired devices, credentials, apps, device/display settings and saved session data stay in the profile. Active streams do not survive a restart. The service and web console use the new package; installed drivers remain available. Legacy AMD encoder names such as `amdvce_experimental` still select AMF. A saved Xbox 360 or DualShock 4 (ViGEmBus) controller type from an earlier version loads as the VHF Xbox One or DualShock 4 pad; an installed ViGEmBus is no longer used.
- For a portable update, close the portable host and extract the complete new ZIP into a separate folder. The launcher reuses `%LOCALAPPDATA%\ButterpolloRust\config`; keep the old ZIP and your profile backup until the new version works. Portable mode does not install a service or drivers. Avoid opening an older ZIP against a newer profile.
- If setup cannot identify one source profile, or Butterpollo already has settings alongside another host's profile, resolve that choice before removing either host. Imports preserve recognized files and unknown fields; old logs and oversized or linked optional files can be skipped, so keep the original backup.
- Check that the console opens, existing clients connect, apps and covers appear, and display/controller settings still work. Restart Playnite if its connector was updated. If an update fails, keep the profile's `updates` folder and `update-result.json` for recovery. Installer downgrades are refused. Normal uninstall keeps the profile and drivers; **factory reset** and **remove drivers** explicitly delete them.

## Open the console

The launcher opens **https://localhost:47990** on the host PC. A custom base port changes the console port too. The console uses a local self-signed certificate, so the browser may show a certificate warning; check that you are opening your own host's address. Create the local administrator account if prompted; initial account setup must happen on the host PC.

On **Overview**, check **Host readiness** for the video encoder, virtual display, audio and screen capture. An available encoder and an active physical display are enough for the first desktop stream. A virtual display showing **Off** is expected when you are streaming a physical monitor.

For a fresh configuration, leave **Settings → Video → Capture method** on **Automatic**. This prefers Windows Graphics Capture (WGC), with Desktop Duplication as its startup fallback. **Copy and convert on a compute queue** is enabled by default on the supported AMD path. Imported explicit capture choices remain in effect.

## Pair Moonlight and start Desktop

1. Open Moonlight on a device on the same local network. Select the host, or add the PC by its local IP address if discovery does not find it.
2. Moonlight displays a four-digit PIN. In Butterpollo's **Devices** page, enter that PIN for the pending device and select **Pair**.
3. Wait for pairing to finish on the client. The device then appears under **Paired devices**.
4. Set Moonlight to **1920×1080 at 60 FPS**, with HDR off for this first check. Use H.264, or HEVC if the client supports hardware decoding.
5. Launch **Desktop**. Check moving windows, sound and input before increasing resolution, frame rate or bitrate.

If pairing succeeds but launching is denied, check the device's enabled state and **List apps**, **View streams** and **Launch apps** permissions in **Devices → Edit**.

## Choose your stream format

| Choice | Use it when |
| --- | --- |
| H.264 | Establishing an SDR baseline or using a client without newer hardware decoders. |
| HEVC | The client supports HEVC; its ten-bit profile supports HDR. |
| AV1 | Both the host encoder and client decoder support AV1; its ten-bit profile also supports HDR. |
| PyroWave | You want full-resolution chroma, including 10-bit HDR 4:4:4, with [Nonary's compatible Moonlight client](https://github.com/Nonary/moonlight-qt) and a fast wired LAN. Start around 399 Mbps for 1080p60, with network headroom. |

Standard Moonlight supports H.264, HEVC and AV1. The exact Moonlight PC 6.2.0 application has recorded codec, reconnect and AMD AV1 crop checks; the [compatibility matrix](../rust/PARITY.md) gives their scope. PyroWave uses a separate codec path and needs substantially more bandwidth; its client can calibrate the connection before streaming.

PyroWave's recommended rates from synthetic desktop and game tests on AMD are **277 Mbps at 720p60, 399 Mbps at 1080p60 and 1593 Mbps at 4K60**. Quality depends on the picture. The stream card warns below those rates and uses a stronger warning below the severe-loss floors of **139, 187 and 747 Mbps** respectively. A rate above the floor alone is not a clean-picture target. Leave headroom for packet overhead and recovery data; 4K60 needs more than gigabit Ethernet. Use HEVC or AV1 when the client or network cannot carry the rate. [Measurements and limits](configuration.md#capture-and-video). NVIDIA users should use [Vibepollo](https://github.com/Nonary/Vibepollo).

Moonlight's **YUV 4:4:4** option needs an encoder that produces 4:4:4. AMD Radeon GPUs encode H.264, HEVC and AV1 in 4:2:0 only, the RX 9000 series included, so with an AMD host Moonlight streams 4:2:0; Moonlight PC warns that the host doesn't support YUV 4:4:4. On AMD, PyroWave is the way to get full-resolution chroma, in SDR and in 10-bit HDR. Native NVENC streams 4:4:4 on NVIDIA GPUs that support it.

For HDR, enable HDR in the client and confirm that the **streamed display** supports and enables HDR in Windows. In **Settings → HDR**, **Display HDR: Match the stream** and **HDR request: Follow the device** are the normal starting choices. A forced display HDR setting does not change an SDR stream into an HDR stream. Enable the game's own HDR mode when available.

The Overview stream card shows the negotiated codec and an **HDR** badge. Check the picture on the actual client display as well. The published native HDR tests verify captured and decoded pixels; they do not calibrate a TV or establish every client's HDR rendering. See [HDR troubleshooting](troubleshooting.md#hdr-looks-washed-out-too-bright-or-different-between-clients).

## Virtual displays and updates

In **Settings → Display**, select **Virtual display: One for each device** or **One shared by all devices** when you want a host-created screen. Choose **Extended** to add it beside the physical monitors; other layouts can make it primary or disable other displays. Per-app and per-device overrides can change the effective choice. The [configuration guide](configuration.md) explains those policies.

Updates notify you first. **Maintenance → Updates** offers **Check now** and, for the installed service, **Install when idle**; the tray menu also has **Check for updates**. **Settings → General → Install updates automatically** is off by default. Enable **Include pre-releases** to receive release candidates.

Installation waits for streams, pending connections, remote monitors and host apps to stop, then for one minute of idle time. A disconnected Desktop session can still have an app open: quit it from Moonlight or the console if an update stays queued. Portable users download the new ZIP from the release page.

For measured performance and hardware coverage, see [Performance](performance.md) and [Compatibility](../rust/PARITY.md).
