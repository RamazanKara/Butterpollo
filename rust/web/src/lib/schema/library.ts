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
const playnite = (values: Values) => on(values, 'playnite_enabled', true);
const playniteSync = (values: Values) => playnite(values) && on(values, 'playnite_auto_sync', true);
const playniteRecent = (values: Values) => playniteSync(values) && !on(values, 'playnite_sync_all_installed', false);

const playniteSettings: Setting[] = [
  {
    key: 'playnite_enabled',
    label: 'Playnite',
    description:
      'Starts apps linked to a Playnite game through Playnite, with the stream’s settings, and ends the stream when Playnite reports the game closed. Needs the Butterpollo plugin in Playnite (Library › Install plugin).',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'toggle' },
    default: true,
  },
  {
    key: 'playnite_auto_sync',
    label: 'Add games automatically',
    description:
      'While Playnite runs, adds the games chosen below as apps and removes them when they no longer qualify. When off, only apps already linked to a game are updated.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: playnite,
  },
  {
    key: 'playnite_sync_all_installed',
    label: 'Every installed game',
    description: 'Adds every installed game. When off, the recently played games and the categories and plugins below are added.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: playniteSync,
  },
  {
    key: 'playnite_recent_games',
    label: 'Recent games',
    description: 'How many of the most recently played games to add. 0 adds none.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'number', min: 0, max: 200 },
    default: 10,
    visibleWhen: playniteRecent,
  },
  {
    key: 'playnite_recent_max_age_days',
    label: 'Played within',
    description: 'Recent games must have been played within this many days. 0 counts any game, played or not.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'number', min: 0, max: 3650, unit: 'days' },
    default: 30,
    visibleWhen: playniteRecent,
  },
  {
    key: 'playnite_sync_categories',
    label: 'Categories to add',
    description: 'A JSON list of Playnite category names whose installed games are added: ["Couch", "Racing"].',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'json' },
    default: [],
    visibleWhen: playniteSync,
  },
  {
    key: 'playnite_sync_plugins',
    label: 'Libraries to add',
    description: 'A JSON list of Playnite library plugin IDs whose installed games are added.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'json' },
    default: [],
    visibleWhen: playniteSync,
    advanced: true,
  },
  {
    key: 'playnite_exclude_games',
    label: 'Games to leave out',
    description: 'A JSON list of Playnite game IDs the sync never adds.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'json' },
    default: [],
    visibleWhen: playniteSync,
  },
  {
    key: 'playnite_exclude_categories',
    label: 'Categories to leave out',
    description: 'A JSON list of category names whose games the sync never adds.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'json' },
    default: [],
    visibleWhen: playniteSync,
    advanced: true,
  },
  {
    key: 'playnite_exclude_plugins',
    label: 'Libraries to leave out',
    description: 'A JSON list of library plugin IDs whose games the sync never adds.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'json' },
    default: [],
    visibleWhen: playniteSync,
    advanced: true,
  },
  {
    key: 'playnite_autosync_remove_uninstalled',
    label: 'Remove uninstalled games',
    description: 'Removes an added app when its game is uninstalled.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: playniteSync,
  },
  {
    key: 'playnite_autosync_delete_after_days',
    label: 'Remove unplayed after',
    description: 'Removes an added app not played within this many days of being added. 0 keeps it.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'number', min: 0, max: 3650, unit: 'days' },
    default: 14,
    visibleWhen: playniteSync,
  },
  {
    key: 'playnite_autosync_require_replacement',
    label: 'Replace older recent games',
    description: 'When more recent games are played, removes as many older added games as are newly added, keeping the count.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: playniteRecent,
    advanced: true,
  },
  {
    key: 'playnite_fullscreen_entry_enabled',
    label: 'Playnite fullscreen app',
    description: 'Adds a “Playnite (Fullscreen)” app that opens Playnite’s fullscreen mode, for choosing a game on the device.',
    category: 'library',
    group: 'Playnite',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: playnite,
  },
];

export const settings: Setting[] = [
  {
    key: 'steam_enabled',
    label: 'Steam library',
    description:
      'Adds your Steam games to the library, with their covers, and ends a stream when the game it started exits. In Library, select Sync now beside Steam to add them now.',
    category: 'library',
    group: 'Steam',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'steam_auto_sync',
    label: 'Sync automatically',
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
  ...playniteSettings,
  {
    key: 'lossless_scaling_path',
    label: 'Lossless Scaling program',
    description:
      'LosslessScaling.exe or its folder, for apps that use Lossless Scaling. Empty finds it in Steam libraries and the usual folders.',
    category: 'library',
    group: 'Lossless Scaling',
    control: { kind: 'text', placeholder: 'Found automatically', mono: true, browse: 'executable' },
    default: '',
  },
  {
    key: 'lossless_scaling_legacy_auto_detect',
    label: 'Let Lossless Scaling start by itself',
    description:
      'Uses Lossless Scaling’s auto scale for the game instead of pressing its hotkey, and leaves its window open. An app can choose otherwise.',
    category: 'library',
    group: 'Lossless Scaling',
    control: { kind: 'toggle' },
    default: false,
  },
];
