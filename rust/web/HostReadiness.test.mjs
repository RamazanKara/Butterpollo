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
const { default: HostReadiness } = await server.ssrLoadModule('/src/components/overview/HostReadiness.svelte');
const { render } = await server.ssrLoadModule('svelte/server');
const metadata = {
  encoder_status: { state: 'failed', h264: false, hevc: false, av1: false, pyrowave: false },
  warnings: [{ code: 'video_encoder', message: 'No video encoder available: AMF error 1. Retrying.' }],
  virtual_display: { capable: false, ready: false, reason: '' },
  capture_status: { virtual_display_configured: false, displays: [], error: null },
  audio_enabled: false,
};
const html = (changes = {}) => render(HostReadiness, {
  props: { metadata: { ...metadata, ...changes }, error: null, checking: false, onrecheck: () => {} },
}).body;

test('host readiness shows the probe error even while a retry is running', () => {
  for (const state of ['failed', 'checking']) {
    const result = html({ encoder_status: { ...metadata.encoder_status, state } });
    assert.match(result, /No video encoder available: AMF error 1\. Retrying\./);
    assert.doesNotMatch(result, /restart the host|Testing which codecs/);
  }
});

test('host readiness clears the warning when encoders recover', () => {
  const result = html({ warnings: [], encoder_status: { ...metadata.encoder_status, state: 'ready', h264: true } });
  assert.doesNotMatch(result, /No video encoder available|Retrying/);
  assert.match(result, /Encodes H\.264\./);
});

test('host readiness escapes driver errors', () => {
  const result = html({ warnings: [{ code: 'video_encoder', message: 'No video encoder available: <script>alert(1)</script>. Retrying.' }] });
  assert.match(result, /&lt;script/);
  assert.doesNotMatch(result, /<script>alert/);
});
