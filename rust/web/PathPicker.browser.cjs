// Uses the same optional Playwright runtime as tests/console_browser.cjs, with no host process.
const assert = require('node:assert/strict');
const { before, after, test } = require('node:test');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

let server, browser, base;
before(async () => {
  const { createServer } = await import('vite');
  const { svelte } = await import('@sveltejs/vite-plugin-svelte');
  server = await createServer({
    root: __dirname, configFile: false,
    plugins: [svelte(), {
      name: 'parity-fixture',
      resolveId(id) { if (id === '/parity-test.js') return '\0parity-test'; },
      load(id) {
        if (id !== '\0parity-test') return;
        return `
          import { mount } from 'svelte';
          import AppEditor from '/src/components/library/AppEditor.svelte';
          import Settings from '/src/pages/Settings.svelte';
          mount(location.pathname.startsWith('/settings') ? Settings : AppEditor, {
            target: document.body,
            props: { position: null, status: null, onsaved: async () => {},
              onmove: async () => {}, ondeleted: () => {}, onstatus: () => {} }
          });`;
      },
      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          if (!['/library/new', '/settings/library'].includes(req.url)) return next();
          res.setHeader('Content-Type', 'text/html');
          res.end('<!doctype html><html><body><script type="module" src="/parity-test.js"></script></body></html>');
        });
      },
    }],
    server: { host: '127.0.0.1', port: 0, hmr: false },
  });
  await server.listen();
  base = `http://127.0.0.1:${server.httpServer.address().port}`;
  browser = await chromium.launch({
    headless: true, args: ['--disable-gpu', '--disable-background-networking'],
    ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}),
  });
});
after(async () => { await browser?.close(); await server?.close(); });

async function fixture(t) {
  const page = await browser.newPage();
  page.setDefaultTimeout(10000);
  t.after(() => page.close());
  const errors = [], writes = [], browsing = [];
  let config = {};
  page.on('pageerror', error => errors.push(error.message));
  await page.route('**/*', async route => {
    const request = route.request(), url = new URL(request.url());
    assert.equal(url.origin, base, 'only the local fixture may receive requests');
    if (!url.pathname.startsWith('/api/')) return route.continue();
    const reply = body => route.fulfill({ json: body });
    if (url.pathname === '/api/browse') {
      const path = url.searchParams.get('path');
      browsing.push({ path, type: url.searchParams.get('type') });
      const entries = path === ''
        ? [{ name: 'Programs', path: 'C:\\Programs', type: 'directory' }]
        : path === 'C:\\Programs'
          ? [{ name: 'Test App.exe', path: 'C:\\Programs\\Test App.exe', type: 'file' },
             { name: 'Game data', path: 'C:\\Programs\\Game data', type: 'directory' }]
          : [];
      return reply({ path, parent: '', entries });
    }
    if (url.pathname === '/api/csrf-token') return reply({ csrf_token: 'fixture-token' });
    if (url.pathname === '/api/metadata') return reply({});
    if (request.method() === 'GET' && url.pathname === '/api/config') return reply(config);
    if (['/api/apps', '/api/config'].includes(url.pathname)) {
      assert.equal(request.headers()['x-csrf-token'], 'fixture-token');
      const body = request.postDataJSON();
      writes.push({ method: request.method(), path: url.pathname, body });
      if (url.pathname === '/api/config') config = { ...config, ...body };
      return reply({ status: true, uuid: 'saved-app', restart_required: false });
    }
    assert.fail(`unexpected API request: ${url.pathname}`);
  });
  return { page, errors, writes, browsing };
}

test('the app picker saves a quoted command and the chosen working directory', async t => {
  const { page, errors, writes, browsing } = await fixture(t);
  await page.goto(base + '/library/new');
  await page.getByLabel('Name', { exact: true }).fill('Picker fixture');
  const command = page.locator('.path-picker').filter({ has: page.getByLabel('Command', { exact: true }) });
  await command.getByRole('button', { name: 'Browse', exact: true }).click();
  await command.getByRole('button', { name: 'Programs', exact: true }).click();
  await command.getByRole('button', { name: /Test App.exe/ }).click();
  assert.equal(await page.getByLabel('Command', { exact: true }).inputValue(), '"C:\\Programs\\Test App.exe"');
  const folder = page.locator('.path-picker').filter({ has: page.getByLabel('Working directory', { exact: true }) });
  await folder.getByRole('button', { name: 'Browse', exact: true }).click();
  await folder.getByRole('button', { name: 'Programs', exact: true }).click();
  await folder.getByRole('button', { name: 'Game data', exact: true }).click();
  await folder.getByRole('button', { name: 'Use this folder', exact: true }).click();
  const saved = page.waitForResponse(response => response.url().endsWith('/api/apps') && response.request().method() === 'POST');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await saved;
  assert.equal(writes.length, 1);
  assert.equal(writes[0].body.cmd, '"C:\\Programs\\Test App.exe"');
  assert.equal(writes[0].body['working-dir'], 'C:\\Programs\\Game data');
  assert.deepEqual(browsing.map(entry => entry.type), ['executable', 'executable', 'directory', 'directory', 'directory']);
  assert.deepEqual(errors, []);
});

test('settings pickers save Lossless Scaling and RTSS paths through the config API', async t => {
  const { page, errors, writes } = await fixture(t);
  await page.goto(base + '/settings/library');
  for (const key of ['lossless_scaling_path', 'rtss_install_path']) {
    await page.getByRole('searchbox', { name: 'Search settings' }).fill(key);
    const picker = page.locator('.path-picker');
    await picker.getByRole('button', { name: 'Browse', exact: true }).click();
    await picker.getByRole('button', { name: 'Programs', exact: true }).click();
    if (key === 'rtss_install_path') await picker.getByRole('button', { name: 'Use this folder', exact: true }).click();
    else await picker.getByRole('button', { name: /Test App.exe/ }).click();
    const saved = page.waitForResponse(response => response.url().endsWith('/api/config') && response.request().method() === 'PATCH');
    await page.getByRole('button', { name: 'Save changes', exact: true }).click();
    await saved;
  }
  assert.deepEqual(writes.map(write => write.body), [
    { lossless_scaling_path: 'C:\\Programs\\Test App.exe' },
    { rtss_install_path: 'C:\\Programs' },
  ]);
  assert.deepEqual(errors, []);
});
