# Configuration

[Docs](README.md) · [Getting started](getting-started.md) · [Troubleshooting](troubleshooting.md)

Configure the current **Windows Rust host** in the Butterpollo console, normally at **https://localhost:47990**. Open **Settings** and search by a setting's name or configuration key. Resolution, stream frame rate, bitrate and codec are normally chosen in the client.

Start with the defaults below. Settings imported from an older installation keep their saved values; upgrading does not reset them to these defaults.

## Defaults worth knowing

| Setting / key | Default | What it means |
| --- | --- | --- |
| Capture method — `capture` | `auto` | Prefers Windows Graphics Capture (WGC), with Desktop Duplication (DDX) as fallback. |
| Compute conversion — `gpu_compute_conversion` | `true` | Uses a separate compute queue for supported AMD capture and colour conversion paths. |
| Encode from GPU memory — `wgc_direct_encoder_input` | `true` | Keeps captured frames on the GPU instead of copying them through system memory. |
| Encoder — `encoder` | `auto` | Selects an available encoder for the capture GPU. |
| AMD usage — `amd_usage` | `ultralowlatency` | AMD's lowest-latency preset. The driver also turns on its internal low-latency mode, and the lowest latency for AV1. |
| AMD quality — `amd_quality` | `speed` | Favours low encoding latency. |
| Frame pacing — `frame_pacing` | `arrival` | Encodes arriving frames, limited to the requested stream rate. |
| Video packet pacing — `pacing_max_bitrate_kbps` | `0` | Automatic: twice the encoder bitrate on confirmed Wi-Fi or mobile routes; up to 800 Mbps on other routes. PyroWave keeps its separate bandwidth policy. |
| HEVC / AV1 support — `hevc_mode`, `av1_mode` | `0` | Automatic capability detection; only working profiles are offered. |
| PyroWave — `pyrowave` | `true` | Offers PyroWave to clients that support it. |
| Virtual display — `virtual_display_mode` | Windows 11: `per_client`; Windows 10: `disabled` | A display for each device on Windows 11; a physical monitor on Windows 10. Requires the virtual display driver when enabled. |
| Virtual display layout — `virtual_display_layout` | `exclusive` | Turns other displays off while the virtual display is used. |
| Physical display preparation — `dd_configuration_option` | `verify_only` | Requires the selected monitor to be active; resolution, refresh and HDR policies are separate. |
| Display HDR — `dd_hdr_option` | `auto` | Matches the streamed display's HDR state to the stream. |
| Limit every stream — `frame_limiter_enable` | `false` | Manual limiting is off; virtual-display automatic limiting can still apply. |
| Limiter — `frame_limiter_provider` | `auto` | Uses RTSS when installed, otherwise the NVIDIA driver where available. |
| Virtual display refresh — `frame_limiter_auto_virtual_framegen` | `legacy` | Runs the virtual display at twice the stream rate and enables automatic frame limiting. |
| Update checks — `update_check_interval` | `86400` seconds | Checks once a day. |
| Automatic installation — `auto_update` | `false` | Announces available updates; installation is opt-in. |
| Include pre-releases — `notify_pre_releases` | `false` | Release candidates are offered only when enabled. |

These are defaults for **unset** keys, not a list of values you must copy into a configuration file.

## Capture and video

**Keep Capture method on Automatic and compute conversion enabled** for normal use. On supported AMD hardware, the compute path lets capture and colour conversion run alongside graphics work. Unsupported textures can fall back to the graphics path. The internal `wgc_compute_copy` switch also defaults to `true`; it is primarily useful for controlled troubleshooting.

The installed service runs WGC through a helper in the signed-in user's session. WGC can fall back to DDX when unavailable. Selecting a particular capture method is useful when investigating a problem; check **Logs** to see which path actually opened.

H.264, HEVC and AV1 depend on the encoder and client. For `hevc_mode` and `av1_mode`, the choices are `0` Automatic, `1` Off, `2` SDR only and `3` SDR and HDR. Restart after changing advertised codec support. There is no 4:4:4 switch: the host offers 4:4:4 for a codec only when the startup check encodes it on a hardware encoder. AMD's encoder makes 4:2:0 only, so an AMD host offers 4:4:4 through PyroWave alone.

