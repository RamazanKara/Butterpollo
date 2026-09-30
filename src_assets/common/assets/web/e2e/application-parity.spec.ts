import { test, expect, type Page } from '@playwright/test';

const appUuid = '11111111-1111-4111-8111-111111111111';

interface HostOptions {
  platform?: 'windows' | 'unknown';
  apps?: Array<Record<string, unknown>>;
}

async function setupHost(page: Page, options: HostOptions = {}) {
  const platform = options.platform ?? 'windows';
  let apps = [...(options.apps ?? [])];
  const saves: Record<string, unknown>[] = [];

  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const method = request.method();
    if (!path.startsWith('/api/')) {
      await route.continue();
      return;
    }

    let body: unknown = { status: true };
    if (path === '/api/auth/status') {
      body = { authenticated: true, login_required: false, credentials_configured: true };
    } else if (path === '/api/configLocale') {
      body = { locale: 'en' };
    } else if (path === '/api/csrf-token') {
      body = { csrf_token: 'test-token' };
    } else if (path === '/api/metadata') {
      body = {
        platform,
        version: '1.0.0',
        windows_build_number: platform === 'windows' ? 26100 : undefined,
        has_nvidia_gpu: platform === 'windows',
      };
    } else if (path === '/api/apps' && method === 'GET') {
      body = { apps };
    } else if (path === '/api/apps' && method === 'POST') {
      const saved = request.postDataJSON() as Record<string, unknown>;
      saves.push(saved);
      apps = [saved];
      body = { status: true };
    } else if (path === '/api/config') {
      body = { capture: 'wgc' };
    } else if (path === '/api/rtss/status') {
      body = { path_exists: true, hooks_found: true };
    } else if (path === '/api/display-devices') {
      body = [];
    }
    await route.fulfill({ json: body });
  });
  return saves;
}

test('new application keeps v1 lifecycle defaults and saves launch controls', async ({ page }) => {
  const saves = await setupHost(page);
  await page.goto('/library/new');
  await expect(page.locator('#app-auto-detach')).toBeChecked();
  await expect(page.locator('#app-wait-all')).toBeChecked();
  await expect(page.locator('#app-exit-timeout')).toHaveValue('5');
  await page.locator('#app-name').fill('Parity test app');
  await page.getByRole('button', { name: 'Save application', exact: true }).last().click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toMatchObject({
    name: 'Parity test app',
    'auto-detach': true,
    'wait-all': true,
    'exclude-global-prep-cmd': false,
    'exit-timeout': 5,
  });
});

test('existing application without lifecycle keys gets v1 defaults and saves changed controls', async ({
  page,
}) => {
  const saves = await setupHost(page, {
    apps: [{ uuid: appUuid, name: 'Absent-key app', cmd: 'C:\\Games\\game.exe' }],
  });
  await page.goto(`/library/${appUuid}`);
  await expect(page.locator('#app-auto-detach')).toBeChecked();
  await expect(page.locator('#app-wait-all')).toBeChecked();
  await expect(page.locator('#app-exit-timeout')).toHaveValue('5');

  await page.locator('#app-auto-detach').uncheck();
  await page.locator('#app-wait-all').uncheck();
  await page.locator('#app-exclude-global-prep').check();
  await page.locator('#app-elevated').check();
  await page.locator('#app-exit-timeout').fill('0');
  await page.getByRole('button', { name: 'Save application', exact: true }).last().click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toMatchObject({
    'auto-detach': false,
    'wait-all': false,
    'exclude-global-prep-cmd': true,
    elevated: true,
    'exit-timeout': 0,
  });
});

test('existing application preserves explicit false and zero, and retired fields', async ({
  page,
}) => {
  const saves = await setupHost(page, {
    apps: [
      {
        uuid: appUuid,
        name: 'Retired fields app',
        cmd: 'C:\\Games\\game.exe',
        'auto-detach': false,
        'wait-all': false,
        'exit-timeout': 0,
        'frame-generation-mode': 'lossless-scaling',
        'lossless-scaling-target-fps': 120,
        'playnite-id': 'retired-game',
        'future-field': 'preserve-me',
      },
    ],
  });
  await page.goto(`/library/${appUuid}`);
  await expect(page.locator('#app-auto-detach')).not.toBeChecked();
  await expect(page.locator('#app-wait-all')).not.toBeChecked();
  await expect(page.locator('#app-exit-timeout')).toHaveValue('0');
  // The host treats the retired Lossless Scaling provider as frame generation off.
  await expect(page.locator('#app-frame-mode')).toHaveValue('off');
  await page.locator('#app-exclude-global-prep').check();
  await page.getByRole('button', { name: 'Save application', exact: true }).last().click();
  await expect.poll(() => saves.length).toBe(1);
  expect(saves[0]).toMatchObject({
    'auto-detach': false,
    'wait-all': false,
    'exit-timeout': 0,
    'frame-generation-mode': 'off',
    'future-field': 'preserve-me',
    'lossless-scaling-target-fps': 120,
    'playnite-id': 'retired-game',
  });
});

test('Missing host metadata hides controls until Windows is confirmed', async ({ page }) => {
  await setupHost(page, {
    platform: 'unknown',
    apps: [{ uuid: appUuid, name: 'App on an unknown host' }],
  });
  await page.goto(`/library/${appUuid}`);
  await expect(page.locator('#app-auto-detach')).toBeVisible();
  await expect(page.locator('#app-elevated')).toHaveCount(0);
});
