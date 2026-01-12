# Deployment & Hosting (COOP/COEP / cross-origin isolation)

Aero performs best with **WebAssembly threads / shared memory** (SharedArrayBuffer + Atomics).
Browsers only enable these capabilities in a **cross-origin isolated** context (see
[ADR 0002](./adr/0002-cross-origin-isolation.md)).

This repo also ships a **non-shared-memory fallback** WASM build (see
[ADR 0004](./adr/0004-wasm-build-variants.md)). When COOP/COEP headers are missing,
the web runtime will automatically load the single-threaded/non-shared variant so
the app can still start (degraded functionality/performance is expected).

That means your deployment **must** send these headers on the top-level document
and all subresources (JS, WASM, worker scripts, etc.):

- `Cross-Origin-Opener-Policy: same-origin` (COOP)
- `Cross-Origin-Embedder-Policy: require-corp` (COEP)
- `Cross-Origin-Resource-Policy: same-origin` (CORP, recommended hardening)

This repository includes production-ready header templates for common hosts.
For the full recommended hardening set (including CSP with `wasm-unsafe-eval` for Aero’s WASM-based JIT),
see [`docs/security-headers.md`](./security-headers.md).

Recommended additional hardening headers (included in the templates):

- `Cross-Origin-Resource-Policy: same-origin`
- `Origin-Agent-Cluster: ?1`

---

## Why COOP/COEP is required

When COOP/COEP are missing, browsers will report:

- `crossOriginIsolated === false`
- `SharedArrayBuffer` may be unavailable
- WASM `shared: true` memories / thread pools will fail to initialize (the threaded build cannot load)

In practice, the runtime loader will fall back to the non-shared-memory build.
This is primarily intended as a compatibility path; Windows 7 workloads will be
unacceptably slow without threads.

---

## Local verification (preview server)

The Vite preview server is configured to send COOP/COEP (and the rest of the recommended hardening set).

Canonical header values live in:

- `scripts/headers.json`
- `scripts/security_headers.mjs` (exports used by Vite config)

CI validates that Vite servers + deployment templates stay in sync via:

- `scripts/ci/check-security-headers.mjs`

Currently validated files:

- `vite.harness.config.ts`
- `web/vite.config.ts` (legacy/experimental Vite app)
- `backend/aero-gateway/src/middleware/crossOriginIsolation.ts`
- `backend/aero-gateway/src/middleware/securityHeaders.ts`
- `deploy/k8s/chart/aero-gateway/values.yaml` (Helm chart defaults)
- `server/src/http.js` (legacy backend)
- `web/public/_headers`
- `deploy/cloudflare-pages/_headers`
- `netlify.toml`
- `deploy/netlify.toml`
- `vercel.json`
- `deploy/vercel.json`
- `deploy/caddy/Caddyfile`
- `deploy/nginx/nginx.conf`

```bash
npm ci
npm run build
npm run preview
```

Then open the printed URL (usually `http://localhost:4173`) and verify:

- Open DevTools Console and run: `crossOriginIsolated` → should be `true`
- Optionally also check: `typeof SharedArrayBuffer !== 'undefined'`

> Note: `http://localhost` is treated as a secure context by browsers, so COOP/COEP works
> in local preview mode. In production you must serve over **HTTPS** (non-localhost
> `http://` will not be cross-origin isolated and will not get SharedArrayBuffer).

If `crossOriginIsolated` is `false`, inspect the **Network** tab and confirm the
main document response includes the required headers.

This repo also includes a Playwright integration test that asserts headers are
present on HTML + JS + worker + WASM responses:

```bash
npm run test:security-headers
```

### Testing the fallback path (no COOP/COEP)

The repo-root Vite dev server can be started with COOP/COEP disabled:

```bash
VITE_DISABLE_COOP_COEP=1 npm run dev
```

In this mode `crossOriginIsolated` should be `false` and the runtime will load the
single-threaded/non-shared WASM variant.

---

## Production hosting templates

### Netlify

This repo provides:

- `netlify.toml` (build/publish settings for the repo-root app + header rules)
- `web/public/_headers` (COOP/COEP + CSP + baseline security headers + caching defaults)

Netlify will apply `dist/_headers` automatically (Vite copies `public/` → `dist/`).

### Vercel

This repo provides `vercel.json` (repo root), which:

- builds the repo-root frontend and deploys `dist`
- applies COOP/COEP + CSP + baseline security headers to all paths
- applies safe caching defaults (`no-cache` for HTML; immutable caching for `/assets/*`)

### Cloudflare Pages

Cloudflare Pages supports the same `_headers` file format.

Configure the project with:

Recommended (npm workspaces / monorepo):

- Root directory: `.`
- Build command: `npm ci && npm run build:prod`
- Build output directory: `dist`

The generated `dist/_headers` file is deployed automatically and enables cross-origin isolation and baseline security headers.

> Note: Some platforms apply only the *most specific* matching `_headers` rule.
> The provided `_headers` file repeats COOP/COEP/CORP in the `/assets/*` and
> `/assets/*.wasm` rules so that cached/static assets remain cross-origin isolated
> even under “most specific wins” behavior.

### Caddy (self-host / reverse proxy)

This repo provides a production-ready Caddy reverse proxy template:

- `deploy/caddy/Caddyfile`
- (end-to-end example) `deploy/docker-compose.yml`

### nginx (self-host / reverse proxy)

This repo provides an nginx reference config:

- `deploy/nginx/nginx.conf`

---

## CI validation (IaC + Kubernetes)

This repository validates deployment artifacts in CI (Terraform + Helm/Kubernetes schema).

- Workflow: `.github/workflows/iac.yml`
- Local reproduction:

```bash
./scripts/ci/check-iac.sh
# or:
#   just check-iac
```

For details and copy/paste commands, see:

- `deploy/README.md` (compose + edge proxy)
- `deploy/k8s/README.md` (Helm chart)

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

## Kubernetes deployments

If you are deploying the Aero backend gateway on Kubernetes, see:

- `deploy/k8s/README.md`

It includes:

- a Helm chart for `aero-gateway`
- Ingress examples for TLS termination + WebSocket upgrades
- options to inject COOP/COEP headers at the Ingress (nginx/Traefik) or at the application layer

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

---

## Browser storage quota & persistence (OPFS durability)

This project stores large disk images in the browser (OPFS / `navigator.storage.getDirectory()`).
Browsers are allowed to evict site storage under pressure unless the origin has been granted
**persistent storage** (`navigator.storage.persist()`).

### Storage quota reporting

The Disk Images panel shows:

- total estimated usage
- total estimated quota
- percent used

When usage exceeds ~80%, the UI warns that imports may fail.

### Requesting persistent storage

The Disk Images panel includes a **Request persistent storage** button.

- If supported and granted, the UI shows `Persistent storage: granted`.
- If denied, the UI shows `Persistent storage: not granted`.
- If unsupported, the UI shows `Persistent storage: unsupported`.

### Manual test instructions

1. Start the web UI:
   - `npm ci`
   - `npm -w web run dev`
2. Open the Disk Images panel and verify quota numbers render (Chrome/Edge/Firefox support `navigator.storage.estimate()`).
3. Click **Request persistent storage**:
   - Chrome/Edge: often grants automatically for installed PWAs or high-engagement sites.
   - Firefox: may grant or deny depending on settings.
   - Safari: typically does not support the persistence APIs (expect `unsupported`).
4. Attempt importing a large file when storage is nearly full:
   - The import flow checks estimated remaining space and prompts for confirmation if it appears low.
