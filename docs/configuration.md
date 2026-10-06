# Configuration

[Docs](README.md) · [Getting started](getting-started.md) · [Troubleshooting](troubleshooting.md)

Configure the current **Windows Rust host** in the Butterpollo console, normally at **https://localhost:47990**. Open **Settings** and search by a setting's name or configuration key. Resolution, stream frame rate, bitrate and codec are normally chosen in the client.

Start with the defaults below. Settings imported from an older installation keep their saved values; upgrading does not reset them to these defaults.

## Defaults worth knowing

| Setting / key | Default | What it means |
| --- | --- | --- |
| Capture method — `capture` | `auto` | Prefers Windows Graphics Capture (WGC), with Desktop Duplication (DDX) as fallback. |
| Compute conversion — `gpu_compute_conversion` | `true` | Uses a separate compute queue for supported AMD capture and colour conversion paths. |
| Encode from GPU memory — `wgc_direct_encoder_input` | `true` | Keeps WGC frames on the GPU instead of copying them through system memory. |
| Encoder — `encoder` | `auto` | Selects an available encoder for the capture GPU. |
| AMD quality — `amd_quality` | `speed` | Favours low encoding latency. |
| Frame pacing — `frame_pacing` | `arrival` | Encodes arriving frames, limited to the requested stream rate. |
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

H.264, HEVC and AV1 depend on the encoder and client. For `hevc_mode` and `av1_mode`, the choices are `0` Automatic, `1` Off, `2` SDR only and `3` SDR and HDR. Restart after changing advertised codec support.

For **native HDR**, enable HDR in the client and use an HDR-capable display and encoder path. Leave **Display HDR** on Automatic so Butterpollo can set the source display appropriately. **10-bit SDR instead of HDR** (`prefer_sdr_10bit`, default `false`) deliberately keeps the stream in SDR; leave it off when you want HDR. RTX HDR is a separate SDR-to-HDR conversion feature, with its own hardware requirements.

**PyroWave needs a compatible client** and much more bandwidth than conventional codecs—typically hundreds of Mbps. Enabling it does not force ordinary Moonlight clients to use it. Start with HEVC or AV1 for constrained networks.

**VRR is client-negotiated.** A VRR request can use a 1000 Hz virtual display when automatic virtual refresh is enabled. That is the host's virtual display rate, not a claim that your TV or monitor refreshes at 1000 Hz. Client and display support still matter.

## Displays and RTSS

Under **Settings → Display**, select a physical display or choose a virtual display per device/shared by all devices. **Exclusive** switches other monitors off; choose **Extended** to keep the existing desktop active. The primary and isolated variants control where the virtual display sits in that desktop.

Resolution and refresh policies default to `auto` (`dd_resolution_option`, `dd_refresh_rate_option`). Virtual refresh follows the frame-limiting policy unless a manual or device display mode overrides it. To leave a **physical** monitor's resolution, refresh and HDR unchanged, select Disabled for `dd_configuration_option`.

A disconnect can leave the app and its display available for reconnection. Enable `dd_config_revert_on_disconnect` if you want the display configuration restored on disconnect; it defaults to `false`. **Maintenance** provides display restoration and saved-baseline controls.

Under **Settings → Frame limiting**:

- Keep **Limiter** on Automatic, or select RTSS. Set **RTSS folder** (`rtss_install_path`) only if detection fails; an empty value searches Program Files.
- Butterpollo starts RTSS when a limit is needed and restores the previous limit after disconnect. If RTSS requires administrator access, the installed service can use the signed-in administrator's token; otherwise start RTSS with the required access yourself.
- **Frame limit** (`frame_limiter_fps_limit`) defaults to `0`, meaning the stream rate. RTSS preserves fractional rates such as 59.94 FPS.
- **Virtual display refresh** offers 2× (`legacy`), 4× (`enabled`), 1000 Hz (`vrr`) or Off (`disabled`). Turning **Limit every stream** off does not disable the automatic virtual-display limit. Choose **Limiter → None** (`frame_limiter_provider = none`) to disable all limiting.

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
- [Historical C++ configuration reference](legacy/configuration-cpp.md) — preserved for older installations; its defaults and platform advice do not describe the current Rust host.
