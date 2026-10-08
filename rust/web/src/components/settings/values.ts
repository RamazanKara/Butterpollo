// Stored settings, edits, and the PATCH body built from them.
//
// The host keeps every value as text and GET /api/config parses each one as
// JSON when it can, so a toggle may arrive as true, "true", "enabled" or 1
// and a number as 20 or "20". Values are normalised for their control before
// they are shown or compared.
import type { Config, ConfigValue, PrepCommand } from '../../lib/api';
import type { Setting } from '../../lib/settings-types';

/** Keys GET /api/config adds that are not settings. They are never sent back. */
export const NOT_SETTINGS: ReadonlySet<string> = new Set(['status', 'platform', 'version']);

/** Edited values by key. null asks for the key to be removed (back to the default). */
export type Edits = Record<string, ConfigValue>;

export interface ServerCommand {
  name: string;
  cmd: string;
  elevated: boolean;
}

export function settingsOnly(config: Config): Config {
  return Object.fromEntries(Object.entries(config).filter(([key]) => !NOT_SETTINGS.has(key)));
}

export const has = (record: object, key: string) => Object.hasOwn(record, key);

function unquote(text: string): string {
  const trimmed = text.trim();
  return trimmed.length >= 2 && trimmed.startsWith('"') && trimmed.endsWith('"') ? trimmed.slice(1, -1) : trimmed;
}

// The words the host's Config::boolean accepts.
const TRUE = new Set(['true', 'yes', '1', 'enable', 'enabled', 'on']);
const FALSE = new Set(['false', 'no', '0', 'disable', 'disabled', 'off']);

export function toBoolean(value: unknown): boolean | null {
  if (typeof value === 'boolean') return value;
  if (typeof value === 'number') return value === 1 ? true : value === 0 ? false : null;
  if (typeof value !== 'string') return null;
  const word = unquote(value).toLowerCase();
  if (TRUE.has(word)) return true;
  if (FALSE.has(word)) return false;
  return null;
}

/** Numbers, numeric text, quoted numbers ("756") and hexadecimal (0x2a), as the host reads them. */
export function toNumber(value: unknown): number | null {
  if (typeof value === 'number') return Number.isFinite(value) ? value : null;
  if (typeof value !== 'string') return null;
  const text = unquote(value);
  if (text === '') return null;
  if (/^[+-]?0x[0-9a-f]+$/i.test(text)) return parseInt(text, 16);
  const number = Number(text);
  return Number.isFinite(number) ? number : null;
}

/** Strings as they are; numbers and booleans as written; anything else as JSON. */
export function toText(value: unknown): string {
  if (value === null || value === undefined) return '';
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return JSON.stringify(value);
}

/** A JSON array, or the "[a, b]" and "a, b" lists older consoles wrote. */
export function toList(value: unknown): string[] {
  if (Array.isArray(value)) return value.map((item: unknown) => toText(item));
  if (value === null || value === undefined) return [];
  if (typeof value !== 'string') return [toText(value)];
  const text = value.trim();
  if (text === '') return [];
  try {
    const parsed: unknown = JSON.parse(text);
    if (Array.isArray(parsed)) return toList(parsed);
  } catch {
    // Not JSON: a comma-separated list.
  }
  return text
    .replace(/^\[/, '')
    .replace(/\]$/, '')
    .split(',')
    .map(unquote)
    .filter((item) => item !== '');
}

function objects(value: unknown): Record<string, unknown>[] {
  let list = value;
  if (typeof list === 'string') {
    try {
      list = JSON.parse(list);
    } catch {
      return [];
    }
  }
  if (!Array.isArray(list)) return [];
  return list.filter(
    (item: unknown): item is Record<string, unknown> => !!item && typeof item === 'object' && !Array.isArray(item),
  );
}

export function toCommands(value: unknown): PrepCommand[] {
  return objects(value).map((entry) => ({
    do: toText(entry.do),
    undo: toText(entry.undo),
    elevated: toBoolean(entry.elevated) === true,
  }));
}

export function toServerCommands(value: unknown): ServerCommand[] {
  return objects(value).map((entry) => ({
    name: toText(entry.name),
    cmd: toText(entry.cmd),
    elevated: toBoolean(entry.elevated) === true,
  }));
}

/** The value in the form its control edits. */
export function normalise(setting: Setting, value: ConfigValue | undefined): ConfigValue {
  switch (setting.control.kind) {
    case 'toggle':
      return toBoolean(value) ?? toBoolean(setting.default) ?? false;
    case 'number':
      return toNumber(value);
    case 'list':
      return toList(value);
    case 'commands':
      return toCommands(value);
    case 'server-commands':
      return toServerCommands(value);
    case 'json':
      return value === undefined ? null : value;
    default:
      return toText(value);
  }
}

/** The edited value, else the stored one, else the default. */
export function effective(setting: Setting, edits: Edits, stored: Config): ConfigValue {
  if (has(edits, setting.key)) return edits[setting.key] ?? setting.default;
  return stored[setting.key] ?? setting.default;
}

/** Every value as the host would see it after saving, defaults filled in; for visibleWhen. */
export function currentValues(settings: readonly Setting[], edits: Edits, stored: Config): Record<string, ConfigValue> {
  const values: Record<string, ConfigValue> = { ...stored };
  for (const [key, value] of Object.entries(edits)) {
    if (value === null) delete values[key];
    else values[key] = value;
  }
  for (const setting of settings) values[setting.key] = normalise(setting, effective(setting, edits, stored));
  return values;
}

