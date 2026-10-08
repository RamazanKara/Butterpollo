// Settings for the general, network, audio, input and commands categories.
// Defaults are the ones the Rust host falls back to when a key is unset.
import type { ConfigValue } from '../api';
import type { Option, Setting } from '../settings-types';

type Values = Record<string, ConfigValue>;

/** A boolean read as the host reads it; anything unrecognised keeps the default. */
function on(values: Values, key: string, fallback: boolean): boolean {
  const value = values[key];
  if (typeof value === 'boolean') return value;
  const word = String(value ?? '')
    .trim()
    .replace(/^"(.*)"$/, '$1')
    .toLowerCase();
  if (['true', 'yes', '1', 'enable', 'enabled', 'on'].includes(word)) return true;
  if (['false', 'no', '0', 'disable', 'disabled', 'off'].includes(word)) return false;
  return fallback;
}

/** A text setting read as the host reads it: blank means the default. */
function text(values: Values, key: string, fallback: string): string {
  const value = values[key];
  const raw = value === null || value === undefined ? '' : String(value);
  return raw.trim() === '' ? fallback : raw;
}

const locales: Option[] = [
  { value: 'en', label: 'English' },
  { value: 'en_GB', label: 'English (UK)' },
  { value: 'en_US', label: 'English (US)' },
  { value: 'bg', label: 'Български' },
  { value: 'cs', label: 'Čeština' },
  { value: 'de', label: 'Deutsch' },
  { value: 'es', label: 'Español' },
  { value: 'fr', label: 'Français' },
  { value: 'hu', label: 'Magyar' },
  { value: 'it', label: 'Italiano' },
  { value: 'ja', label: '日本語' },
  { value: 'ko', label: '한국어' },
  { value: 'pl', label: 'Polski' },
  { value: 'pt', label: 'Português' },
  { value: 'pt_BR', label: 'Português (Brasil)' },
  { value: 'ru', label: 'Русский' },
  { value: 'sv', label: 'Svenska' },
  { value: 'tr', label: 'Türkçe' },
  { value: 'uk', label: 'Українська' },
  { value: 'vi', label: 'Tiếng Việt' },
  { value: 'zh', label: '中文（简体）' },
  { value: 'zh_TW', label: '中文（繁體）' },
];

