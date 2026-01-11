// NOTE: This Vite config is for the *repo-root dev harness*.
//
// It exists primarily for:
// - Playwright E2E that exercises low-level primitives (workers, COOP/COEP, etc.)
// - Importing source modules across the repo (e.g. `/web/src/...`) in a browser context
//
// The production/canonical browser host lives in `web/` (see ADR 0001).
import { defineConfig, type Connect, type Plugin } from 'vite';

import {
  baselineSecurityHeaders,
  crossOriginIsolationHeaders,
  cspHeaders,
} from './scripts/security_headers.mjs';

const coopCoepSetting = (process.env.VITE_DISABLE_COOP_COEP ?? '').toLowerCase();
const coopCoepDisabled = coopCoepSetting === '1' || coopCoepSetting === 'true';

function wasmMimeTypePlugin(): Plugin {
  const installWasmMiddleware = (middlewares: Connect.Server) => {
    middlewares.use((req, res, next) => {
      // `instantiateStreaming` requires the correct MIME type.
      const pathname = req.url?.split('?', 1)[0];
      if (pathname?.endsWith('.wasm')) {
        res.setHeader('Content-Type', 'application/wasm');
      }
      next();
    });
  };

  return {
    name: 'wasm-mime-type',
    configureServer(server) {
      installWasmMiddleware(server.middlewares);
    },
    configurePreviewServer(server) {
      installWasmMiddleware(server.middlewares);
    },
  };
}

export default defineConfig({
  // Reuse `web/public` across the repo so test assets and `_headers` templates
  // are consistently available in `vite preview` runs.
  publicDir: 'web/public',
  plugins: [wasmMimeTypePlugin()],
  // The repo heavily relies on module workers (`type: 'module'` + `import.meta.url`).
  // Keep the harness build aligned with `web/vite.config.ts` so worker bundling
  // supports code-splitting.
  worker: {
    format: 'es',
  },
  server: {
    headers: {
      ...(coopCoepDisabled ? {} : crossOriginIsolationHeaders),
      ...baselineSecurityHeaders,
    },
  },
  preview: {
    headers: {
      ...(coopCoepDisabled ? {} : crossOriginIsolationHeaders),
      ...baselineSecurityHeaders,
      ...cspHeaders,
    },
  },
});
