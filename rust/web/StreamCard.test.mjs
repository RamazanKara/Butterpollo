import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import { createServer } from 'vite';

globalThis.location = new URL('http://localhost/');
globalThis.window = new EventTarget();
const server = await createServer({
  server: { middlewareMode: true }, appType: 'custom',
  optimizeDeps: { noDiscovery: true, include: [] },
});
after(() => server.close());
const { default: StreamCard } = await server.ssrLoadModule('/src/components/overview/StreamCard.svelte');
const { render } = await server.ssrLoadModule('svelte/server');
const stream = {
  device_name: 'Test client', state: 'RUNNING', width: 3840, height: 2160, fps: 116,
  video_format: 1, encoder: 'amf', encoder_bitrate_kbps: 100_000,
  pyrowave_minimum_kbps: null, pyrowave_recommended_kbps: null,
  uptime_seconds: 1, performance: { sample_frames: 0, history: [] }, warnings: [],
};
const html = (changes = {}) => render(StreamCard, {
  props: { stream: { ...stream, ...changes }, ondisconnect: () => {} },
}).body;

test('stream card renders the backend and actionable warnings as escaped text', () => {
  const result = html({ warnings: [
    { code: 'capture', message: 'Capturing with Desktop Duplication: WGC unavailable. Sign in again.' },
    { code: 'encoder', message: 'Driver failed: <script>alert(1)</script>' },
  ] });
  assert.match(result, /Encoder: amf/);
  assert.match(result, /role="status"[^>]*>Capturing with Desktop Duplication/);
  assert.match(result, /&lt;script/);
  assert.doesNotMatch(result, /<script>alert/);
});

test('recovered warnings disappear without invented timing measurements', () => {
  const result = html();
  assert.doesNotMatch(result, /role="status"/);
  assert.match(result, /No frames sent in the last 2 seconds/);
});

test('PyroWave quality warning uses the current encoder bitrate', () => {
  const result = html({ video_format: 3, pyrowave_minimum_kbps: 200_000, pyrowave_recommended_kbps: 300_000 });
  assert.match(result, /PyroWave bitrate is too low/);
  assert.match(result, /severe detail loss/);
});