const general: Setting[] = [
  {
    key: 'sunshine_name',
    label: 'Host name',
    description:
      "The name Moonlight shows for this host. Leave it empty to use the PC's name; LAN discovery picks up a change after a restart.",
    category: 'general',
    group: 'Host',
    control: { kind: 'text', placeholder: "This PC's name" },
    default: '',
  },
  {
    key: 'locale',
    label: 'Language',
    description: "Language of the host's built-in web pages, also reported to tools that ask for it.",
    category: 'general',
    group: 'Host',
    control: { kind: 'select', options: locales },
    default: 'en',
  },
  {
    key: 'enable_pairing',
    label: 'Allow pairing',
    description:
      'Let new devices pair with this host. Turn it off once your devices are paired; devices already paired keep working.',
    category: 'general',
    group: 'Host',
    control: { kind: 'toggle' },
    default: true,
  },
  {
    key: 'system_tray',
    label: 'Tray icon',
    description: 'Show a Butterpollo icon in the Windows notification area, with a shortcut to this console.',
    category: 'general',
    group: 'Tray',
    control: { kind: 'toggle' },
    default: true,
    restart: true,
  },
  {
    key: 'hide_tray_controls',
    label: 'Hide tray controls',
    description:
      "Remove Disconnect clients, Restart and Quit from the tray icon's menu, so people at the PC cannot stop the host from there.",
    category: 'general',
    group: 'Tray',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: (values) => on(values, 'system_tray', true),
    restart: true,
  },
  {
    key: 'update_check_interval',
    label: 'Update check interval',
    description:
      'How often the host looks for a new Butterpollo release. 0 turns off automatic checks; you can still check from Maintenance.',
    category: 'general',
    group: 'Updates',
    control: { kind: 'number', min: 0, step: 1, unit: 's' },
    default: 86400,
  },
  {
    key: 'auto_update',
    label: 'Install updates automatically',
    description: 'Download verified releases and install after one minute without streams, pending connections or running host apps. Requires the Windows service. Off by default.',
    category: 'general',
    group: 'Updates',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'notify_pre_releases',
    label: 'Include pre-releases',
    description: 'Also offer pre-release versions when checking for updates. They get fixes sooner but may have more bugs.',
    category: 'general',
    group: 'Updates',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'min_log_level',
    label: 'Log level',
    description:
      'How much detail the host writes to its log. Debug and Verbose help when reporting a problem but make the log grow quickly.',
    category: 'general',
    group: 'Logging',
    control: {
      kind: 'select',
      options: [
        { value: 'verbose', label: 'Verbose' },
        { value: 'debug', label: 'Debug' },
        { value: 'info', label: 'Info' },
        { value: 'warning', label: 'Warning' },
        { value: 'error', label: 'Error' },
        { value: 'none', label: 'None' },
      ],
    },
    default: 'info',
    restart: true,
  },
  {
    key: 'log_path',
    label: 'Log file',
    description: "Where the host writes its log. A relative path is inside the host's configuration folder.",
    category: 'general',
    group: 'Logging',
    control: { kind: 'text', placeholder: 'logs/butterpollo.log', mono: true },
    default: 'logs/butterpollo.log',
    restart: true,
  },
  {
    key: 'legacy_ordering',
    label: 'App order for older clients',
    description:
      'Add invisible characters to app names so older Moonlight clients list apps in your order. This can break scripts and tools that match app names exactly.',
    category: 'general',
    group: 'Compatibility',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'envvar_compatibility_mode',
    label: 'Whole-number frame rate for commands',
    description:
      'Give commands SUNSHINE_CLIENT_FPS as a whole number, such as 60 instead of 59.940. Turn it on if a script cannot read fractional frame rates.',
    category: 'general',
    group: 'Compatibility',
    control: { kind: 'toggle' },
    default: false,
  },
];

const encryption: Option[] = [
  { value: '0', label: 'Off' },
  { value: '1', label: 'When the client supports it' },
  { value: '2', label: 'Required' },
];

