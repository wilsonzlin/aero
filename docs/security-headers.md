# Security headers (CSP, COOP/COEP, Permissions-Policy)

Aero needs a stricter-than-usual set of browser capabilities:

- **WebAssembly threads / `SharedArrayBuffer`** → requires **cross-origin isolation** (COOP/COEP).
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

**Tradeoff:** COEP will block embedding cross-origin resources unless they send `Cross-Origin-Resource-Policy` / CORS headers. Keep all JS/WASM/assets same-origin where possible.

### Content Security Policy (CSP)

Recommended CSP (single line):

```
default-src 'none'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; connect-src 'self' https://aero-proxy.invalid wss://aero-proxy.invalid; img-src 'self' data: blob:; style-src 'self'; font-src 'self'
```

Directive rationale:

- `default-src 'none'`: deny by default; explicitly allow what Aero needs.
- `script-src 'self' 'wasm-unsafe-eval'`: allow ESM from same-origin **and** dynamic WASM compilation for JIT **without** enabling JS `eval`.
- `worker-src 'self' blob:`: allow module workers from same-origin; allow `blob:` workers for bundlers/worklets that generate worker code at runtime.
- `connect-src 'self' …`: allow `fetch()` / `WebAssembly.compileStreaming()` from same-origin and optionally a WebSocket proxy origin.
  - `https://aero-proxy.invalid` and `wss://aero-proxy.invalid` are **documentation-only placeholders** (the `.invalid` TLD will never resolve). Replace with your real proxy origin or remove them entirely.
- `img-src 'self' data: blob:`: allow icons and generated object URLs.
- `style-src 'self'`: allow same-origin CSS without allowing inline script execution.
  - If you must use inline styles (e.g. CSS-in-JS), consider `'unsafe-inline'` here **only** (avoid it in `script-src`).
- `base-uri 'none'`, `object-src 'none'`, `frame-ancestors 'none'`: reduce common injection and clickjacking risks.

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
- `Permissions-Policy: camera=(), microphone=(), geolocation=()`

Notes:

- Disable `microphone` by default. If/when the project adds live audio input, relax the policy deliberately (e.g. `microphone=(self)`).

---

## Hosting templates

These templates apply headers to **all paths** so they cover HTML, JS, WASM, and worker scripts.

### Netlify (`netlify.toml`)

See: `deploy/netlify.toml`

### Vercel (`vercel.json`)

See: `deploy/vercel.json`

### Cloudflare Pages (`_headers`)

See: `deploy/cloudflare-pages/_headers`

Cloudflare Pages requires `_headers` to be present at the **root of the build output directory**. Depending on your build tool, you may need to copy it into `dist/` (or equivalent) as part of the build.

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
