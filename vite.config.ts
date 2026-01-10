import { defineConfig } from 'vite';

const crossOriginIsolationHeaders: Record<string, string> = {
  // Required for SharedArrayBuffer / WASM threads (crossOriginIsolated === true)
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

export default defineConfig({
  server: {
    headers: crossOriginIsolationHeaders,
  },
  preview: {
    headers: crossOriginIsolationHeaders,
  },
});