const network: Setting[] = [
  {
    key: 'port',
    label: 'Base port',
    description:
      'Moonlight connects on this port and the one 5 below it, this console uses the next port up, and streams use ports 9, 10, 11 and 21 above it. Change it only if another program needs these ports.',
    category: 'network',
    group: 'Ports and addresses',
    control: { kind: 'number', min: 1029, max: 65514, step: 1 },
    default: 47989,
    restart: true,
  },
  {
    key: 'address_family',
    label: 'IP versions',
    description: 'Whether the host also accepts connections over IPv6. Ignored when a bind address is set.',
    category: 'network',
    group: 'Ports and addresses',
    control: {
      kind: 'select',
      options: [
        { value: 'ipv4', label: 'IPv4 only' },
        { value: 'both', label: 'IPv4 and IPv6' },
      ],
    },
    default: 'ipv4',
    restart: true,
  },
  {
    key: 'bind_address',
    label: 'Bind address',
    description:
      "Listen on one of this PC's IP addresses instead of all of them; LAN discovery then uses only that address. Leave it empty to listen on every address.",
    category: 'network',
    group: 'Ports and addresses',
    control: { kind: 'text', placeholder: 'All addresses', mono: true },
    default: '',
    restart: true,
  },
  {
    key: 'enable_discovery',
    label: 'LAN discovery',
    description: 'Announce this host on the local network so Moonlight finds it without typing an address.',
    category: 'network',
    group: 'Discovery and internet',
    control: { kind: 'toggle' },
    default: true,
    restart: true,
  },
  {
    key: 'upnp',
    label: 'UPnP port forwarding',
    description:
      'Ask the router to forward the streaming ports so clients can connect over the internet. The router must have UPnP turned on.',
    category: 'network',
    group: 'Discovery and internet',
    control: { kind: 'toggle' },
    default: false,
    restart: true,
  },
  {
    key: 'lan_encryption_mode',
    label: 'LAN encryption',
    description:
      'Whether streams to clients on the local network, including Tailscale, are encrypted. Required turns away clients that cannot encrypt; encryption costs some performance on slower devices.',
    category: 'network',
    group: 'Streams',
    control: { kind: 'select', options: encryption },
    default: 0,
  },
  {
    key: 'wan_encryption_mode',
    label: 'Internet encryption',
    description:
      'Whether streams to clients outside the local network are encrypted. Required turns away clients that cannot encrypt.',
    category: 'network',
    group: 'Streams',
    control: { kind: 'select', options: encryption },
    default: 1,
  },
  {
    key: 'ping_timeout',
    label: 'Client timeout',
    description:
      'How long the host waits without hearing from a client before it ends the stream. Raise it if streams drop on an unreliable network.',
    category: 'network',
    group: 'Streams',
    control: { kind: 'number', min: 1000, max: 300000, step: 1, unit: 'ms' },
    default: 10000,
  },
  {
    key: 'origin_web_ui_allowed',
    label: 'Console access',
    description:
      'Which addresses may open this console and use its API. Local network includes private addresses and Tailscale (100.64.0.0/10); with Any network and UPnP on, the console port is forwarded too.',
    category: 'network',
    group: 'Console',
    control: {
      kind: 'select',
      options: [
        { value: 'pc', label: 'This PC only' },
        { value: 'lan', label: 'Local network' },
        { value: 'wan', label: 'Any network' },
      ],
    },
    default: 'lan',
  },
  {
    key: 'csrf_allowed_origins',
    label: 'Trusted origins',
    description:
      'Other web addresses allowed to make changes through this console, written as scheme and host, such as https://stream.example.com. Add one only when you reach the console through a reverse proxy.',
    category: 'network',
    group: 'Console',
    control: { kind: 'list', placeholder: 'https://stream.example.com' },
    default: [],
  },
  {
    key: 'session_token_ttl_seconds',
    label: 'Sign-in token lifetime',
    description:
      'How long a console sign-in lasts before the browser must renew it (60 to 604800 seconds). Without "remember me", a sign-in ends after this time or one day, whichever is longer.',
    category: 'network',
    group: 'Console',
    control: { kind: 'number', min: 60, max: 604800, step: 1, unit: 's' },
    default: 7200,
  },
  {
    key: 'remember_me_refresh_token_ttl_seconds',
    label: 'Remembered sign-in lifetime',
    description:
      'How long a browser signed in with "remember me" stays signed in, up to 366 days. Lower it on shared computers.',
    category: 'network',
    group: 'Console',
    control: { kind: 'number', min: 60, max: 31622400, step: 1, unit: 's' },
    default: 604800,
  },
];

const streamsAudio = (values: Values) => on(values, 'stream_audio', true);

const audio: Setting[] = [
  {
    key: 'stream_audio',
    label: 'Stream audio',
    description:
      "Send the host's sound to the client. Turn it off when a stream is only used as an extra monitor and should stay silent.",
    category: 'audio',
    group: 'Sound',
    control: { kind: 'toggle' },
    default: true,
  },
  {
    key: 'audio_sink',
    label: 'Audio device',
    description:
      'The device captured when the client keeps sound playing on the host, or when no virtual speakers exist. Leave it empty to use the Windows default output.',
    category: 'audio',
    group: 'Devices',
    control: { kind: 'audio' },
    default: '',
    visibleWhen: streamsAudio,
  },
  {
    key: 'virtual_sink',
    label: 'Virtual speakers',
    description:
      "The device made the Windows default during a stream, so sound goes to the client instead of the PC's speakers. Leave it empty to use Steam Streaming Speakers when installed; a device chosen here is used even when the client keeps sound on the host.",
    category: 'audio',
    group: 'Devices',
    control: { kind: 'audio' },
    default: '',
    visibleWhen: streamsAudio,
  },
  {
    key: 'audio_sink_capture_only',
    label: 'Capture without switching',
    description:
      'Capture the audio device without changing the Windows default output; send the apps you want to hear to that device yourself. Applies only when an audio device is set and virtual speakers are not.',
    category: 'audio',
    group: 'Devices',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: (values) =>
      streamsAudio(values) && text(values, 'audio_sink', '') !== '' && text(values, 'virtual_sink', '') === '',
  },
  {
    key: 'auto_capture_sink',
    label: 'Follow the default device',
    description:
      'Without virtual speakers, capture whichever device is the Windows default, even if it changes during a stream, and retry when capture fails. Turn it off to stay on the device chosen when the stream started.',
    category: 'audio',
    group: 'Devices',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: streamsAudio,
  },
  {
    key: 'keep_sink_default',
    label: 'Keep virtual speakers as default',
    description:
      'If another device becomes the Windows default during a stream, switch back to the virtual speakers. The device you picked is still restored when the stream ends.',
    category: 'audio',
    group: 'Virtual speakers',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: streamsAudio,
  },
  {
    key: 'install_steam_audio_drivers',
    label: 'Install Steam Streaming Speakers',
    description:
      'When no virtual speakers exist and Steam is installed, install its Streaming Speakers at the start of a stream. They keep the PC silent and support 5.1 and 7.1 surround.',
    category: 'audio',
    group: 'Virtual speakers',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: streamsAudio,
  },
];

