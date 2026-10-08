import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

globalThis.location = new URL('http://localhost/');
globalThis.window = new EventTarget();
const server = await createServer({
  server: { middlewareMode: true, hmr: false }, appType: 'custom',
  optimizeDeps: { noDiscovery: true, include: [] },
});
after(() => server.close());
const { commandFor, coverKey, startPath } = await server.ssrLoadModule('/src/lib/paths.ts');
const { default: SettingControl } = await server.ssrLoadModule('/src/components/settings/SettingControl.svelte');
const { render } = await server.ssrLoadModule('svelte/server');

test('a picked program becomes a command, quoted when its path has spaces', () => {
  assert.equal(commandFor('C:\\Games\\game.exe'), 'C:\\Games\\game.exe');
  assert.equal(commandFor('C:\\Program Files\\Game\\game.exe'), '"C:\\Program Files\\Game\\game.exe"');
});

test('browsing starts at the program of a command, or at the drives', () => {
  assert.equal(startPath('"C:\\Program Files\\Game\\game.exe" -windowed'), 'C:\\Program Files\\Game\\game.exe');
  assert.equal(startPath(' D:/Games/game.exe -a '), 'D:/Games/game.exe -a');
  assert.equal(startPath('\\\\nas\\games'), '\\\\nas\\games');
  assert.equal(startPath('steam://rungameid/570'), '');
  assert.equal(startPath('game.exe'), '');
  assert.equal(startPath(''), '');
});

test('only covers saved in the covers folder have a key for Playnite', () => {
  assert.equal(coverKey('C:\\ProgramData\\Butterpollo\\covers\\upload_1730000000000.png'), 'upload_1730000000000');
  assert.equal(coverKey('C:/ProgramData/Butterpollo/covers/igdb_1942.png'), 'igdb_1942');
  assert.equal(coverKey('C:\\Users\\me\\Pictures\\cover.png'), '');
  assert.equal(coverKey('C:\\ProgramData\\Butterpollo\\covers\\a b.png'), '');
  assert.equal(coverKey(''), '');
});

test('a text setting with browse gets a Browse button beside its field', () => {
  const setting = {
    key: 'lossless_scaling_path', label: 'Lossless Scaling program', category: 'library',
    control: { kind: 'text', mono: true, browse: 'executable' }, default: '',
  };
  const html = render(SettingControl, {
    props: { setting, id: 'lossless', value: 'C:\\LS\\LosslessScaling.exe', onchange: () => {} },
  }).body;
  assert.match(html, /<input[^>]*id="lossless"/);
  assert.match(html, />\s*Browse\s*</);
  const plain = render(SettingControl, {
    props: { setting: { ...setting, control: { kind: 'text' } }, id: 'plain', value: '', onchange: () => {} },
  }).body;
  assert.doesNotMatch(plain, /Browse/);
});
