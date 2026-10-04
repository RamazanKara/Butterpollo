// The editable copy of an app. The host replaces the whole record on save, so
// every key it sent is kept, including the ones this editor never shows.
import type { App, PrepCommand } from '../../lib/api';

export interface PrepRow extends PrepCommand {
  /** Keeps each row's inputs in place when rows move. */
  key: number;
}
export interface TextRow {
  key: number;
  value: string;
}

let nextKey = 1;
export const prepRow = (command: Partial<PrepCommand> = {}): PrepRow => ({
  key: nextKey++,
  do: typeof command.do === 'string' ? command.do : '',
  undo: typeof command.undo === 'string' ? command.undo : '',
  elevated: command.elevated === true,
});
export const textRow = (value = ''): TextRow => ({ key: nextKey++, value });

/** Switches, with the value the host uses when the key is missing. */
export const FLAGS = {
  elevated: false,
  'auto-detach': true,
  'wait-all': true,
  'terminate-on-pause': false,
  'allow-client-commands': true,
  'exclude-global-state-cmd': false,
  'exclude-global-prep-cmd': false,
  'virtual-display': false,
  'virtual-display-primary': false,
  'use-app-identity': false,
  'per-client-app-identity': false,
  'gen1-framegen-fix': false,
  'frame-gen-limiter-fix': false,
  'rtx-hdr': false,
  'lossless-scaling-enabled': false,
  'lossless-scaling-legacy-auto-detect': false,
} as const;
export type FlagKey = keyof typeof FLAGS;

/** Whole numbers and the range the host accepts. A missing key uses the host's value. */
export const NUMBERS: Record<string, { min: number; max?: number }> = {
  'exit-timeout': { min: 0, max: 300 },
  'scale-factor': { min: 1 },
  'rtx-hdr-sdr-brightness': { min: 0, max: 100 },
  'rtx-hdr-contrast': { min: -100, max: 100 },
  'rtx-hdr-saturation': { min: -100, max: 100 },
  'rtx-hdr-middle-gray': { min: 10, max: 100 },
  'rtx-hdr-peak-brightness': { min: 400, max: 2000 },
  'lossless-scaling-target-fps': { min: 1, max: 480 },
  'lossless-scaling-rtss-limit': { min: 1, max: 480 },
  'lossless-scaling-launch-delay': { min: 0, max: 600 },
};
export type NumberKey =
  | 'exit-timeout'
  | 'scale-factor'
  | 'rtx-hdr-sdr-brightness'
  | 'rtx-hdr-contrast'
  | 'rtx-hdr-saturation'
  | 'rtx-hdr-middle-gray'
  | 'rtx-hdr-peak-brightness'
  | 'lossless-scaling-target-fps'
  | 'lossless-scaling-rtss-limit'
  | 'lossless-scaling-launch-delay';

/** Choices and free text. An empty value removes the key. */
export type TextKey =
  | 'virtual-display-mode'
  | 'virtual-display-layout'
  | 'display-output'
  | 'dd-configuration-option'
  | 'frame-generation-mode'
  | 'gamepad'
  | 'lossless-scaling-profile';
const TEXT_KEYS: TextKey[] = [
  'virtual-display-mode',
  'virtual-display-layout',
  'display-output',
  'dd-configuration-option',
  'frame-generation-mode',
  'gamepad',
  'lossless-scaling-profile',
];

/** Keys the editor has a control for (or shows elsewhere). */
const KNOWN = new Set<string>([
  'name',
  'cmd',
  'working-dir',
  'detached',
  'prep-cmd',
  'image-path',
  'uuid',
  'prefer-10bit-sdr',
  'lossless-scaling-recommended',
  'lossless-scaling-custom',
  'lossless-scaling-framegen',
  ...Object.keys(FLAGS),
  ...Object.keys(NUMBERS),
  ...TEXT_KEYS,
]);

export function blankApp(): App {
  return { name: '', cmd: '', 'working-dir': '', 'prep-cmd': [] };
}

/** JSON with object keys sorted, for comparing records. */
export function stable(value: unknown): string {
  return JSON.stringify(value, (_key, item: unknown) =>
    item && typeof item === 'object' && !Array.isArray(item)
      ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
      : item,
  );
}

/** Read a switch the way the host does. */
function flagValue(value: unknown, fallback: boolean): boolean {
  if (typeof value === 'boolean') return value;
  if (typeof value === 'string') return ['true', '1', 'yes'].includes(value);
  return fallback;
}

function prepRows(value: unknown): PrepRow[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry: unknown) =>
    entry && typeof entry === 'object' ? [prepRow(entry as Partial<PrepCommand>)] : [],
  );
}

function textRows(value: unknown): TextRow[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry: unknown) => (typeof entry === 'string' ? [textRow(entry)] : []));
}

/** The record to send: blank command rows are dropped. */
function assemble(app: App, prep: PrepRow[], detached: TextRow[], keepDetached: boolean): App {
  const record: App = { ...app };
  record['prep-cmd'] = prep
    .filter((row) => row.do.trim() || row.undo.trim())
    .map(({ do: run, undo, elevated }) => ({ do: run, undo, elevated }));
  const commands = detached.map((row) => row.value).filter((value) => value.trim());
  if (commands.length || keepDetached) record.detached = commands;
  else delete record.detached;
  return record;
}

