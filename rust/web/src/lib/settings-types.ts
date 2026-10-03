// The shape of the settings schema. The data lives in settings-schema.ts.
import type { ConfigValue } from './api';

export type CategoryId =
  | 'general'
  | 'network'
  | 'video'
  | 'encoders'
  | 'display'
  | 'frame-limiting'
  | 'hdr'
  | 'audio'
  | 'input'
  | 'commands'
  | 'advanced';

export interface Category {
  id: CategoryId;
  label: string;
  description: string;
}

export interface Option {
  value: string;
  label: string;
}

export type Control =
  | { kind: 'toggle' }
  | { kind: 'select'; options: Option[] }
  | { kind: 'number'; min?: number; max?: number; step?: number; unit?: string }
  | { kind: 'text'; placeholder?: string; mono?: boolean }
  /** A string list, stored as a JSON array of strings. */
  | { kind: 'list'; placeholder?: string }
  /** Any JSON value, edited as text and validated. */
  | { kind: 'json' }
  /** Global preparation or state commands: [{do, undo, elevated}]. */
  | { kind: 'commands' }
  /** Commands clients may run: [{name, cmd, elevated}]. */
  | { kind: 'server-commands' }
  /** A display, chosen from the host's displays (empty = automatic). */
  | { kind: 'display' }
  /** An audio endpoint, chosen from the host's endpoints (empty = default). */
  | { kind: 'audio' }
  /** A GPU, chosen by name from the host's adapters (empty = automatic). */
  | { kind: 'adapter' };

export interface Setting {
  key: string;
  label: string;
  /** One or two plain sentences: what it does and when to change it. */
  description: string;
  category: CategoryId;
  /** A heading within the category, for grouping related settings. */
  group?: string;
  control: Control;
  /** The value the Rust host uses when the key is not set. */
  default: ConfigValue;
  /** Hide when not relevant, given the current (edited) values. */
  visibleWhen?: (values: Record<string, ConfigValue>) => boolean;
  /** Takes effect only after the host restarts. */
  restart?: boolean;
  advanced?: boolean;
}

export interface Schema {
  categories: Category[];
  settings: Setting[];
}
