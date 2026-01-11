import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const headersPath = resolve(scriptsDir, 'headers.json');

/**
 * Canonical security header values used across:
 * - Vite dev + preview servers
 * - Static hosting templates (`_headers`, `vercel.json`)
 * - Reverse proxy templates (nginx/caddy)
 *
 * Do not duplicate these header strings elsewhere; instead, consume the exports
 * from this module and let CI enforce that templates match.
 */
const raw = JSON.parse(readFileSync(headersPath, 'utf8'));

export const crossOriginIsolationHeaders = raw.crossOriginIsolation;
export const baselineSecurityHeaders = raw.baseline;
export const cspHeaders = raw.contentSecurityPolicy;

export const canonicalSecurityHeaders = {
  ...crossOriginIsolationHeaders,
  ...baselineSecurityHeaders,
  ...cspHeaders,
};

// `vite dev` intentionally omits CSP (HMR/websocket tooling can be sensitive to it).
export const viteDevHeaders = {
  ...crossOriginIsolationHeaders,
  ...baselineSecurityHeaders,
};

export const vitePreviewHeaders = canonicalSecurityHeaders;

