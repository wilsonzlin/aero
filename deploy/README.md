# Aero deployment (TLS + COOP/COEP at the edge)

This directory contains **production** and **local-dev** deployment artifacts that:

1) Terminate TLS (HTTPS/WSS) at the edge
2) Enforce **cross-origin isolation** headers (COOP/COEP/CORP) required for:
   - `SharedArrayBuffer` + WASM threads
   - some high-performance browser execution patterns
3) Set additional hardening headers (CSP, Referrer-Policy, Permissions-Policy, etc.)
4) Reverse-proxy backend HTTP APIs and WebSocket upgrades (e.g. `/tcp`) to the Aero gateway

The recommended topology is **single-origin**:

```
Browser  ──HTTPS/WSS──▶  Caddy (edge)  ──HTTP/WS──▶  aero-gateway
                  same-origin for UI + APIs (no CORS needed)
```

## Files

- `deploy/docker-compose.yml` – runs:
  - `aero-proxy` (Caddy) on `:80/:443`
  - `aero-gateway` (`backend/aero-gateway`) on the internal docker network
- `deploy/caddy/Caddyfile` – TLS termination, COOP/COEP headers, reverse proxy rules
- `deploy/scripts/smoke.sh` – builds + boots the compose stack and asserts key headers
- `deploy/static/index.html` – a small **smoke test page** to validate `window.crossOriginIsolated`
- `deploy/k8s/` – Kubernetes/Helm deployment for `aero-gateway` with Ingress TLS + COOP/COEP/CSP headers

For CSP details and tradeoffs (including why Aero needs `'wasm-unsafe-eval'` for WASM-based JIT),
see: `docs/security-headers.md`.

## Production-ready vs examples

This directory intentionally includes both **copy/paste-ready** configs and **reference-only**
templates.

Production-ready building blocks:

- `deploy/docker-compose.yml` + `deploy/caddy/Caddyfile` – single-host deployments (VM/bare metal)
- `deploy/k8s/chart/aero-gateway/` – Kubernetes Helm chart for the gateway + Ingress headers

Examples / reference-only:

