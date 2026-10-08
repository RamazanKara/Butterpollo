// Paired-device permissions and the editable form of a device's settings.
import { PERM, PERM_ALL, type Client } from '../../lib/api';

export const VIEW_ONLY = PERM.LIST_APPS | PERM.VIEW_STREAMS;

export const PERMISSION_GROUPS: { name: string; items: { bit: number; label: string }[] }[] = [
  {
    name: 'Apps',
    items: [
      { bit: PERM.LIST_APPS, label: 'List apps' },
      { bit: PERM.VIEW_STREAMS, label: 'View streams' },
      { bit: PERM.LAUNCH_APPS, label: 'Launch apps' },
    ],
  },
  {
    name: 'Input',
    items: [
      { bit: PERM.CONTROLLER, label: 'Controllers' },
      { bit: PERM.TOUCH, label: 'Touch' },
      { bit: PERM.PEN, label: 'Pen' },
      { bit: PERM.MOUSE, label: 'Mouse' },
      { bit: PERM.KEYBOARD, label: 'Keyboard' },
    ],
  },
  {
    name: 'Other',
    items: [
      { bit: PERM.CLIPBOARD_WRITE, label: 'Write to the clipboard' },
      { bit: PERM.CLIPBOARD_READ, label: 'Read the clipboard' },
      { bit: PERM.FILE_UPLOAD, label: 'Upload files' },
      { bit: PERM.FILE_DOWNLOAD, label: 'Download files' },
      { bit: PERM.SERVER_CMD, label: 'Host commands' },
    ],
  },
];

export function permissionSummary(perm: number): string {
  const granted = perm & PERM_ALL;
  if (granted === PERM_ALL) return 'Full control';
  if (granted === VIEW_ONLY) return 'View only';
  if (granted === 0) return 'No access';
  return 'Custom';
}

/** "List apps, Mouse, Keyboard" for a tooltip. */
export function grantedLabels(perm: number): string {
  return PERMISSION_GROUPS.flatMap((group) => group.items)
    .filter((item) => (perm & item.bit) === item.bit)
    .map((item) => item.label)
    .join(', ');
}

export const VIRTUAL_DISPLAY_MODES = [
  { value: 'disabled', label: 'Off, stream a monitor' },
  { value: 'per_client', label: 'One for each device' },
  { value: 'shared', label: 'One shared by all devices' },
];

export const VIRTUAL_DISPLAY_LAYOUTS = [
  { value: 'exclusive', label: 'Exclusive', hint: 'Turns the other monitors off while streaming.' },
  { value: 'extended', label: 'Extended', hint: 'Adds the virtual display next to the monitors.' },
  { value: 'extended_primary', label: 'Extended, primary', hint: 'Adds it and makes it the main display.' },
  {
    value: 'extended_isolated',
    label: 'Extended, isolated',
    hint: 'Adds it far from the monitors so the mouse cannot wander onto it.',
  },
  {
    value: 'extended_primary_isolated',
    label: 'Extended, primary and isolated',
    hint: 'Main display, placed far from the other monitors.',
  },
];

export interface CommandRow {
  /** Keeps each row's inputs in place when another row is removed. */
  key: number;
  cmd: string;
  elevated: boolean;
}

/** The editor's state. An empty string means "use the host setting". */
export interface Draft {
  name: string;
  enabled: boolean;
  perm: number;
  output_name_override: string;
  /** WIDTHxHEIGHTxREFRESH, empty for the mode the device asks for. */
  display_mode: string;
  always_use_virtual_display: boolean;
  virtual_display_mode: string;
  virtual_display_layout: string;
  prefer_10bit_sdr: '' | 'on' | 'off';
  hdr_profile: string;
  allow_client_commands: boolean;
  do: CommandRow[];
  undo: CommandRow[];
}

/** Settings files written by older consoles hold "true" and "0" as often as booleans. */
function flag(value: unknown): boolean | null {
  if (value === true || value === 1 || value === 'true' || value === '1') return true;
  if (value === false || value === 0 || value === 'false' || value === '0') return false;
  return null;
}

function text(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function choice(value: unknown, allowed: { value: string }[]): string {
  const wanted = text(value).toLowerCase();
  return allowed.some((option) => option.value === wanted) ? wanted : '';
}

/** The host reads WIDTHxHEIGHTxREFRESH, with up to three decimals in the refresh. */
export function validDisplayMode(value: string): boolean {
  if (value.trim() === '') return true;
  const match = /^\s*(\d{1,5})[xX](\d{1,5})[xX](\d{1,4}(?:\.\d{1,3})?)\s*$/.exec(value);
  if (!match) return false;
  const [width, height, rate] = [Number(match[1]), Number(match[2]), Number(match[3])];
  return width >= 1 && width <= 16384 && height >= 1 && height <= 16384 && rate >= 1 && rate <= 1000;
}

let nextKey = 1;

export function commandRow(cmd = '', elevated = false): CommandRow {
  return { key: nextKey++, cmd, elevated };
}

function commandRows(value: unknown): CommandRow[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry: unknown) => {
    if (!entry || typeof entry !== 'object') return [];
    const { cmd, elevated } = entry as { cmd?: unknown; elevated?: unknown };
    const command = text(cmd);
    return command ? [commandRow(command, flag(elevated) === true)] : [];
  });
}

export function toDraft(client: Client): Draft {
  const sdr = flag(client.prefer_10bit_sdr);
  return {
    name: client.name,
    enabled: flag(client.enabled) !== false,
    perm: client.perm & PERM_ALL,
    output_name_override: text(client.output_name_override),
    display_mode: text(client.display_mode),
    always_use_virtual_display: flag(client.always_use_virtual_display) === true,
    virtual_display_mode: choice(client.virtual_display_mode, VIRTUAL_DISPLAY_MODES),
    virtual_display_layout: choice(client.virtual_display_layout, VIRTUAL_DISPLAY_LAYOUTS),
    prefer_10bit_sdr: sdr === null ? '' : sdr ? 'on' : 'off',
    hdr_profile: text(client.hdr_profile),
    allow_client_commands: flag(client.allow_client_commands) !== false,
    do: commandRows(client.do),
    undo: commandRows(client.undo),
  };
}

type Command = { cmd: string; elevated: boolean };
export type Changes = Record<string, string | number | boolean | null | Command[]>;

function commands(rows: CommandRow[]): Command[] {
  return rows.filter((row) => row.cmd.trim()).map((row) => ({ cmd: row.cmd.trim(), elevated: row.elevated }));
}

/** The draft as the host stores it: null clears a per-device setting. */
function serialise(draft: Draft): Changes {
  return {
    name: draft.name.trim(),
    enabled: draft.enabled,
    perm: draft.perm,
    output_name_override: draft.output_name_override || null,
    display_mode: draft.display_mode.trim() || null,
    always_use_virtual_display: draft.always_use_virtual_display,
    virtual_display_mode: draft.virtual_display_mode || null,
    virtual_display_layout: draft.virtual_display_layout || null,
    prefer_10bit_sdr: draft.prefer_10bit_sdr === '' ? null : draft.prefer_10bit_sdr === 'on',
    hdr_profile: draft.hdr_profile || null,
    allow_client_commands: draft.allow_client_commands,
    // The host rejects a null command list, so an empty list is sent instead.
    do: commands(draft.do),
    undo: commands(draft.undo),
  };
}

/** Only the keys whose stored value would change. */
export function changes(before: Draft, after: Draft): Changes {
  const old = serialise(before);
  return Object.fromEntries(
    Object.entries(serialise(after)).filter(([key, value]) => JSON.stringify(value) !== JSON.stringify(old[key])),
  );
}
