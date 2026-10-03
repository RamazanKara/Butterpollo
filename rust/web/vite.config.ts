import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// `npm run dev` proxies the API to a running host. The host only accepts
// writes from its own origin, so the proxy presents that origin.
const host = process.env.BUTTERPOLLO_HOST ?? 'https://localhost:47990';

export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: false,
    target: 'es2022',
  },
  server: {
    proxy: {
      '/api': { target: host, secure: false, changeOrigin: true, headers: { origin: host } },
    },
  },
});
