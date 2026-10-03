// Settings for the categories named in settings-schema.ts.
import type { ConfigValue } from '../api';
import type { Setting } from '../settings-types';

type Values = Record<string, ConfigValue>;

/** A text setting as the host reads it: unset or blank means the default. */
function text(values: Values, key: string, fallback: string): string {
  const value = values[key];
  if (value === undefined || value === null) return fallback;
  const trimmed = String(value).trim();
  return trimmed === '' ? fallback : trimmed;
}

/** A switch as the host reads it, including the spellings Vibepollo wrote. */
function on(values: Values, key: string, fallback: boolean): boolean {
  const value = values[key] ?? fallback;
  if (typeof value === 'boolean') return value;
  const word = String(value).trim().replace(/^"(.*)"$/, '$1').toLowerCase();
  if (['true', 'yes', '1', 'enable', 'enabled', 'on'].includes(word)) return true;
  if (['false', 'no', '0', 'disable', 'disabled', 'off'].includes(word)) return false;
  return fallback;
}

/** Whether the limiter may use RTSS. The host reads unknown providers as automatic. */
function rtss(values: Values): boolean {
  const name = text(values, 'frame_limiter_provider', 'auto')
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '');
  return !['none', 'disabled', 'nvidia', 'nvidiacontrolpanel', 'nvcp'].includes(name);
}

const SCALES = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