For **native HDR**, enable HDR in the client and use an HDR-capable display and encoder path. Leave **Display HDR** on Automatic so Butterpollo can set the source display appropriately. **10-bit SDR instead of HDR** (`prefer_sdr_10bit`, default `false`) deliberately keeps the stream in SDR; leave it off when you want HDR. RTX HDR is a separate SDR-to-HDR conversion feature, with its own hardware requirements.

**PyroWave needs a compatible client** and much more bandwidth than conventional codecs. Synthetic desktop and game measurements on AMD give two warning levels. The floor warns about severe detail loss; the recommendation targets clean pictures in those tests. Passing the floor alone does not mean a clean picture.

| Stream | Severe-loss floor | Recommended |
| --- | ---: | ---: |
| 720p60 | 139 Mbps | 277 Mbps |
| 1080p60 | 187 Mbps | 399 Mbps |
| 4K60 | 747 Mbps | 1593 Mbps |

These values are rounded up to whole Mbps. Below 1080p the floor/recommendation are 2.5/5 bits per pixel per frame; at 1080p and above they are 1.5/3.2. The smaller desktop's one-pixel text needed more bits per pixel. Rates scale with pixel count and frame rate within each band: halve them at 30 fps, double them at 120 fps. Other sizes are estimates; actual quality depends on text size, textures and the picture. The console stream card uses red below the floor and amber below the recommendation, and the log distinguishes both. [Method, criteria and measurements](../rust/PERFORMANCE.md#october-7-pyrowave-bitrate-from-representative-pictures).

Set the bitrate in Moonlight and leave network headroom for packet overhead and recovery data. The 4K60 recommendation needs more than gigabit Ethernet. Client and host limits still apply: stream setup allows up to 2 Gbps, while the client's runtime `/bitrate` endpoint caps changes at 500 Mbps. If the client or network cannot carry the recommended rate, use HEVC or AV1. Enabling PyroWave does not force ordinary Moonlight clients to use it. NVIDIA users should use [Vibepollo](https://github.com/Nonary/Vibepollo).

**VRR is client-negotiated.** A VRR request can use a 1000 Hz virtual display when automatic virtual refresh is enabled. That is the host's virtual display rate, not a claim that your TV or monitor refreshes at 1000 Hz. Client and display support still matter.

**Packet pacing limits video bursts.** With `pacing_max_bitrate_kbps = 0`, H.264, HEVC and AV1 use twice the negotiated encoder bitrate when Windows identifies the host's route as Wi-Fi or mobile broadband, bounded to 1–800 Mbps. Ethernet, loopback, VPN, Tailscale and unknown routes retain the rc.19 default of 800 Mbps. A known physical Ethernet link caps that at 80% of its reported speed. For virtual Ethernet adapters such as Hyper-V vSwitches, the host tries to resolve the physical adapter through Windows' interface stack. Missing or ambiguous bindings keep the wired default. The host cannot detect a wireless client behind a wired access point from its own Ethernet route.

Set `pacing_max_bitrate_kbps` to a positive value in **kbps** to override the automatic policy, including for PyroWave. For example, `120000` paces at 120 Mbps. The existing floor of 110% of the stream bitrate still applies, and a known Ethernet link can lower the limit to 80% of link speed. This changes packet spacing, not the encoded bitrate; reconnect after saving. PyroWave's automatic policy retains 95% of a known Ethernet link, or 1.25 times its per-frame wire demand (at least 110% of the stream bitrate) on other routes, so its high-bandwidth intra frames are not restricted by the conventional-codec default. [Measurements and pacing math](../rust/PERFORMANCE.md#october-7-wi-fi-and-unknown-route-pacing).

