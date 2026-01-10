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
  - `aero-gateway` (your backend container) on the internal docker network
- `deploy/caddy/Caddyfile` – TLS termination, COOP/COEP headers, reverse proxy rules
- `deploy/static/index.html` – a small **smoke test page** to validate `window.crossOriginIsolated`
- `deploy/k8s/` – Kubernetes/Helm deployment for `aero-gateway` with Ingress TLS + COOP/COEP headers

For CSP details and tradeoffs (including why Aero needs `'wasm-unsafe-eval'` for WASM-based JIT),
see: `docs/security-headers.md`.

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

Set these in your shell or a `.env` file next to `deploy/docker-compose.yml`:

- `AERO_DOMAIN` (default: `localhost`)
  - `localhost` for local dev
  - `aero.example.com` (or similar) for production
- `AERO_GATEWAY_IMAGE` (default: `aero-gateway:dev`)
  - This compose file builds a small **stub gateway** by default (see `deploy/gateway/`).
  - For production, replace the `aero-gateway` service with your real gateway image.
- `AERO_GATEWAY_UPSTREAM` (default: `aero-gateway:8080`)
  - Only change if your gateway listens on a different port inside docker.
- `AERO_HSTS_MAX_AGE` (default: `0`)
  - `0` disables HSTS (good for local dev)
  - Recommended production value: `31536000` (1 year)

Gateway environment variables (used by `backend/aero-gateway` and passed through in
`deploy/docker-compose.yml`):

- `PUBLIC_BASE_URL` (default in compose: `https://${AERO_DOMAIN}`)
  - Used to derive the default `ALLOWED_ORIGINS` allowlist.
- `ALLOWED_ORIGINS` (optional, comma-separated)
  - Set explicitly if you need to allow additional origins (e.g. a dev server).
- `TRUST_PROXY` (default in compose: `1`)
  - Set to `1` only when the gateway is reachable **only** via the reverse proxy.
  - Required if you want `request.ip` / rate limiting to use `X-Forwarded-For`.
- `CROSS_ORIGIN_ISOLATION` (default in compose: `1`)
  - Optional defense-in-depth; the proxy already sets these headers at the edge.

### Using the real gateway in production

`deploy/docker-compose.yml` includes a `build: ./gateway` section to make
`docker compose up` work out-of-the-box in local dev.

For production deployments, you will typically:

1) Remove the `build:` stanza from the `aero-gateway` service
2) Set `image:` to your real published gateway image
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
curl -I https://localhost/
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
# With the deploy stub gateway (no query params required):
npx wscat -c "wss://localhost/tcp"

# With a real Aero gateway, adjust query params to match its /tcp contract.
# Canonical (v1):
#   npx wscat -c "wss://localhost/tcp?v=1&host=example.com&port=80"
#
# Compatibility form (also supported):
#   npx wscat -c "wss://localhost/tcp?v=1&target=example.com:80"
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

To serve your real frontend:

1) Build it (example):

```bash
cd web
npm ci
npm run build
```

2) Replace the volume mount in `deploy/docker-compose.yml`:

```yaml
    volumes:
      # - ./static:/srv:ro
      - ../web/dist:/srv:ro
```

3) Restart:

```bash
docker compose -f deploy/docker-compose.yml up --force-recreate
```
