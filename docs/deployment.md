# Deployment & Hosting (COOP/COEP / cross-origin isolation)

Aero requires **WebAssembly threads** (SharedArrayBuffer + Atomics) for performance.
Browsers only enable these capabilities in a **cross-origin isolated** context.

That means your deployment **must** send these headers on the top-level document
and all subresources (JS, WASM, worker scripts, etc.):

- `Cross-Origin-Opener-Policy: same-origin` (COOP)
- `Cross-Origin-Embedder-Policy: require-corp` (COEP)

This repository includes production-ready header templates for common hosts.

---

## Why COOP/COEP is required

When COOP/COEP are missing, browsers will report:

- `crossOriginIsolated === false`
- `SharedArrayBuffer` may be unavailable
- WASM `shared: true` memories / thread pools will fail to initialize

In practice, the emulator will either fail at startup or silently fall back to
single-threaded execution (unacceptably slow for Windows 7 workloads).

---

## Local verification (preview server)

The Vite preview server is configured to send COOP/COEP headers.

```bash
cd web
npm install
npm run build
npm run preview
```

Then open the printed URL (usually `http://localhost:4173`) and verify:

- Open DevTools Console and run: `crossOriginIsolated` → should be `true`
- Optionally also check: `typeof SharedArrayBuffer !== 'undefined'`

If `crossOriginIsolated` is `false`, inspect the **Network** tab and confirm the
main document response includes the required headers.

---

## Production hosting templates

### Netlify

This repo provides:

- `netlify.toml` (build/publish settings for the `web/` subproject)
- `web/public/_headers` (COOP/COEP + caching defaults)

Netlify will apply `dist/_headers` automatically (Vite copies `public/` → `dist/`).

### Cloudflare Pages

Cloudflare Pages supports the same `_headers` file format.

Configure the project with:

- Build command: `npm run build`
- Build output directory: `web/dist`

The generated `web/dist/_headers` file is deployed automatically and enables
cross-origin isolation.

---

## Caching defaults (safe + update-friendly)

The provided `_headers` rules do the following:

- **HTML / routes**: `Cache-Control: no-cache` (so updates roll out quickly)
- **Hashed static assets** (`/assets/*`): `Cache-Control: public, max-age=31536000, immutable`

If you change Vite’s `assetsDir` or add non-hashed critical files, review and
adjust caching rules accordingly.

---

## Why GitHub Pages is not suitable

GitHub Pages does **not** allow custom response headers for static sites.
Because COOP/COEP headers are mandatory for SharedArrayBuffer/WASM threads, a
GitHub Pages deployment cannot reliably run Aero.

If you must use GitHub infrastructure, put a header-capable CDN/reverse-proxy
in front (e.g. Cloudflare) — but using a host with native header support
(Netlify/Cloudflare Pages/Vercel/etc.) is simpler.

---

## Troubleshooting / gotchas

1. **Avoid third-party CDNs for JS/WASM/worker scripts**
   - With `COEP: require-corp`, cross-origin subresources must be served with
     CORS or `Cross-Origin-Resource-Policy` headers. The simplest path is to
     serve everything from the same origin.
2. **WASM content-type**
   - Some static hosts serve `.wasm` with an incorrect `Content-Type`, which can
     break `WebAssembly.compileStreaming(...)`. The templates include
     `Content-Type: application/wasm` for `*.wasm` assets.