- `deploy/static/` – smoke-test frontend (not the real UI)
- `deploy/nginx/` – nginx examples (useful if you don't want Caddy)
- `deploy/k8s/aero-storage-server/` – optional disk/image service templates (not required for the gateway)
- Static-host templates (production app living under `web/`):
  - `_headers` (Cloudflare Pages / Netlify-style):
    - `web/public/_headers` (copied to `web/dist/_headers` on build)
    - `deploy/cloudflare-pages/_headers` (copy/paste template variant)
  - Netlify (`netlify.toml`):
    - `netlify.toml` (repo root; build config + header rules)
    - `deploy/netlify.toml` (headers-only template)
  - Vercel (`vercel.json`):
    - `vercel.json` (repo root; build config + header rules)
    - `deploy/vercel.json` (headers-only template)

The header values are centralized in `scripts/headers.json` (exported via `scripts/security_headers.mjs`).
CI enforces consistency via `scripts/ci/check-security-headers.mjs`.

## CI validation (Terraform + Helm)

CI validates the deployment artifacts under:

- `infra/` (Terraform formatting + validation)
- `deploy/k8s/` (Helm lint + template rendering + Kubernetes schema validation)
- top-level deployment manifests (basic hygiene labelling; see `scripts/ci/check-deploy-manifests.mjs`)

Reproduce locally:

```bash
# Terraform (requires `terraform`; CI also runs `tflint`)
cd infra/aws-s3-cloudfront-range
terraform fmt -check -recursive
terraform init -backend=false -input=false
terraform validate

# Optional: extra linting (requires `tflint`)
tflint --init
tflint

# Helm/Kubernetes (requires `helm` + `kubeconform`)
CHART=deploy/k8s/chart/aero-gateway
helm lint "$CHART" -f "$CHART/values-dev.yaml"
helm lint "$CHART" -f "$CHART/values-prod.yaml"
```

## Production DNS requirements

To use public, browser-trusted certificates (Let’s Encrypt via Caddy), you need:

- An **A** record pointing your domain at your server IPv4
- (Optional) An **AAAA** record for IPv6
- Ports **80/tcp** and **443/tcp** reachable from the public internet

Example:

| Type | Name | Value |
|------|------|-------|
| A | `aero.example.com` | `203.0.113.10` |
| AAAA | `aero.example.com` | `2001:db8::10` |

## Environment variables

Set these in your shell or a `.env` file next to `deploy/docker-compose.yml`.

Quick start:

```bash
cp deploy/.env.example deploy/.env
```

- `AERO_DOMAIN` (default: `localhost`)
  - `localhost` for local dev
  - `aero.example.com` (or similar) for production
- `AERO_GATEWAY_IMAGE` (default: `aero-gateway:dev`)
  - By default, `deploy/docker-compose.yml` builds `backend/aero-gateway` from source.
  - For production, prefer a published image and remove the compose `build:` stanza (or override it).
- `AERO_GATEWAY_UPSTREAM` (default: `aero-gateway:8080`)
  - Only change if your gateway listens on a different port inside docker.
- `AERO_HSTS_MAX_AGE` (default: `0`)
  - `0` disables HSTS (good for local dev)
  - Recommended production value: `31536000` (1 year)
- `AERO_FRONTEND_ROOT` (default: `./static`)
  - Which directory Caddy serves as `/` (mounted at `/srv` in the container).
  - Recommended: `../web/dist` (after building the real frontend)
- `AERO_CSP_CONNECT_SRC_EXTRA` (default: empty)
  - Optional additional origins to allow in the Caddy Content Security Policy `connect-src`.
  - Use this if the frontend needs to connect to a separate origin for networking (e.g. a TCP proxy service).
  - Example: `AERO_CSP_CONNECT_SRC_EXTRA="https://proxy.example.com wss://proxy.example.com"`

Gateway environment variables (used by `backend/aero-gateway` and passed through in
`deploy/docker-compose.yml`):

- `PUBLIC_BASE_URL` (default in compose: `https://${AERO_DOMAIN}`)
  - Used to derive the default `ALLOWED_ORIGINS` allowlist.
- `ALLOWED_ORIGINS` (optional, comma-separated)
  - Set explicitly if you need to allow additional origins (e.g. a dev server).
- `TRUST_PROXY` (default in compose: `1`)
  - Set to `1` only when the gateway is reachable **only** via the reverse proxy.
  - Required if you want `request.ip` / rate limiting to use `X-Forwarded-For`.
- `CROSS_ORIGIN_ISOLATION` (default in compose: `0`)
  - Set to `1` only if you are not injecting COOP/COEP headers at the edge proxy.

### Using the real gateway in production

`deploy/docker-compose.yml` builds `backend/aero-gateway` from source so `docker compose up`
works without needing a published image.

For production deployments, you will typically:

1) Remove the `build:` stanza from the `aero-gateway` service (or override via a separate compose file)
2) Set `image:` to a published gateway image
3) Keep the proxy as-is (it is production-ready)

The edge proxy (Caddy) automatically sets standard forwarding headers like:
`X-Forwarded-For`, `X-Forwarded-Proto`, and `X-Forwarded-Host`.

## Local dev (self-signed TLS)

Run:

```bash
docker compose -f deploy/docker-compose.yml up
```

Then open:

- `https://localhost/`

You should see the smoke test page.

> Note: Caddy serves HTTPS with HTTP/2 enabled automatically when using TLS.

### Trusting the certificate (recommended)

For `localhost`, Caddy uses an internal CA. Browsers may require you to trust
that CA for the origin to be treated as fully secure.

To export the Caddy local root CA:

```bash
docker compose -f deploy/docker-compose.yml cp aero-proxy:/data/caddy/pki/authorities/local/root.crt ./caddy-local-root.crt
```

Then import `./caddy-local-root.crt` into your OS/browser trust store.

## Verifying cross-origin isolation

### SharedArrayBuffer enablement checklist

To reliably get `SharedArrayBuffer` + WASM threads working in production:

- [ ] The page is served from a **secure context** (`https://` in production)
- [ ] The main document response includes **COOP + COEP**:
  - [ ] `Cross-Origin-Opener-Policy: same-origin`
  - [ ] `Cross-Origin-Embedder-Policy: require-corp`
- [ ] Recommended additional hardening headers are present:
  - [ ] `Cross-Origin-Resource-Policy: same-origin`
  - [ ] `Origin-Agent-Cluster: ?1`
- [ ] All subresources (scripts/wasm/workers) are **same-origin**, or explicitly
      CORS/CORP-enabled
- [ ] No mixed content (no `http://` subresources on an `https://` page)

### 1) Check the headers

```bash
# If you haven't trusted the local Caddy CA yet, add `-k` (insecure) or trust the
# CA as described below.
curl -kI https://localhost/
```

Expect:

- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp`
- `Cross-Origin-Resource-Policy: same-origin`
- `Origin-Agent-Cluster: ?1`

### 2) Check in the browser

Open DevTools Console:

```js
window.crossOriginIsolated === true
```

Also check:

```js
typeof SharedArrayBuffer !== "undefined"
```

If `crossOriginIsolated` is `false`, the most common causes are:

- Missing COOP/COEP headers on the **HTML document** response
- One or more subresources (scripts/wasm/workers) being loaded cross-origin
  without proper `CORP` or CORS headers
- TLS is not considered secure (certificate not trusted, mixed content, etc.)

## WebSocket proxy validation (WSS)

The edge proxy is configured to forward WebSocket upgrades for `/tcp`.

You can validate that the TLS + upgrade path works with a CLI client like
[`wscat`](https://github.com/websockets/wscat) or [`websocat`](https://github.com/vi/websocat):

```bash
# If you haven't trusted the local Caddy CA yet, you may need:
#   NODE_TLS_REJECT_UNAUTHORIZED=0
NODE_TLS_REJECT_UNAUTHORIZED=0 npx wscat -c "wss://localhost/tcp?v=1&target=example.com:80"
```

If you see a successful handshake but the connection immediately closes, the
gateway may be rejecting the query parameters or target.

## CORS / origin strategy

### Recommended (no CORS): same-origin UI + gateway

Serve the frontend and gateway through the same `https://AERO_DOMAIN` origin.
This is what the provided `Caddyfile` + compose setup enables.

Benefits:

- No CORS configuration required
- Simplest path to `crossOriginIsolated`
- WSS and WebRTC requirements are met by default (secure context)

### Dev server (Vite) caveat

If you run a dev server like `http://localhost:5173`, you are **changing the
origin**, which introduces CORS requirements and can break cross-origin
isolation unless the dev server also sets COOP/COEP headers.

At minimum, your dev server must:

- Serve over a secure context (prefer `https://`)
- Send the same COOP/COEP/CORP headers on the HTML + JS/worker responses

For Vite, this is typically done by setting `server.headers` and enabling HTTPS.
This repo’s Vite app (`web/`) already includes these headers in `web/vite.config.ts`.

If you need to call the gateway from a different origin (e.g. Vite dev server),
your gateway must also be configured with an explicit CORS allowlist (for
example, allowing `https://localhost:5173`). Prefer a strict allowlist over
`*`, especially once credentials or session tokens are involved.

### Recommended dev workflow: keep a single origin

If you want hot-reload but still want **same-origin** + COOP/COEP enforcement,
run the Vite dev server separately and have Caddy proxy non-API routes to it.
That way the browser still sees a single `https://AERO_DOMAIN` origin.

## Serving your real frontend build

By default, `deploy/docker-compose.yml` mounts `deploy/static/` into the proxy
at `/srv` as a smoke test.

`deploy/caddy/Caddyfile` is tuned for Vite output:

- `/assets/*` gets long-lived caching (`Cache-Control: public, max-age=31536000, immutable`)
- everything else (including `index.html`) is served with `Cache-Control: no-cache`

To serve your real frontend:

1) Build it (example):

```bash
npm ci
npm -w web run build
```

2) Replace the volume mount in `deploy/docker-compose.yml`:

Set `AERO_FRONTEND_ROOT` (recommended; no compose edits required):

```bash
# in deploy/.env (or export it in your shell)
AERO_FRONTEND_ROOT=../web/dist
```

3) Restart:

```bash
docker compose -f deploy/docker-compose.yml up --force-recreate
```

## Separate static hosting (frontend on a different origin)

The simplest/most robust setup is **single-origin** (serve static UI + gateway under the same host).
If you must host the frontend elsewhere (Netlify/Vercel/Cloudflare Pages), you must configure **both**:

1) **Frontend headers** (COOP/COEP + CSP)
2) **Gateway origin allowlist** (`PUBLIC_BASE_URL` / `ALLOWED_ORIGINS`)

Hosting templates in this repo:

- Netlify + Cloudflare Pages headers: `web/public/_headers` (copied into `web/dist/_headers` on build)
- Netlify build config: `netlify.toml` (repo root)
- Vercel config: `vercel.json` (repo root)

When using a separate gateway origin, update the frontend CSP `connect-src` to include the gateway:

```
connect-src 'self' https://gateway.example.com wss://gateway.example.com
```

Then configure the gateway to allow the frontend origin:

- `PUBLIC_BASE_URL=https://gateway.example.com`
- `ALLOWED_ORIGINS=https://frontend.example.com` (comma-separated if multiple)

## Compose smoke check

To validate the compose stack end-to-end (build + headers + `/healthz` + wasm MIME/caching), run:

```bash
bash deploy/scripts/smoke.sh
```
