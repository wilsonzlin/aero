import { defineConfig } from 'vite';

const crossOriginIsolationHeaders: Record<string, string> = {
  // Required for SharedArrayBuffer / WASM threads (crossOriginIsolated === true)
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
  'Cross-Origin-Resource-Policy': 'same-origin',
  'Origin-Agent-Cluster': '?1',
};

const commonSecurityHeaders: Record<string, string> = {
  'X-Content-Type-Options': 'nosniff',
  'Referrer-Policy': 'no-referrer',
  'Permissions-Policy': 'camera=(), microphone=(), geolocation=()',
};

const previewOnlyHeaders: Record<string, string> = {
  // CSP is enabled for `vite preview` so production builds can be validated under
  // the same secure-by-default policy used in deployment templates.
  //
  // Note: do not set a strict CSP on the dev server; it can interfere with HMR.
  'Content-Security-Policy':
    "default-src 'none'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; connect-src 'self' https://aero-gateway.invalid wss://aero-gateway.invalid; img-src 'self' data: blob:; style-src 'self'; font-src 'self'",
};

export default defineConfig({
  server: {
    headers: { ...crossOriginIsolationHeaders, ...commonSecurityHeaders },
  },
  preview: {
    headers: { ...crossOriginIsolationHeaders, ...commonSecurityHeaders, ...previewOnlyHeaders },
  },
});