const count = (n: number) => (n === 0 ? 'None' : n === 1 ? '1 command' : `${n} commands`);
const clip = (text: string, max = 60) => (text.length > max ? `${text.slice(0, max - 1)}…` : text);

export interface DefaultText {
  text: string;
  /** A raw value (set in mono), not a word such as On or Automatic. */
  literal: boolean;
}

const word = (text: string): DefaultText => ({ text, literal: false });
const literal = (text: string, otherwise: string): DefaultText =>
  text ? { text: clip(text), literal: true } : word(otherwise);

/** The default in words: On, an option's label, Automatic, Not set; or the value itself. */
export function describeDefault(setting: Setting): DefaultText {
  const control = setting.control;
  const value = setting.default;
  switch (control.kind) {
    case 'toggle':
      return word(toBoolean(value) ? 'On' : 'Off');
    case 'select': {
      const text = toText(value);
      const option = control.options.find((candidate) => candidate.value === text);
      return option ? word(option.label) : literal(text, 'Automatic');
    }
    case 'number': {
      const number = toNumber(value);
      if (number === null) return word('Not set');
      return literal(control.unit ? `${number} ${control.unit}` : String(number), 'Not set');
    }
    case 'text':
      return literal(toText(value), 'Not set');
    case 'display':
    case 'adapter':
      return literal(toText(value), 'Automatic');
    case 'audio':
      return literal(toText(value), 'System default');
    case 'list':
      return literal(toList(value).join(', '), 'None');
    case 'commands':
      return word(count(toCommands(value).length));
    case 'server-commands':
      return word(count(toServerCommands(value).length));
    case 'json':
      return word(value === null || value === '' ? 'Not set' : Array.isArray(value) ? `${value.length} entries` : 'Built-in configuration');
  }
}

function isNonEmptyList(value: unknown): boolean {
  return toList(value).length > 0;
}

/** An emptied list is removed, unless the default has items: then [] is kept on purpose. */
function listValue(items: unknown[], fallback: ConfigValue): ConfigValue {
  return items.length || isNonEmptyList(fallback) ? items : null;
}

function textValue(value: unknown): string | null {
  const text = toText(value).trim();
  return text === '' ? null : text;
}

/**
 * The value as the host should store it. null means remove the key: empty
 * text, an emptied list, a cleared number. A key the schema does not
 * describe (setting undefined) is stored as the text typed.
 */
export function patchValue(setting: Setting | undefined, value: ConfigValue | undefined): ConfigValue {
  if (value === null || value === undefined) return null;
  if (!setting) return textValue(value);
  switch (setting.control.kind) {
    case 'toggle':
      return toBoolean(value);
    case 'number':
      return toNumber(value);
    case 'list':
      return listValue(
        toList(value)
          .map((item) => item.trim())
          .filter((item) => item !== ''),
        setting.default,
      );
    case 'commands':
      return listValue(
        toCommands(value)
          .map((command) => ({ do: command.do.trim(), undo: command.undo.trim(), elevated: command.elevated }))
          .filter((command) => command.do || command.undo),
        setting.default,
      );
    case 'server-commands':
      return listValue(
        toServerCommands(value)
          .map((command) => ({ name: command.name.trim(), cmd: command.cmd.trim(), elevated: command.elevated }))
          .filter((command) => command.name || command.cmd),
        setting.default,
      );
    case 'json':
      return value === '' ? null : value;
    default:
      return textValue(value);
  }
}

/** JSON with object keys sorted, so equal values compare equal. */
function stable(value: unknown): string {
  return (
    JSON.stringify(value, (_key, item: unknown) =>
      item && typeof item === 'object' && !Array.isArray(item)
        ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
        : item,
    ) ?? 'undefined'
  );
}

export function sameValue(a: unknown, b: unknown): boolean {
  return stable(a) === stable(b);
}

/**
 * The PATCH body: only keys whose stored value would change, null for keys
 * to remove. An edit equal to what the host already uses is not a change.
 */
export function computeChanges(settings: ReadonlyMap<string, Setting>, edits: Edits, stored: Config): Config {
  const changes: Config = {};
  for (const [key, edit] of Object.entries(edits)) {
    if (NOT_SETTINGS.has(key)) continue;
    const setting = settings.get(key);
    const isStored = has(stored, key);
    const next = edit === null ? null : patchValue(setting, edit);
    if (next === null) {
      if (isStored) changes[key] = null;
      continue;
    }
    const current = isStored ? patchValue(setting, stored[key]) : setting ? patchValue(setting, setting.default) : null;
    if (!sameValue(next, current)) changes[key] = next;
  }
  return changes;
}

/** The stored values after the host accepted `changes`. */
export function applyChanges(stored: Config, changes: Config): Config {
  const next = { ...stored };
  for (const [key, value] of Object.entries(changes)) {
    if (value === null) delete next[key];
    else next[key] = value;
  }
  return next;
}

/** A JSON value as text for editing; empty when not set. */
export function jsonText(value: ConfigValue | undefined): string {
  if (value === null || value === undefined || value === '') return '';
  return JSON.stringify(value, null, 2);
}

export type Parsed = { ok: true; value: ConfigValue } | { ok: false; error: string };

/** Empty text is valid and means not set. */
export function parseJson(text: string): Parsed {
  if (text.trim() === '') return { ok: true, value: '' };
  try {
    return { ok: true, value: JSON.parse(text) as ConfigValue };
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) };
  }
}
