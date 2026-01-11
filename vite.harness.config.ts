// NOTE: This Vite config is for the *repo-root dev harness*.
//
// It exists primarily for:
// - Playwright E2E that exercises low-level primitives (workers, COOP/COEP, etc.)
// - Importing source modules across the repo (e.g. `/web/src/...`) in a browser context
//
// The production/canonical browser host lives in `web/` (see ADR 0001).
import { defineConfig } from 'vite';

import {
  baselineSecurityHeaders,
  crossOriginIsolationHeaders,
  cspHeaders,
} from './scripts/security_headers.mjs';

const coopCoepSetting = (process.env.VITE_DISABLE_COOP_COEP ?? '').toLowerCase();
const coopCoepDisabled = coopCoepSetting === '1' || coopCoepSetting === 'true';

export default defineConfig({
  // Reuse `web/public` across the repo so test assets and `_headers` templates
  // are consistently available in `vite preview` runs.
  publicDir: 'web/public',
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