**Leave AMF's low-latency mode and AV1 latency mode on Driver default.** With the default usage, ultra-low latency, AMD's driver already runs H.264 and HEVC in its internal low-latency mode and AV1 at its lowest latency. Forcing them (`amd_lowlatency_mode`, `amd_av1_latency_mode`) gave the same encode time and the same output size on an RX 7900 XT. They only matter after choosing the Low latency or Transcoding usage, which leave them off: forcing them there saved about 0.4 ms per HEVC frame and 1.3 ms per AV1 frame at 1440p. Forcing the low-latency mode has frozen HEVC encoding on RX 9000 cards (video stops while audio plays), so it stays opt-in. [Measurements →](../rust/PERFORMANCE.md#october-7-amf-low-latency-mode-and-av1-latency-mode)

**Adaptive quantization (`amd_vbaq`) is off for H.264 and on for HEVC and AV1 when unset.** On two game clips at 1440p120 and 20 Mbps, turning it off for H.264 raised VMAF by 1.0 and 9.4 points at the same encode time; for HEVC and AV1 it made no clear difference. Set it to On to use it for H.264 too, or Driver default to leave it to the driver.

**Leave split-frame encoding on Automatic.** AMF can split one HEVC or AV1 frame across a Radeon's two encoder engines (`amd_split_frame`), and the driver decides whether it does. Automatic asks for it only when the GPU has two engines and the driver has it off, as the original host did; On asks for it whenever there are two engines, and Off turns it off. On an RX 7900 XT the driver already has it on, and on, off and unset encoded every frame in the same time, from 1080p to 7680×2160: one stream used one engine either way. H.264 has no such property, and GPUs with one engine, such as the RX 9070 XT, get nothing written. [Measurements →](../rust/PERFORMANCE.md#october-7-amf-split-frame-encoding)

**AMF rate-control limits are optional.** In **Settings → Encoders → AMD AMF**, the advanced controls below use the client's requested bitrate and frame rate. All default to `0`, which leaves the corresponding property to the driver. Reconnect after saving. An unsupported explicit request is reported as an AMF setting error; the effective values appear in `AMF encoder settings`.

| Key | Values | Meaning |
| --- | --- | --- |
| `amd_peak_bitrate_ratio` | `0`, or `1`–`2` | Peak bitrate as a multiple of the stream bitrate; the console offers 1×, 1.5× and 2×. This is not an individual-frame cap. |
| `amd_vbv_buffer_frames` | `0`, or `0.5`–`2` | Rate-control buffer in nominal frame budgets. One budget is bitrate divided by frame rate, including fractional rates. |
| `amd_max_frame_size` | `0`, or `1`–`8` | Requested maximum encoded frame size in nominal frame budgets, including recovery keyframes. Uses `MaxAUSize` for H.264, `HevcMaxAUSize` for HEVC and `Av1MaxCompressedFrameSize` for AV1. |

These are encoder bit budgets, not extra queued frames or packet-pacing settings. A driver can exceed a requested frame cap, especially at startup; a smaller budget can also reduce picture quality. Intra refresh remains client-negotiated and does not replace an explicit recovery-keyframe request. See the [rate-control measurements](../rust/PERFORMANCE.md#october-7-2026-amf-rate-control-and-recovery-keyframes) before changing these controls. NVIDIA users should use [Vibepollo](https://github.com/Nonary/Vibepollo).

`amd_rc` remains `vbr_latency` by default. For an affected AMD stream, `amd_max_frame_size=1` or `2` lets you compare smaller recovery frames against picture quality; `0` restores the driver default. These caps helped on an RX 7900 XT, but were not strict size bounds and were not tested on RDNA4. With frequent recovery requests the tighter cap also reduced actual bit usage and slightly increased HEVC encode time, so it is not enabled automatically. Switching to CBR or shrinking VBV alone did not consistently reduce bursts.

## Displays and RTSS

Under **Settings → Display**, select a physical display or choose a virtual display per device/shared by all devices. **Exclusive** switches other monitors off; choose **Extended** to keep the existing desktop active. The primary and isolated variants control where the virtual display sits in that desktop.

Resolution and refresh policies default to `auto` (`dd_resolution_option`, `dd_refresh_rate_option`). Virtual refresh follows the frame-limiting policy unless a manual or device display mode overrides it. To leave a **physical** monitor's resolution, refresh and HDR unchanged, select Disabled for `dd_configuration_option`.

A disconnect can leave the app and its display available for reconnection. Enable `dd_config_revert_on_disconnect` if you want the display configuration restored on disconnect; it defaults to `false`. Without it, a disconnected device's display is kept while its app runs for `dd_paused_virtual_display_timeout_secs` (default `7200` seconds; `0` keeps it until the app closes). A client that sends nothing for `ping_timeout` (default `10000` ms) ends its stream. **Maintenance** provides display restoration and saved-baseline controls.

Under **Settings → Frame limiting**:

- Keep **Limiter** on Automatic, or select RTSS. Set **RTSS folder** (`rtss_install_path`) only if detection fails; an empty value searches Program Files.
- Butterpollo starts RTSS when a limit is needed and restores the previous limit after disconnect. If RTSS requires administrator access, the installed service can use the signed-in administrator's token; otherwise start RTSS with the required access yourself.
- **Frame limit** (`frame_limiter_fps_limit`) defaults to `0`, meaning the stream rate. RTSS preserves fractional rates such as 59.94 FPS.
- **Virtual display refresh** offers 2× (`legacy`), 4× (`enabled`), 1000 Hz (`vrr`) or Off (`disabled`). Turning **Limit every stream** off does not disable the automatic virtual-display limit. Choose **Limiter → None** (`frame_limiter_provider = none`) to disable all limiting.

## Controllers

**Settings → Input → Controller type** controls which virtual controller games see. Every pad comes from the VHF gamepad driver that the installer brings; no other driver is needed.

`gamepad = auto` matches each client's controller. PlayStation clients get a DualSense, the only pad with adaptive triggers; it also carries motion, battery, touchpad, lightbar and rumble. A game's trigger effects reach the player only if the client forwards them to a real DualSense. Nintendo clients get a Switch Pro. Other clients get an Xbox Series pad, or a DualSense when they have motion sensors or a touchpad and `motion_as_ds4` or `touchpad_as_ds4` is enabled. Both preferences default to enabled, so a Steam Deck reporting motion gets a DualSense.

`vhf_xbox`, `vhf_xbox_one`, `vhf_ds4`, `vhf_ds5` and `vhf_switch` give every client that pad: `vhf_ds5` a DualSense, `vhf_ds4` a DualShock 4 without adaptive triggers. Logs name the profile for each connected controller.

Earlier releases also offered Xbox 360 (`x360`) and DualShock 4 (`ds4`) through ViGEmBus. Since rc.25 Butterpollo changes those settings to `vhf_xbox_one` and `vhf_ds4` when it loads them, in the host configuration, app and device overrides, and configurations imported from Sunshine, Apollo or Vibepollo. An installed ViGEmBus is no longer used and can be uninstalled.

### Steam Deck

A Steam Deck can reach the host as a **real Steam Deck controller**: Butterpollo serves the Deck's own USB controller (Valve 28de:1205) over USB/IP on the host's loopback address, and [usbip-win2](https://github.com/vadimgrn/usbip-win2) attaches it with its signed driver, so Windows sees a Steam Deck plugged into a USB port. Steam on the host then recognises a Steam Deck and applies the Deck's Steam Input layout, with both trackpads, the gyro, the back grips (L4, R4, L5, R5), the Steam and "…" buttons and rumble. Games see whatever Steam Input makes of it, as on a Deck.

- **Settings → Input → Steam Deck and back grips → Steam Deck controller** (`steam_deck_controller`): **Automatic** (`auto`, the default) attaches a Steam Deck when usbip-win2 is installed and Steam is running on the host when the controller connects; **Steam Deck** (`steam_deck`) attaches one whenever usbip-win2 is installed, even without Steam, where only SDL-based games see it; **Virtual pad** (`virtual_pad`) always uses the virtual DualSense or Xbox pad below.
- **Install usbip-win2 on the host** from its [releases](https://github.com/vadimgrn/usbip-win2/releases) (Windows 10 1903 or later). Its setup installs a driver and restarts the USB hubs once, so devices on them reconnect; it recommends a restore point first. Butterpollo looks for `usbip.exe` in `C:\Program Files\USBip` and on `PATH`. Nothing listens beyond `127.0.0.1`.
- If usbip-win2 is missing or the attach fails, the Deck falls back to the virtual pad, and the stream card says why when **Steam Deck** was chosen or an attach failed.

Moonlight passes on the Deck's own controls only when Steam Input is off for Moonlight on the Deck. With it on, Steam on the Deck turns the controls into a virtual pad first: the host still gets a Steam Deck with the buttons, sticks and triggers, but no gyro, trackpads or back grips, because Moonlight never receives them. On the Deck, open Moonlight's controller settings in Steam, choose to disable Steam Input, and reconnect. If the paired device's name says Steam Deck (SteamOS calls it `steamdeck`) and its controller arrives without a gyro, the stream card says this.

What Moonlight can't carry: touching a stick (capacitive), clicking the left trackpad, and the Deck's own orientation quaternion. Moonlight sends trackpad touches only from builds whose SDL reads the Deck's trackpads (SDL 3); older builds send none.

With the virtual pad (no usbip-win2, or **Virtual pad**), Moonlight's Steam controller with gyro gets a DualSense:

- **Gyro and accelerometer** are the pad's motion sensors.
- **Trackpads** share the pad's one touch surface: the left trackpad is its left half and the right trackpad its right half. Clicking the right trackpad is the touchpad click.
- **Back grips** (L4, R4, L5, R5): the virtual pads have no back buttons, so each grip presses what **Settings → Input → Steam Deck and back grips** sets for it: a face button, bumper, a fully pulled trigger, a stick click, Back, Start, Guide, a d-pad direction, the touchpad click or Share/Mute (`back_grip_l4`, `back_grip_r4`, `back_grip_l5`, `back_grip_r5`; values `a`, `b`, `x`, `y`, `lb`, `rb`, `lt`, `rt`, `l3`, `r3`, `back`, `start`, `guide`, `dpad_up`, `dpad_down`, `dpad_left`, `dpad_right`, `touchpad`, `misc`). They default to nothing, and pressing an unmapped grip shows a hint on the stream card. The same settings map an Xbox Elite's paddles (P1 is R4, P3 is L4, P2 is R5, P4 is L5) and a DualSense Edge's back buttons.
- **Steam and "…" buttons** become the PS button and the DualSense's mute button (the touchpad click on a DualShock 4), when Steam on the Deck lets them through to Moonlight.
- **Rumble and battery level** are passed on.

Setting `motion_as_ds4` and `touchpad_as_ds4` both off keeps a Deck's virtual pad an Xbox pad, without its gyro and trackpads.

## Settings for one app or device

Edit an app in **Library** or a paired device in **Devices** to set its display, HDR and other overrides. Leave an override unset to inherit the host configuration.

For general configuration overrides, Butterpollo applies **host settings → device overrides → app overrides**. Only supported stream, input, display and encoder keys are accepted; host-wide network, identity and path settings cannot be overridden per stream.

Display selection has dedicated rules: a device's explicit virtual-display mode takes priority over the app's mode, and its **display mode** (`WIDTHxHEIGHTxREFRESH`) overrides the host's resolution/refresh policy. This does not change the frame rate requested for the encoded stream.

## Save, reconnect and restart

Select **Save changes** to write the configuration. Edits are not saved merely by changing a control or switching categories. Saved stream settings are read when a new stream starts; an existing stream generally keeps its current configuration.

The console shows **“Saved. Some changes apply after the host restarts.”** Startup settings, including network listeners and advertised codec capabilities, need a restart. Use **Restart now** after disconnecting: restarting the host disconnects active streams.

For file-based configuration, the file is still named `sunshine.conf` for compatibility:

| Installation | Default profile folder |
| --- | --- |
| Windows service | `%ProgramData%\Butterpollo\config` |
| Portable host | `%LocalAppData%\ButterpolloRust\config` |
| Explicit profile | Folder passed to `butterpollo.exe --config-dir <folder>` |

The format is `key = value`, one setting per line. Stop the host before editing files directly, then start it again to load them. Prefer the console for ordinary changes; old or unknown keys can be preserved without being implemented by the Rust host.

## Updates

Automatic **checking** is enabled; automatic **installation** is not. Use **Maintenance** to check manually or start an offered update. Enable **Include pre-releases** if you want release candidates.

If you opt into **Install updates automatically**, the installed Windows service waits for one minute without active streams, pending connections, remote-monitor sessions or running host apps. The updater validates the selected installer against the release's size and SHA-256. Portable installations use the release download page instead.

Set `update_check_interval = 0` to disable scheduled checks; manual checks remain available.

## Further reference

- [Feature coverage and hardware limits](../rust/PARITY.md)
- [Measured performance and testing conditions](../rust/PERFORMANCE.md)
- [Console settings definitions](../rust/web/src/lib/settings-schema.ts), including [video](../rust/web/src/lib/schema/video.ts), [display](../rust/web/src/lib/schema/display.ts) and [general settings](../rust/web/src/lib/schema/basics.ts)
- [Configuration parser and allowed overrides](../rust/core/src/config.rs)
- [Historical C++ configuration reference](https://github.com/RamazanKara/Butterpollo/blob/2.0.0-rc.23/docs/legacy/configuration-cpp.md) — preserved for older installations; its defaults and platform advice do not describe the current Rust host.
