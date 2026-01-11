# Security headers (CSP, COOP/COEP, Permissions-Policy)

Aero needs a stricter-than-usual set of browser capabilities:

- **WebAssembly threads / `SharedArrayBuffer`** → requires **cross-origin isolation** (COOP/COEP).  
  See [ADR 0002](./adr/0002-cross-origin-isolation.md) and [ADR 0004](./adr/0004-wasm-build-variants.md) for how this ties into threaded vs single-threaded builds.
- **Dynamic WebAssembly compilation** for JIT blocks (e.g. `WebAssembly.compile`, `WebAssembly.instantiate`, `WebAssembly.compileStreaming`) → requires CSP **`'wasm-unsafe-eval'`**.
- **Web Workers / module workers** (and potentially bundler-generated `blob:` workers) → requires CSP **`worker-src 'self' blob:`**.
- **WebGPU** + **OPFS** do not require CSP directives, but CSP should avoid accidentally blocking the resources needed to start the app (scripts, workers, WASM fetches, WebSocket proxy).

This document defines a **secure-by-default** header set and provides templates for common hosting providers.

---

## Recommended header set

### Cross-origin isolation (required for threads)

These are required for `SharedArrayBuffer` (and therefore `wasm32-threads` / parallel workers) in modern browsers:

- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp`
- `Cross-Origin-Resource-Policy: same-origin`
- `Origin-Agent-Cluster: ?1` (recommended hardening; keeps the origin in an origin-keyed agent cluster)

**Tradeoff:** COEP will block embedding cross-origin resources unless they send `Cross-Origin-Resource-Policy` / CORS headers. Keep all JS/WASM/assets same-origin where possible.

### Content Security Policy (CSP)

Recommended CSP (single line):

```
default-src 'none'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; connect-src 'self' https://aero-gateway.invalid wss://aero-gateway.invalid; img-src 'self' data: blob:; style-src 'self'; font-src 'self'
```

Directive rationale:

- `default-src 'none'`: deny by default; explicitly allow what Aero needs.
- `script-src 'self' 'wasm-unsafe-eval'`: allow ESM from same-origin **and** dynamic WASM compilation for JIT **without** enabling JS `eval`.
- `worker-src 'self' blob:`: allow module workers from same-origin; allow `blob:` workers for bundlers/worklets that generate worker code at runtime.
- `connect-src 'self' …`: allow `fetch()` / `WebAssembly.compileStreaming()` from same-origin and optionally a WebSocket proxy origin.
  - `https://aero-gateway.invalid` and `wss://aero-gateway.invalid` are **documentation-only placeholders** (the `.invalid` TLD will never resolve). Replace with your real gateway/proxy origin or remove them entirely.
- `img-src 'self' data: blob:`: allow icons and generated object URLs.
- `style-src 'self'`: allow same-origin CSS without allowing inline script execution.
  - If you must use inline styles (e.g. CSS-in-JS), consider `'unsafe-inline'` here **only** (avoid it in `script-src`).
- `base-uri 'none'`, `object-src 'none'`, `frame-ancestors 'none'`: reduce common injection and clickjacking risks.

#### No inline scripts/styles by default

This CSP intentionally does **not** include `'unsafe-inline'`. That means:

- `<script>…</script>` (inline) will be blocked
- `<style>…</style>` and `style="…"` (inline) will be blocked

Prefer external JS/CSS files loaded from `'self'`. If you absolutely must use inline code, use **nonces/hashes** rather than `'unsafe-inline'` (note that many static host header configs cannot inject per-request nonces).

#### Why `wasm-unsafe-eval` is needed

Aero’s JIT design compiles new WASM modules at runtime (e.g. for hot x86 blocks). Under CSP, WASM compilation is controlled by the `script-src` directive:

- Without `script-src 'wasm-unsafe-eval'`, browsers may block:
  - `WebAssembly.compile(...)`
  - `WebAssembly.instantiate(...)` when given raw bytes
  - Streaming variants that compile from a response

`'wasm-unsafe-eval'` is preferred over `'unsafe-eval'` because it enables WASM compilation while still blocking classic JavaScript eval sinks like `eval()` and `new Function()`.

#### Browser support notes

`'wasm-unsafe-eval'` is the modern, least-bad way to permit dynamic WASM compilation. If a target browser does not recognize it, you have two options:

- **Disable runtime compilation** (ship precompiled modules only; no JIT tier), or
- As a last resort, add **`'unsafe-eval'`** (significantly weaker; enables JS `eval`/`new Function`).

#### Risk tradeoffs of `wasm-unsafe-eval`

Enabling dynamic WASM compilation:

- **Expands the set of executable code sources** (code can be created from bytes at runtime).
- Makes certain classes of “code as data” bugs more dangerous (e.g. if untrusted input can reach a WASM compiler path).

However:

- It is **much narrower** than `'unsafe-eval'` (no JS `eval`).
- WASM still executes inside the browser sandbox; it does not grant native code execution.

If you can avoid runtime compilation (e.g. ship precompiled WASM modules only), you can remove `'wasm-unsafe-eval'` for an even tighter policy, but Aero’s JIT tier may not work.

### Tightening `connect-src` (recommended)

Keep `connect-src` as narrow as possible because it governs:

- `fetch()` and `WebAssembly.compileStreaming()` network loads
- `WebSocket` / `WebRTC` signaling (if used)

Recommendations:

- If the proxy can be hosted **same-origin** (e.g. behind the same domain), use:
  - `connect-src 'self'`
- If the proxy is on a separate origin, add the **exact** origin(s) only:
  - `connect-src 'self' https://proxy.example.com wss://proxy.example.com`

### Other security headers

Recommended baseline:

- `Referrer-Policy: no-referrer` (privacy-first; alternatively `strict-origin-when-cross-origin`)
- `X-Content-Type-Options: nosniff`
- `Permissions-Policy: camera=(), geolocation=(), microphone=(self)`

Notes:

- If you do not need microphone capture, you can disable it with `microphone=()`. Aero’s web UI uses the microphone only when the user explicitly enables it.

---

## Hosting templates

These templates apply headers to **all paths** so they cover HTML, JS, WASM, and worker scripts.

The canonical header values live in:

- `scripts/headers.json` (data)
- `scripts/security_headers.mjs` (exports for Vite/config/templates)

CI validates that the following stay in sync with the canonical values:

- Vite configs:
  - `web/vite.config.ts` (production app)
  - `vite.harness.config.ts` (repo-root dev harness)
- Deployment templates:
  - Static hosts (`_headers`):
    - `web/public/_headers`
    - `deploy/cloudflare-pages/_headers`
  - Netlify (`netlify.toml`):
    - `netlify.toml` (repo root)
    - `deploy/netlify.toml` (headers-only template)
  - Vercel (`vercel.json`):
    - `vercel.json` (repo root)
    - `deploy/vercel.json` (headers-only template)
  - Kubernetes (Helm chart defaults):
    - `deploy/k8s/chart/aero-gateway/values.yaml`
  - `deploy/caddy/Caddyfile`
  - `deploy/nginx/nginx.conf`

The check lives at:

- `scripts/ci/check-security-headers.mjs`

### Static hosts that support `_headers` (Netlify, Cloudflare Pages)

The `_headers` file shipped with the built frontend lives at:

- `web/public/_headers` (copied to `web/dist/_headers` by Vite)

Vite copies `public/` into `dist/`, so production builds automatically contain:

- `web/dist/_headers`

Netlify and Cloudflare Pages will apply this file automatically when it exists at the root of the deployed output directory.

Netlify build settings and header rules are in `netlify.toml` (repo root). CI validates that its header values match `scripts/headers.json`.

### Vercel (`vercel.json`)

See: `vercel.json` (repo root).

### Reverse proxy templates (self-host)

- Caddy: `deploy/caddy/Caddyfile`
- nginx: `deploy/nginx/nginx.conf`

The Caddy template supports `AERO_CSP_CONNECT_SRC_EXTRA` to append additional allowed `connect-src` origins without editing the CSP string directly.

---

## Verification checklist

In a production build, open DevTools → Console and ensure:

- No CSP violations on startup.
- WASM loads and initializes normally.
- A synthetic dynamic compilation works (example):

```js
await WebAssembly.compile(new Uint8Array([0x00,0x61,0x73,0x6d,0x01,0x00,0x00,0x00]));
```

If you see errors like `Refused to compile or instantiate WebAssembly module because 'wasm-unsafe-eval' is not an allowed source`, your deployed CSP is missing `'wasm-unsafe-eval'` in `script-src`.

### Repo-local CSP/COOP/COEP regression tests

This repo includes an automated browser PoC and Playwright coverage to prevent regressions:

- CSP/COOP/COEP PoC app: `web/public/wasm-jit-csp/`
- CSP test server (sets COOP/COEP + CSP variants): `server/poc-server.mjs`
- Playwright spec: `tests/e2e/csp-fallback.spec.ts`
- Playwright spec (preview server headers): `tests/e2e/security-headers.spec.ts`

Run locally:

```bash
node server/poc-server.mjs --port 4180
# then open:
#  - http://127.0.0.1:4180/csp/strict/
#  - http://127.0.0.1:4180/csp/wasm-unsafe-eval/
```

Or run the automated check:

```bash
npx playwright test tests/e2e/csp-fallback.spec.ts
```

And validate the canonical preview server header set:

```bash
npm run test:security-headers
```

The host-side capability bit that gates Tier-1/2 WASM JIT is exposed as:

- `jit_dynamic_wasm` in `src/platform/features.ts` and `web/src/platform/features.ts`
