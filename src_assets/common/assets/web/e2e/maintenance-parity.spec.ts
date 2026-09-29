import { test, expect, type Page } from '@playwright/test';

interface HostOptions {
  platform?: 'windows' | 'linux';
}

async function setupHost(page: Page, options: HostOptions = {}) {
  const platform = options.platform ?? 'windows';
  const calls = {
    crashManifest: 0,
    crashParts: [] as number[],
  };
  let failedPartTwo = true;

  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (!url.pathname.startsWith('/api/')) {
      await route.continue();
      return;
    }
    const path = url.pathname;
    const method = request.method();
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
        prerelease: '',
        status: true,
        windows_build_number: 26100,
        encoder_status: { state: 'ready', h264: true },
        capture_status: { virtual_display_configured: true },
      };
    } else if (path === '/api/auth/sessions') {
      body = { sessions: [] };
    } else if (path === '/api/health/crashdump') {
      body = { available: true, filename: 'crash.dmp', size_bytes: 1234 };
    } else if (path === '/api/display/golden_status') {
      body = { exists: false };
    } else if (path === '/api/apps') {
      body = { apps: [] };
    } else if (path === '/api/rtss/status') {
      body = { enabled: false, path_exists: false };
    } else if (path === '/api/vigembus/status') {
      body = { installed: true, version_compatible: true };
    } else if (path === '/api/health/vulkan-hdr-layer') {
      body = { installed: true, enabled: false };
    } else if (path === '/api/config' && method === 'GET') {
      body = { capture: 'wgc', encoder: 'nvenc' };
    } else if (path === '/api/logs/export_crash/manifest') {
      calls.crashManifest += 1;
      body = {
        parts: [
          { index: 1, filename: 'crash-part1.zip', estimated_size_bytes: 10 },
          { index: 2, filename: 'crash-part2.zip', estimated_size_bytes: 20 },
        ],
      };
    } else if (path === '/api/logs/export_crash') {
      const index = Number(url.searchParams.get('part') ?? 1);
      calls.crashParts.push(index);
      if (index === 2 && failedPartTwo) {
        failedPartTwo = false;
        await route.fulfill({ status: 503, json: { error: 'part unavailable' } });
        return;
      }
      await route.fulfill({
        status: 200,
        body: `zip-part-${index}`,
        headers: {
          'content-type': 'application/zip',
          'content-disposition': `attachment; filename="crash-part${index}.zip"`,
        },
      });
      return;
    }

    await route.fulfill({ json: body });
  });
  await page.route('**/assets/changelog.json', async (route) =>
    route.fulfill({ json: { releases: [] } }),
  );
  return calls;
}

test('Windows maintenance exposes every crash part and recovers a failed part', async ({
  page,
}) => {
  const calls = await setupHost(page);
  await page.goto('/v2/maintenance');
  await expect(page.getByRole('button', { name: 'Download crash bundle' })).toBeVisible();
  await page.getByRole('button', { name: 'Download crash bundle' }).click();
  await expect(page.getByText('Crash bundle parts', { exact: true })).toBeVisible();
  await expect(page.getByText('crash-part1.zip', { exact: true })).toBeVisible();
  await expect(page.getByText('crash-part2.zip', { exact: true })).toBeVisible();
  await expect(page.getByText(/parts 2 failed/)).toBeVisible();
  expect(calls.crashManifest).toBe(1);
  expect(calls.crashParts).toEqual([1, 2]);

  await page.getByRole('button', { name: 'Retry part' }).click();
  await expect.poll(() => calls.crashParts).toEqual([1, 2, 2]);
  await expect(page.getByText(/Crash bundle downloads started/)).toBeVisible();
  await page.screenshot({
    path: '/tmp/vibeshine-ui-results/maintenance-crash-desktop.png',
    animations: 'disabled',
    fullPage: false,
  });
  await page.setViewportSize({ width: 390, height: 1000 });
  await page.screenshot({
    path: '/tmp/vibeshine-ui-results/maintenance-crash-mobile.png',
    animations: 'disabled',
    fullPage: false,
  });
});