const keyboardOn = (values: Values) => on(values, 'keyboard', true);
const mouseOn = (values: Values) => on(values, 'mouse', true);
const controllerOn = (values: Values) => on(values, 'controller', true);

const input: Setting[] = [
  {
    key: 'keyboard',
    label: 'Keyboard',
    description: 'Let clients type on the host.',
    category: 'input',
    group: 'Devices',
    control: { kind: 'toggle' },
    default: true,
  },
  {
    key: 'mouse',
    label: 'Mouse',
    description: 'Let clients use the mouse on the host. Pen and touch input need this too.',
    category: 'input',
    group: 'Devices',
    control: { kind: 'toggle' },
    default: true,
  },
  {
    key: 'controller',
    label: 'Controllers',
    description: 'Let clients use game controllers on the host.',
    category: 'input',
    group: 'Devices',
    control: { kind: 'toggle' },
    default: true,
  },
  {
    key: 'enable_input_only_mode',
    label: 'Remote input entry',
    description:
      "Add a Remote Input entry to every client's app list. It sends keyboard, mouse and controller input to the host without video, for example from a second device.",
    category: 'input',
    group: 'Devices',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'always_send_scancodes',
    label: 'Send scan codes',
    description:
      'Send keys as scan codes, which some games need to see keyboard input at all. Turn it off if a client with a non-US layout types the wrong characters.',
    category: 'input',
    group: 'Keyboard',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: keyboardOn,
  },
  {
    key: 'key_rightalt_to_key_win',
    label: 'Right Alt as Windows key',
    description: 'Treat the right Alt key as the Windows key, for clients that cannot send the Windows key.',
    category: 'input',
    group: 'Keyboard',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: keyboardOn,
  },
  {
    key: 'key_repeat_delay',
    label: 'Key repeat delay',
    description: 'How long a held key waits before it starts repeating.',
    category: 'input',
    group: 'Keyboard',
    control: { kind: 'number', min: 0, max: 60000, step: 1, unit: 'ms' },
    default: 500,
    visibleWhen: keyboardOn,
  },
  {
    key: 'key_repeat_frequency',
    label: 'Key repeat rate',
    description: 'How many times per second a held key repeats. Decimals are allowed; 0 uses the default.',
    category: 'input',
    group: 'Keyboard',
    control: { kind: 'number', min: 0, max: 1000, step: 0.1, unit: 'Hz' },
    default: 24.9,
    visibleWhen: keyboardOn,
  },
  {
    key: 'keybindings',
    label: 'Key remapping',
    description:
      'Pairs of Windows virtual-key codes in one list, each key from the client followed by the key the host presses instead, such as ["0x10", "0xA0"]. Shift, Ctrl and Alt already map to their left-hand keys.',
    category: 'input',
    group: 'Keyboard',
    control: { kind: 'json' },
    default: [],
    visibleWhen: keyboardOn,
  },
  {
    key: 'high_resolution_scrolling',
    label: 'High-resolution scrolling',
    description:
      'Pass fine scroll steps from the client. Turn it off for older apps that scroll too far, so scrolling moves in whole notches.',
    category: 'input',
    group: 'Mouse, pen and touch',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: mouseOn,
  },
  {
    key: 'native_pen_touch',
    label: 'Native pen and touch',
    description:
      'Pass pen and touch from clients to Windows as pen and touch. Turn it off for apps that handle them badly; clients then send touch as mouse input.',
    category: 'input',
    group: 'Mouse, pen and touch',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: mouseOn,
  },
  {
    key: 'gamepad',
    label: 'Controller type',
    description:
      'Automatic uses ViGEmBus when installed for Xbox-type controllers: DualShock 4 for enabled motion/touchpad input, Xbox 360 otherwise. This Xbox 360 avoids Steam detecting one VHF Xbox pad twice. PlayStation controllers get the VHF DualSense, with adaptive triggers, when that driver is installed too, and a ViGEmBus DualShock 4 otherwise. Without ViGEmBus, Automatic uses VHF: DualSense for PlayStation, Switch Pro for Nintendo, and Xbox Series otherwise, with motion/touchpad preferences for other types. Explicit choices use the named driver.',
    category: 'input',
    group: 'Controllers',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Automatic' },
        { value: 'x360', label: 'Xbox 360 (ViGEmBus)' },
        { value: 'ds4', label: 'DualShock 4 (ViGEmBus)' },
        { value: 'vhf_xbox', label: 'Xbox Series (VHF)' },
        { value: 'vhf_xbox_one', label: 'Xbox One (VHF)' },
        { value: 'vhf_ds4', label: 'DualShock 4 (VHF)' },
        { value: 'vhf_ds5', label: 'DualSense (VHF)' },
        { value: 'vhf_switch', label: 'Switch Pro (VHF)' },
      ],
    },
    default: 'auto',
    visibleWhen: controllerOn,
  },
  {
    key: 'motion_as_ds4',
    label: 'PlayStation controller for motion controls',
    description:
      "With Automatic, motion sensors select DualShock 4 through ViGEmBus or DualSense through VHF, including Xbox-type clients such as Steam Deck. VHF keeps Nintendo clients as Switch Pro. Turn it off to ignore motion sensors when choosing.",
    category: 'input',
    group: 'Controllers',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: controllerOn,
  },
  {
    key: 'touchpad_as_ds4',
    label: 'PlayStation controller for touchpads',
    description:
      "With Automatic, a touchpad selects DualShock 4 through ViGEmBus or DualSense through VHF. VHF keeps Nintendo clients as Switch Pro. Turn it off to ignore the touchpad when choosing.",
    category: 'input',
    group: 'Controllers',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: controllerOn,
  },
  {
    key: 'forward_rumble',
    label: 'Vibration',
    description: "Send vibration from games back to the client's controller.",
    category: 'input',
    group: 'Controllers',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: controllerOn,
  },
  {
    key: 'back_button_timeout',
    label: 'Hold Back for Guide',
    description:
      'Holding Back (Select) this long presses the Guide (Home) button instead, for controllers without one. -1 turns this off.',
    category: 'input',
    group: 'Controllers',
    control: { kind: 'number', min: -1, max: 60000, step: 1, unit: 'ms' },
    default: -1,
    visibleWhen: controllerOn,
  },
];

const commands: Setting[] = [
  {
    key: 'global_prep_cmd',
    label: 'Preparation commands',
    description:
      'Commands run before every app starts (do) and after it closes (undo). If a do command fails, the app does not start; apps can opt out in their settings.',
    category: 'commands',
    group: 'Around every app',
    control: { kind: 'commands' },
    default: [],
  },
  {
    key: 'global_state_cmd',
    label: 'State commands',
    description:
      "Commands run when any app's stream resumes (do) and when it pauses (undo), before the app's own state commands. Apps can opt out in their settings.",
    category: 'commands',
    group: 'Around every app',
    control: { kind: 'commands' },
    default: [],
  },
  {
    key: 'server_cmd',
    label: 'Server commands',
    description:
      'Commands a client can run on the host from its stream menu, in clients that support it such as Artemis. Only devices with the Server commands permission see them.',
    category: 'commands',
    group: 'From clients',
    control: { kind: 'server-commands' },
    default: [],
  },
];

export const settings: Setting[] = [...general, ...network, ...audio, ...input, ...commands];
