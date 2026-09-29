import {
  providerSupported,
  supportsManagedLinuxDisplay,
  settingsCapabilitySupported,
} from '../utils/providerCapabilities.ts';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  matchesPlatform,
  settingsFields,
  optionsForPlatform,
  encoderFamilyFor,
  fieldForPlatform,
  captureOptionsForPlatform,
  frameGenerationOptionsForPlatform,
  gamepadOptionsForPlatform,
  settingsCategories,
  settingsDefaults,
  type SettingsField,
} from '../configs/settingsSchema.ts';
import {
  displayFieldVisibility,
  normalizeCommandRows,
  normalizeServerCommandRows,
  preserveHiddenDisplayValues,
  serializeCommandRows,
  serializeServerCommandRows,
} from '../utils/v2Parity.ts';

test('capture options follow the host platform', () => {
  assert.deepEqual(
    captureOptionsForPlatform('linux').map((option) => option.value),
    ['', 'kms', 'kwin', 'gamescope', 'portal', 'wlr', 'x11', 'nvfbc'],
  );
  assert.deepEqual(
    captureOptionsForPlatform('windows').map((option) => option.value),
    ['', 'wgc', 'wgcc', 'ddx'],
  );
  assert.deepEqual(
    captureOptionsForPlatform('macos').map((option) => option.value),
    [''],
  );
});

test('Linux remote-monitor controls and frame-generation labels are available', () => {
  for (const key of [
    'remote_monitor_mute_audio',
    'remote_monitor_disconnect_on_stream_end',
    'remote_monitor_disconnect_on_client_disconnect',
    'remote_monitor_terminate_on_first_request',
  ]) {
    assert.equal(matchesPlatform(settingsFields.get(key)!, 'linux'), true);
    assert.equal(matchesPlatform(settingsFields.get(key)!, 'windows'), true);
    assert.equal(matchesPlatform(settingsFields.get(key)!, 'macos'), false);
  }
  const messages = JSON.parse(
    readFileSync(new URL('../public/assets/locale/ui/en.json', import.meta.url), 'utf8'),
  );
  for (const option of frameGenerationOptionsForPlatform('linux')) {
    const label = option.labelKey.split('.').reduce((value, key) => value?.[key], messages);
    assert.equal(typeof label, 'string', option.labelKey);
    assert.ok(label.length);
  }
});

test('gamepad options follow the host platform', () => {
  assert.deepEqual(
    gamepadOptionsForPlatform('linux').map((option) => option.value),
    ['auto', 'xone', 'ds4', 'ds5', 'switch'],
  );
  assert.deepEqual(
    gamepadOptionsForPlatform('windows').map((option) => option.value),
    ['auto', 'vhf_xbox', 'vhf_xbox_one', 'vhf_ds4', 'vhf_ds5', 'vhf_switch'],
  );
});

test('Linux hides Windows-only input and audio installation controls', () => {
  for (const key of ['always_send_scancodes', 'native_pen_touch', 'install_steam_audio_drivers']) {
    assert.equal(settingsFields.get(key)?.platform, 'windows', key);
  }
});

test('Linux Proton and MangoHUD limiter choices are available', () => {
  assert.equal(settingsDefaults.frame_limiter_provider, 'auto');
  assert.equal(settingsDefaults.mangohud_limiter_method, 'late');

  const fields = settingsCategories.flatMap((category) =>
    category.groups.flatMap((group) => group.fields),
  );
  const method = fields.find((field) => field.key === 'mangohud_limiter_method');
  assert.deepEqual(method?.platform, 'linux');
  assert.deepEqual(method?.visibleWhen, {
    key: 'frame_limiter_provider',
    equals: 'mangohud',
  });
  assert.deepEqual(
    method?.options?.map((option) => option.value),
    ['early', 'late'],
  );

  const messages = JSON.parse(
    readFileSync(new URL('../public/assets/locale/ui/en.json', import.meta.url), 'utf8'),
  );
  assert.match(messages.ui.integrations.mangohud.providerAuto, /Proton.*MangoHUD/i);
  assert.match(messages.ui.integrations.mangohud.limiterMethodDescription, /latency/i);
  assert.match(messages.ui.integrations.mangohud.limiterMethodDescription, /frame generation/i);
});

test('Linux maintenance omits Windows-only support and recovery sections', () => {
  const maintenanceView = readFileSync(
    new URL('../views/MaintenanceView.vue', import.meta.url),
    'utf8',
  );
  assert.match(maintenanceView, /v-if="isWindows"[\s\S]*aria-labelledby="display-recovery-title"/);
  assert.match(maintenanceView, /v-if="isWindows"[^>]*aria-labelledby="support-title"/);
  assert.doesNotMatch(maintenanceView, /ui\.maintenance\.support\.windowsUnavailable/);
});

test('Settings protects drafts and keeps restart actions available', () => {
  const settingsView = readFileSync(new URL('../views/SettingsView.vue', import.meta.url), 'utf8');
  assert.match(settingsView, /<form[\s\S]*@submit\.prevent="save"/);
  assert.match(settingsView, /class="button button--primary" type="submit"/);
  assert.match(settingsView, /:disabled="loading \|\| saving \|\| isDirty"/);
  assert.match(settingsView, /restartAvailable\.value \|\|= Boolean\(result\.restartRequired\)/);
  assert.match(settingsView, /v-else-if="notice \|\| restartAvailable"/);
  assert.match(settingsView, /:disabled="restarting"/);
});