export const settings: Setting[] = [
  // Display
  {
    key: 'output_name',
    label: 'Display',
    description:
      'The monitor to stream when no virtual display is used. Empty streams the primary display; a device or app can choose its own.',
    category: 'display',
    group: 'Streamed display',
    control: { kind: 'display' },
    default: '',
  },
  {
    key: 'virtual_display_mode',
    label: 'Virtual display',
    description:
      'Whether streams use a virtual display sized for the device instead of a monitor. Unset, it is one for each device on Windows 11 and off on Windows 10; a device’s or app’s own choice wins.',
    category: 'display',
    group: 'Streamed display',
    control: {
      kind: 'select',
      options: [
        { value: 'disabled', label: 'Off, stream a monitor' },
        { value: 'per_client', label: 'One for each device' },
        { value: 'shared', label: 'One shared by all devices' },
      ],
    },
    default: 'per_client',
  },
  {
    key: 'virtual_display_layout',
    label: 'Virtual display layout',
    description:
      'How the virtual display joins the desktop. Exclusive turns the other monitors off while streaming; isolated places it far from them so the mouse cannot wander onto it.',
    category: 'display',
    group: 'Streamed display',
    control: {
      kind: 'select',
      options: [
        { value: 'exclusive', label: 'Exclusive' },
        { value: 'extended', label: 'Extended' },
        { value: 'extended_primary', label: 'Extended, primary' },
        { value: 'extended_isolated', label: 'Extended, isolated' },
        { value: 'extended_primary_isolated', label: 'Extended, primary and isolated' },
      ],
    },
    default: 'exclusive',
  },
  {
    key: 'dd_virtual_display_scale',
    label: 'Virtual display scale',
    description:
      'The Windows scale set on a virtual display. Recommended picks one from the resolution, such as 125% at 1080p; Keep leaves the scale Windows remembers for that display.',
    category: 'display',
    group: 'Streamed display',
    control: {
      kind: 'select',
      options: [
        { value: '0', label: 'Keep' },
        { value: '-1', label: 'Recommended' },
        ...SCALES.map((scale) => ({ value: String(scale), label: `${scale}%` })),
      ],
    },
    default: 0,
  },
  {
    key: 'dd_activate_virtual_display',
    label: 'Activate a virtual display',
    description:
      'Streams use a virtual display even when Virtual display is off, unless the device asks for none. It has no effect while Virtual display is on.',
    category: 'display',
    group: 'Streamed display',
    control: { kind: 'toggle' },
    default: false,
    advanced: true,
  },
  {
    key: 'dd_virtual_display_permanent_count',
    label: 'Permanent virtual displays',
    description:
      'Virtual displays the driver keeps even when nothing streams, from 0 to 4. Applied when the host starts; while unset, the driver keeps its current count.',
    category: 'display',
    group: 'Streamed display',
    control: { kind: 'number', min: 0, max: 4, step: 1 },
    default: 0,
    restart: true,
    advanced: true,
  },
  {
    key: 'dd_configuration_option',
    label: 'Monitor setup',
    description:
      'What happens to the streamed monitor when the stream does not use a virtual display. Leaving the monitors alone also skips resolution, refresh rate and HDR changes on it.',
    category: 'display',
    group: 'Streamed display',
    control: {
      kind: 'select',
      options: [
        { value: 'verify_only', label: 'Only check that it’s on' },
        { value: 'ensure_active', label: 'Turn it on' },
        { value: 'ensure_primary', label: 'Turn it on and make it primary' },
        { value: 'ensure_only_display', label: 'Turn it on and the others off' },
        { value: 'disabled', label: 'Leave the monitors alone' },
      ],
    },
    default: 'verify_only',
  },
  {
    key: 'dd_resolution_option',
    label: 'Resolution',
    description:
      'The resolution set on the streamed display. Keeping the current one applies to monitors only; a virtual display matches the device unless a fixed resolution is set.',
    category: 'display',
    group: 'Resolution and refresh rate',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Match the device' },
        { value: 'disabled', label: 'Keep the current resolution' },
        { value: 'manual', label: 'Use a fixed resolution' },
      ],
    },
    default: 'auto',
  },
  {
    key: 'dd_manual_resolution',
    label: 'Fixed resolution',
    description: 'WIDTHxHEIGHT, for example 2560x1440. The width can be 320 to 7680 and the height 200 to 4320.',
    category: 'display',
    group: 'Resolution and refresh rate',
    control: { kind: 'text', placeholder: '2560x1440', mono: true },
    default: '',
    visibleWhen: (values) => text(values, 'dd_resolution_option', 'auto') === 'manual',
  },
  {
    key: 'dd_refresh_rate_option',
    label: 'Refresh rate',
    description:
      'The refresh rate set on the streamed display. On a virtual display, matching follows Virtual display refresh under Frame limiting, and the highest and current choices apply to monitors only.',
    category: 'display',
    group: 'Resolution and refresh rate',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Match the stream' },
        { value: 'prefer_highest', label: 'Highest available' },
        { value: 'disabled', label: 'Keep the current refresh rate' },
        { value: 'manual', label: 'Use a fixed refresh rate' },
      ],
    },
    default: 'auto',
  },
  {
    key: 'dd_manual_refresh_rate',
    label: 'Fixed refresh rate',
    description:
      'In hertz, with up to three decimals such as 59.94 or 119.88. On a virtual display it replaces the faster automatic refresh.',
    category: 'display',
    group: 'Resolution and refresh rate',
    control: { kind: 'number', min: 1, max: 1000, step: 0.001, unit: 'Hz' },
    default: '',
    visibleWhen: (values) => text(values, 'dd_refresh_rate_option', 'auto') === 'manual',
  },
  {
    key: 'dd_mode_remapping',
    label: 'Mode substitutions',
    description:
      'Changes a requested mode before it is applied: the first entry whose requested_resolution and requested_fps match sets final_resolution and final_refresh_rate. The mixed list is used when resolution and refresh rate both match the device, resolution_only or refresh_rate_only when only that one does.',
    category: 'display',
    group: 'Resolution and refresh rate',
    control: { kind: 'json' },
    default: { mixed: [], resolution_only: [], refresh_rate_only: [] },
    visibleWhen: (values) =>
      text(values, 'dd_resolution_option', 'auto') === 'auto' ||
      text(values, 'dd_refresh_rate_option', 'auto') === 'auto',
    advanced: true,
  },
  {
    key: 'dd_config_revert_on_disconnect',
    label: 'Restore displays on disconnect',
    description:
      'When a device disconnects but its app keeps running, restore the displays after the delay below instead of keeping them for a reconnect.',
    category: 'display',
    group: 'After a stream',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'dd_config_revert_delay',
    label: 'Restore delay',
    description: 'How long to wait after a device disconnects before restoring the displays.',
    category: 'display',
    group: 'After a stream',
    control: { kind: 'number', min: 0, step: 100, unit: 'ms' },
    default: 3000,
    visibleWhen: (values) => on(values, 'dd_config_revert_on_disconnect', false),
  },
  {
    key: 'dd_paused_virtual_display_timeout_secs',
    label: 'Keep a disconnected display for',
    description:
      'How long a disconnected device’s display is kept while its app runs, so the device can resume. 0 keeps it until the app closes.',
    category: 'display',
    group: 'After a stream',
    control: { kind: 'number', min: 0, step: 1, unit: 's' },
    default: 7200,
    visibleWhen: (values) => !on(values, 'dd_config_revert_on_disconnect', false),
  },
  {
    key: 'dd_always_restore_from_golden',
    label: 'Restore the saved layout',
    description:
      'After a stream, return the displays to the layout saved under Maintenance instead of the layout from before the stream. The saved layout is used only while every display it names is connected.',
    category: 'display',
    group: 'After a stream',
    control: { kind: 'toggle' },
    default: true,
  },
  {
    key: 'dd_snapshot_exclude_devices',
    label: 'Displays the saved layout skips',
    description:
      'A JSON list of display device IDs that restoring the saved layout leaves as they are, such as a dummy plug.',
    category: 'display',
    group: 'After a stream',
    control: { kind: 'json' },
    default: [],
    advanced: true,
  },
  {
    key: 'dd_snapshot_restore_hotkey',
    label: 'Restore hotkey',
    description:
      'A key on this PC that ends every stream, removes the virtual displays and restores the display layout, for when a stream leaves the screens unusable. F1 to F24, a letter, a digit or a virtual-key code such as 0x2E. Empty turns it off.',
    category: 'display',
    group: 'After a stream',
    control: { kind: 'text', placeholder: 'Off', mono: true },
    default: '',
  },
  {
    key: 'dd_snapshot_restore_hotkey_modifiers',
    label: 'Restore hotkey modifiers',
    description: 'Keys held with the restore hotkey: ctrl, alt, shift and win, joined with +.',
    category: 'display',
    group: 'After a stream',
    control: { kind: 'text', placeholder: 'ctrl+alt+shift', mono: true },
    default: 'ctrl+alt+shift',
    visibleWhen: (values) => String(values.dd_snapshot_restore_hotkey ?? '').trim() !== '',
  },
  {
    key: 'remote_monitor_mute_audio',
    label: 'Mute remote monitors',
    description: 'Sends picture and input, but no audio, to a device used as a remote monitor.',
    category: 'display',
    group: 'Remote monitors',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'remote_monitor_disconnect_on_stream_end',
    label: 'Remove when its stream ends',
    description:
      'Removes a device’s remote monitor when its stream ends. When off, the monitor stays so the device can resume.',
    category: 'display',
    group: 'Remote monitors',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'remote_monitor_disconnect_on_client_disconnect',
    label: 'Remove when the device disconnects',
    description:
      'Removes the remote monitor as soon as the device’s connection drops, even if it was kept for resuming.',
    category: 'display',
    group: 'Remote monitors',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'remote_monitor_terminate_on_first_request',
    label: 'Quit the game on the first request',
    description:
      'Lets another device quit the running game with its first request. When off, it must ask again within 60 seconds; the device that started the game is never asked twice.',
    category: 'display',
    group: 'Remote monitors',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'remote_monitor_confirm_app_replacement',
    label: 'Confirm before replacing a running app',
    description:
      'Refuses the first request to start a different app while one runs, so Moonlight can warn first; starting it again within 60 seconds replaces the app. Remote monitors and remote input still connect without asking.',
    category: 'display',
    group: 'Remote monitors',
    control: { kind: 'toggle' },
    default: true,
  },

  // Frame limiting
  {
    key: 'frame_limiter_enable',
    label: 'Limit every stream',
    description:
      'Caps the game’s frame rate on every stream, not only on virtual displays. Virtual-display streams are already limited unless Virtual display refresh is off.',
    category: 'frame-limiting',
    group: 'Frame limiter',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'frame_limiter_provider',
    label: 'Limiter',
    description:
      'What enforces the limit. Automatic and RTSS use RTSS when it is installed and the NVIDIA driver otherwise; None turns off all limiting.',
    category: 'frame-limiting',
    group: 'Frame limiter',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Automatic' },
        { value: 'rtss', label: 'RTSS' },
        { value: 'nvidia-control-panel', label: 'NVIDIA driver' },
        { value: 'none', label: 'None' },
      ],
    },
    default: 'auto',
  },
  {
    key: 'frame_limiter_fps_limit',
    label: 'Frame limit',
    description:
      'The frame rate games are capped at; 0 uses the stream’s frame rate. RTSS keeps decimals such as 59.94, the NVIDIA driver rounds them.',
    category: 'frame-limiting',
    group: 'Frame limiter',
    control: { kind: 'number', min: 0, max: 1000, step: 0.001, unit: 'FPS' },
    default: 0,
  },
  {
    key: 'frame_limiter_auto_virtual_framegen',
    label: 'Virtual display refresh',
    description:
      'How fast a virtual display refreshes; a faster display captures each game frame sooner after it is drawn. Every choice but Off also caps games at the frame limit and uses 1000 Hz when the device asks for VRR.',
    category: 'frame-limiting',
    group: 'Frame limiter',
    control: {
      kind: 'select',
      options: [
        { value: 'legacy', label: 'Twice the stream rate' },
        { value: 'enabled', label: 'Four times the stream rate' },
        { value: 'vrr', label: '1000 Hz' },
        { value: 'disabled', label: 'Off' },
      ],
    },
    default: 'legacy',
  },
  {
    key: 'frame_limiter_disable_vsync',
    label: 'Turn off vertical sync',
    description:
      'Turns off vertical sync in the NVIDIA driver during streams. The driver setting is restored after the last stream.',
    category: 'frame-limiting',
    group: 'Frame limiter',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'rtss_install_path',
    label: 'RTSS folder',
    description: 'Where RivaTuner Statistics Server is installed. Empty looks for it in Program Files.',
    category: 'frame-limiting',
    group: 'RTSS',
    control: { kind: 'text', placeholder: 'C:\\Program Files (x86)\\RivaTuner Statistics Server', mono: true },
    default: '',
    visibleWhen: rtss,
  },
  {
    key: 'rtss_frame_limit_type',
    label: 'RTSS limit mode',
    description:
      'How RTSS paces frames. Virtual-display streams, unless Virtual display refresh is off, and frame-generation games on a monitor use NVIDIA Reflex instead, or front edge sync where Reflex is unavailable.',
    category: 'frame-limiting',
    group: 'RTSS',
    control: {
      kind: 'select',
      options: [
        { value: 'async', label: 'Async' },
        { value: 'front edge sync', label: 'Front edge sync' },
        { value: 'back edge sync', label: 'Back edge sync' },
        { value: 'nvidia reflex', label: 'NVIDIA Reflex' },
      ],
    },
    default: 'async',
    visibleWhen: rtss,
  },
  {
    key: 'rtss_allow_virtual_display_override',
    label: 'Use this mode on virtual displays',
    description:
      'Applies the RTSS limit mode to virtual-display streams instead of NVIDIA Reflex. Apps set to game-provided frame generation keep Reflex, because other modes can add a lot of latency there.',
    category: 'frame-limiting',
    group: 'RTSS',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: rtss,
    advanced: true,
  },

  // HDR
  {
    key: 'dd_hdr_option',
    label: 'Display HDR',
    description:
      'Turns HDR on the streamed display on or off to match the stream. While RTX HDR converts the picture, the display stays in SDR.',
    category: 'hdr',
    group: 'Display',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Match the stream' },
        { value: 'disabled', label: 'Don’t change HDR' },
      ],
    },
    default: 'auto',
  },
  {
    key: 'dd_hdr_request_override',
    label: 'HDR request',
    description:
      'Overrides the device’s HDR request for the display, for devices or games that report it wrongly. The stream itself still follows the device.',
    category: 'hdr',
    group: 'Display',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Follow the device' },
        { value: 'force_on', label: 'Always on' },
        { value: 'force_off', label: 'Always off' },
      ],
    },
    default: 'auto',
    visibleWhen: (values) => text(values, 'dd_hdr_option', 'auto') !== 'disabled',
  },
  {
    key: 'vulkan_hdr_layer',
    label: 'Vulkan HDR on virtual displays',
    description:
      'Registers a Vulkan layer so Vulkan games can use HDR on a virtual display. Turn it off if Vulkan games crash.',
    category: 'hdr',
    group: 'Display',
    control: { kind: 'toggle' },
    default: true,
    advanced: true,
  },
  {
    key: 'rtx_hdr',
    label: 'RTX HDR',
    description:
      'Turns a game’s SDR picture into HDR on NVIDIA GPUs. It runs only when an app or device turns it on and the device asks for HDR, so this switch alone converts nothing.',
    category: 'hdr',
    group: 'RTX HDR',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'rtx_hdr_sdr_brightness',
    label: 'SDR brightness',
    description: 'Raises the brightness of the SDR picture in an RTX HDR stream, from 0 to 100. 0 leaves it unchanged.',
    category: 'hdr',
    group: 'RTX HDR',
    control: { kind: 'number', min: 0, max: 100, step: 1 },
    default: 0,
  },
  {
    key: 'rtx_hdr_contrast',
    label: 'Contrast',
    description:
      'From −100 to 100; 0 is neutral. A value in the game’s NVIDIA driver profile wins, and the desktop always uses 0.',
    category: 'hdr',
    group: 'RTX HDR',
    control: { kind: 'number', min: -100, max: 100, step: 1 },
    default: 0,
  },
  {
    key: 'rtx_hdr_saturation',
    label: 'Saturation',
    description:
      'From −100 to 100; 0 is neutral. A value in the game’s NVIDIA driver profile wins, and the desktop always uses 0.',
    category: 'hdr',
    group: 'RTX HDR',
    control: { kind: 'number', min: -100, max: 100, step: 1 },
    default: 0,
  },
  {
    key: 'rtx_hdr_middle_gray',
    label: 'Middle gray',
    description:
      'Mid-tone brightness from 10 to 100; higher is brighter. A value in the game’s NVIDIA driver profile wins, and the desktop always uses 50.',
    category: 'hdr',
    group: 'RTX HDR',
    control: { kind: 'number', min: 10, max: 100, step: 1 },
    default: 50,
  },
  {
    key: 'rtx_hdr_peak_brightness',
    label: 'Peak brightness',
    description:
      'The brightest the device’s screen can show, used for the conversion and as the peak a virtual display reports. A device’s HDR color profile or the game’s NVIDIA driver profile can replace it.',
    category: 'hdr',
    group: 'RTX HDR',
    control: { kind: 'number', min: 400, max: 2000, step: 1, unit: 'nits' },
    default: 1000,
  },

  // Advanced
  {
    key: 'fallback_mode',
    label: 'Default stream mode',
    description:
      'The resolution and frame rate used when a launch request names none, as WIDTHxHEIGHTxFPS. Clients normally send their own.',
    category: 'advanced',
    group: 'Displays',
    control: { kind: 'text', placeholder: '1920x1080x60', mono: true },
    default: '1920x1080x60',
    advanced: true,
  },
  {
    key: 'dd_wa_dummy_plug_hdr10',
    label: 'Dummy plug HDR workaround',
    description:
      'Turns off vertical sync in the NVIDIA driver during every stream, like Turn off vertical sync under Frame limiting. It is kept for settings carried over from Vibepollo.',
    category: 'advanced',
    group: 'Displays',
    control: { kind: 'toggle' },
    default: false,
    advanced: true,
  },
];
