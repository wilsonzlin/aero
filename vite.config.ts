import { defineConfig } from 'vite';

import {
  baselineSecurityHeaders,
  crossOriginIsolationHeaders,
  cspHeaders,
} from './scripts/security_headers.mjs';

const coopCoepDisabled =
  process.env.VITE_DISABLE_COOP_COEP === '1' || process.env.VITE_DISABLE_COOP_COEP === 'true';

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