test('Settings explains unavailable host metadata and virtual-display readiness', () => {
  const settingsView = readFileSync(new URL('../views/SettingsView.vue', import.meta.url), 'utf8');
  assert.match(settingsView, /metadataUnavailable\.value = true/);
  assert.match(settingsView, /virtualDisplayUnavailable/);

  const messages = JSON.parse(
    readFileSync(new URL('../public/assets/locale/ui/en.json', import.meta.url), 'utf8'),
  );
  assert.equal(typeof messages.ui.settings.metadata_unavailable.description, 'string');
  assert.equal(typeof messages.ui.settings.virtual_display_unavailable.description, 'string');
});

test('AMD speed default agrees across backend and settings UI', () => {
  assert.match(
    readFileSync(new URL('../configs/settingsSchema.ts', import.meta.url), 'utf8'),
    /amd_quality:\s*'speed'/,
  );
  const backend = readFileSync(new URL('../../../../../src/config.cpp', import.meta.url), 'utf8');
  for (const codec of ['h264', 'hevc', 'av1']) {
    assert.ok(backend.includes(`amd::quality_${codec}_e::speed,  // quality (${codec})`));
  }
});

test('global command rows preserve order, verbatim text, and Windows elevation', () => {
  const source = [
    { do: '  set-mode "A"  ', undo: 'restore A', elevated: true, custom: 'keep' },
    { do: 'second', undo: '', elevated: false },
  ];
  const rows = normalizeCommandRows(source, 'windows');
  assert.deepEqual(serializeCommandRows(rows, 'windows'), source);
  assert.deepEqual(serializeCommandRows(source, 'linux'), [
    { do: '  set-mode "A"  ', undo: 'restore A', custom: 'keep' },
    { do: 'second', undo: '' },
  ]);
});

test('persisted command JSON is available to the command editor', () => {
  const persisted = JSON.stringify([{ do: 'connect', undo: 'disconnect', elevated: true }]);
  assert.deepEqual(normalizeCommandRows(persisted, 'windows'), [
    { do: 'connect', undo: 'disconnect', elevated: true },
  ]);
});

test('display visibility calls disabled Physical and does not clear hidden values', () => {
  assert.deepEqual(displayFieldVisibility('disabled'), { physical: true, virtual: false });
  assert.deepEqual(displayFieldVisibility('per_client'), { physical: false, virtual: true });
  assert.deepEqual(
    preserveHiddenDisplayValues(
      { dd_virtual_display_scale: 125 },
      { virtual_display_mode: 'disabled' },
    ),
    { dd_virtual_display_scale: 125, virtual_display_mode: 'disabled' },
  );
});

test('advanced encoder settings have actual fields and platform-aware options', () => {
  for (const key of [
    'nvenc_twopass',
    'nvenc_spatial_aq',
    'qsv_coder',
    'amd_rc',
    'vaapi_strict_rc_buffer',
    'vk_tune',
    'sw_preset',
    'vt_software',
    'keybindings',
    'session_token_ttl_seconds',
  ])
    assert.ok(settingsFields.has(key), key);
  assert.equal(encoderFamilyFor('vaapi'), 'vaapi');
  assert.equal(encoderFamilyFor('vulkan'), 'vulkan');
  assert.equal(encoderFamilyFor('nvenc_legacy'), 'nvidia');
  assert.equal(matchesPlatform(settingsFields.get('qsv_coder')!, 'linux'), false);
  assert.equal(fieldForPlatform(settingsFields.get('adapter_name')!, 'linux').kind, 'text');
  assert.equal(
    optionsForPlatform(settingsFields.get('virtual_display_mode')!, 'linux')[0].value,
    'per_client',
  );
});

test('client command edits survive the latest-device merge before save', () => {
  const devicesView = readFileSync(new URL('../views/DevicesView.vue', import.meta.url), 'utf8');
  assert.match(
    devicesView,
    /'allowClientCommands',\s*'doCommands',\s*'undoCommands',\s*'displayMode'/,
  );
});

test('server command rows round-trip for the Vibepollo editor', () => {
  const server = normalizeServerCommandRows(
    JSON.stringify([{ name: 'Open overlay', cmd: 'overlay.exe', elevated: true }]),
    'windows',
  );
  assert.deepEqual(serializeServerCommandRows(server, 'windows'), [
    { name: 'Open overlay', cmd: 'overlay.exe', elevated: true },
  ]);
});

test('provider actions and private Linux controls require backend capabilities', () => {
  for (const metadata of [undefined, {}, { platform: 'linux' }, { platform: 'windows' }]) {
    assert.equal(providerSupported(metadata, 'mangohud'), false);
  }
  assert.equal(providerSupported({ providers: { mangohud: 'true' } }, 'mangohud'), false);
  assert.equal(providerSupported({ providers: { mangohud: true } }, 'mangohud'), true);
  assert.equal(supportsManagedLinuxDisplay({ platform: 'linux' }), false);
  for (const key of ['virtual_display_mode', 'dd_refresh_rate_option', 'frame_limiter_provider'])
    assert.equal(settingsCapabilitySupported(key, { platform: 'linux' }), false);
  assert.equal(settingsCapabilitySupported('virtual_display_mode', { platform: 'windows' }), true);
  assert.equal(
    settingsCapabilitySupported('virtual_display_mode', {
      platform: 'linux',
      virtual_display: { backend: 'kscreen-vkms' },
    }),
    true,
  );
  assert.equal(settingsCapabilitySupported('stream_audio', { platform: 'linux' }), true);
});
