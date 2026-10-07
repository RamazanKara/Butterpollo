import assert from 'node:assert/strict';
import test from 'node:test';
import { settings } from '../web/src/lib/schema/video.ts';

const presets = ['amd_quality', 'nvenc_preset', 'qsv_preset', 'sw_preset'];
const visiblePresets = (encoder?: string) =>
  settings.filter((setting) =>
    presets.includes(setting.key) && setting.visibleWhen?.(encoder === undefined ? {} : { encoder }))
    .map((setting) => setting.key);

test('automatic selection does not imply that a vendor encoder is active', () => {
  for (const encoder of [undefined, '', 'auto', 'pyrowave', 'mediafoundation']) {
    assert.deepEqual(visiblePresets(encoder), []);
  }
});

test('explicit and imported encoder selections show their own settings', () => {
  for (const encoder of ['amf', 'amdvce', 'amdvce_experimental', 'amdvce_ffmpeg', 'amdvce_legacy']) {
    assert.deepEqual(visiblePresets(encoder), ['amd_quality']);
  }
  for (const encoder of ['nvenc', 'nvenc_legacy', 'nvenc_experimental']) {
    assert.deepEqual(visiblePresets(encoder), ['nvenc_preset']);
  }
  for (const encoder of ['qsv', 'quicksync']) {
    assert.deepEqual(visiblePresets(encoder), ['qsv_preset']);
  }
  assert.deepEqual(visiblePresets('software'), ['sw_preset']);
});