function normalise(source: App): App {
  const copy = $state.snapshot(source) as App;
  return assemble(copy, prepRows(copy['prep-cmd']), textRows(copy.detached), 'detached' in copy);
}

export class AppDraft {
  /** Everything but the command lists, which live in the rows below. */
  app = $state<App>(blankApp());
  prep = $state<PrepRow[]>([]);
  detached = $state<TextRow[]>([]);
  #saved = $state.raw<App>(blankApp());
  #baseline = $state('');

  readonly changed = $derived(stable(this.record()) !== this.#baseline);
  readonly nameMissing = $derived(this.app.name.trim() === '');
  readonly invalid = $derived(
    this.nameMissing || Object.keys(NUMBERS).some((key) => this.numberError(key as NumberKey) !== ''),
  );

  constructor(source: App = blankApp()) {
    this.reset(source);
  }

  /** The record as the host has it. */
  get saved(): App {
    return this.#saved;
  }

  reset(source: App) {
    const copy = $state.snapshot(source) as App;
    this.#saved = copy;
    this.app = $state.snapshot(source) as App;
    this.prep = prepRows(copy['prep-cmd']);
    this.detached = textRows(copy.detached);
    this.#baseline = stable(normalise(copy));
  }

  /** The host now has `record`; later edits stay unsaved. */
  markSaved(record: App) {
    this.#saved = $state.snapshot(record) as App;
    if (record.uuid) this.app.uuid = record.uuid;
    this.#baseline = stable(normalise(record));
  }

  /** Whether `source` is what this draft started from. */
  matches(source: App): boolean {
    return stable(normalise(source)) === this.#baseline;
  }

  record(): App {
    return assemble($state.snapshot(this.app) as App, this.prep, this.detached, 'detached' in this.#saved);
  }

  /** The cover file's path on the host; '' for none. */
  cover(): string {
    const value = this.app['image-path'];
    return typeof value === 'string' ? value : '';
  }
  setCover(path: string) {
    if (path) this.app['image-path'] = path;
    else delete this.app['image-path'];
  }

  flag(key: FlagKey): boolean {
    return flagValue(this.app[key], FLAGS[key]);
  }
  setFlag(key: FlagKey, value: boolean) {
    // The host's default needs no key, unless the record already spelled it out.
    if (value === FLAGS[key] && !(key in this.#saved)) delete this.app[key];
    else this.app[key] = value;
  }

  /** '' when the host setting applies. */
  tristate(key: 'prefer-10bit-sdr'): '' | 'on' | 'off' {
    const value = this.app[key];
    if (value === true || value === 'true' || value === '1' || value === 'on') return 'on';
    if (value === false || value === 'false' || value === '0' || value === 'off') return 'off';
    return '';
  }
  setTristate(key: 'prefer-10bit-sdr', value: '' | 'on' | 'off') {
    if (value === '') delete this.app[key];
    else this.app[key] = value === 'on';
  }

  text(key: TextKey): string {
    const value = this.app[key];
    return typeof value === 'string' ? value : '';
  }
  setText(key: TextKey, value: string) {
    if (value.trim()) this.app[key] = value;
    else delete this.app[key];
  }

  number(key: NumberKey): number | undefined {
    const value = this.app[key];
    if (typeof value === 'number') return value;
    if (typeof value === 'string' && value.trim() !== '' && Number.isFinite(Number(value))) return Number(value);
    return undefined;
  }
  setNumber(key: NumberKey, value: number | null | undefined) {
    if (value === null || value === undefined || Number.isNaN(value)) delete this.app[key];
    else this.app[key] = value;
  }
  numberError(key: NumberKey): string {
    const value = this.number(key);
    const range = NUMBERS[key];
    if (value === undefined || !range) return '';
    if (!Number.isInteger(value)) return 'Enter a whole number.';
    if (range.max === undefined) return value < range.min ? `Enter ${range.min} or more.` : '';
    return value < range.min || value > range.max ? `Enter ${range.min} to ${range.max}.` : '';
  }

  /** Keys the editor has no control for; they are saved unchanged. */
  others(): [string, unknown][] {
    return Object.entries(this.app)
      .filter(([key]) => !KNOWN.has(key))
      .sort(([a], [b]) => a.localeCompare(b));
  }

  /** The Lossless Scaling profile the app uses. */
  losslessProfile(): 'lossless-scaling-recommended' | 'lossless-scaling-custom' {
    return this.text('lossless-scaling-profile').toLowerCase() === 'recommended'
      ? 'lossless-scaling-recommended'
      : 'lossless-scaling-custom';
  }
  /** A value in the app's Lossless Scaling profile. */
  lossless(field: string): unknown {
    const profile = this.app[this.losslessProfile()];
    return profile && typeof profile === 'object' ? (profile as Record<string, unknown>)[field] : undefined;
  }
  setLossless(field: string, value: unknown) {
    const key = this.losslessProfile();
    const current = this.app[key];
    const profile = { ...(current && typeof current === 'object' ? (current as Record<string, unknown>) : {}) };
    if (value === undefined || value === '' || (typeof value === 'number' && Number.isNaN(value))) delete profile[field];
    else profile[field] = value;
    if (Object.keys(profile).length) this.app[key] = profile;
    else delete this.app[key];
  }
}
