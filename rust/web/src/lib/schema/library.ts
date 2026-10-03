// Settings for game library integrations.
import type { ConfigValue } from '../api';
import type { Setting } from '../settings-types';

type Values = Record<string, ConfigValue>;

/** A switch as the host reads it, including the spellings Vibepollo wrote. */
function on(values: Values, key: string, fallback: boolean): boolean {
  const value = values[key] ?? fallback;
  if (typeof value === 'boolean') return value;
  const word = String(value).trim().replace(/^"(.*)"$/, '$1').toLowerCase();
  if (['true', 'yes', '1', 'enable', 'enabled', 'on'].includes(word)) return true;
  if (['false', 'no', '0', 'disable', 'disabled', 'off'].includes(word)) return false;
  return fallback;
}

const steam = (values: Values) => on(values, 'steam_enabled', false);
const recent = (values: Values) => steam(values) && !on(values, 'steam_sync_all_installed', false);

export const settings: Setting[] = [
  {
    key: 'steam_enabled',
    label: 'Steam library',
    description:
      'Adds your Steam games to the library, with their covers, and ends a stream when the game it started exits. Library › Sync Steam adds them now.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'steam_auto_sync',
    label: 'Keep in step',
    description: 'Checks Steam every 30 seconds and updates the Steam apps when games are installed, removed or played.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: steam,
  },
  {
    key: 'steam_sync_all_installed',
    label: 'Every installed game',
    description: 'Adds every installed game. When off, only the games played most recently are added.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: steam,
  },
  {
    key: 'steam_recent_games',
    label: 'Recent games',
    description: 'How many of the most recently played games to add.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'number', min: 0, max: 100 },
    default: 10,
    visibleWhen: recent,
  },
  {
    key: 'steam_recent_max_age_days',
    label: 'Played within',
    description: 'Leaves out games not played for this many days. 0 adds them however long ago they were played.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'number', min: 0, max: 3650, unit: 'days' },
    default: 30,
    visibleWhen: recent,
  },
  {
    key: 'steam_autosync_remove_uninstalled',
    label: 'Remove uninstalled games',
    description:
      'Removes a Steam app when its game is uninstalled. With recent games only, apps that fall out of the list are always removed.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: steam,
  },
  {
    key: 'steam_include_tools',
    label: 'Include tools',
    description: 'Also adds tools, runtimes, redistributables and DLC that Steam lists as installed.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: steam,
    advanced: true,
  },
  {
    key: 'steam_exclude_games',
    label: 'Games to leave out',
    description:
      'A JSON list of games the sync never adds, by Steam app ID or name: ["570", {"name": "Dota 2"}]. Their Steam apps are removed.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'json' },
    default: [],
    visibleWhen: steam,
  },
];
