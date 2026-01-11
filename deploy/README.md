# Aero deployment (TLS + COOP/COEP at the edge)

This directory contains **production** and **local-dev** deployment artifacts that:

1) Terminate TLS (HTTPS/WSS) at the edge
2) Enforce **cross-origin isolation** headers (COOP/COEP/CORP) required for:
   - `SharedArrayBuffer` + WASM threads
   - some high-performance browser execution patterns
3) Set additional hardening headers (CSP, Referrer-Policy, Permissions-Policy, etc.)
4) Reverse-proxy backend HTTP APIs and WebSocket upgrades (e.g. `/tcp`, `/tcp-mux`, `/l2`) to backend services
5) Reverse-proxy WebRTC signaling + ICE discovery endpoints (e.g. `/webrtc/ice`) to the UDP relay

The recommended topology is **single-origin**:

```
Browser  ──HTTPS/WSS──▶  Caddy (edge)  ──HTTP/WS──▶  aero-gateway
                     │                ──HTTP/WS──▶  aero-l2-proxy
                     │                ──HTTP/WS──▶  aero-webrtc-udp-relay
                     │
                     └──UDP (ICE + relay data)────▶  aero-webrtc-udp-relay (published UDP range)

Same-origin for UI + APIs (no CORS needed).
```

## Optional: UDP relay service (WebRTC + WebSocket fallback)

The gateway (`backend/aero-gateway`) covers **TCP** (WebSocket) and **DNS-over-HTTPS**. Guest **UDP** requires a separate relay service:

- [`proxy/webrtc-udp-relay`](../proxy/webrtc-udp-relay/) — WebRTC DataChannel (`label="udp"`) with a `GET /udp` WebSocket fallback, using the versioned v1/v2 datagram framing in [`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md).

To integrate the relay with the gateway (recommended for production):

1. Deploy the relay somewhere reachable by the browser.
2. Configure the gateway with `UDP_RELAY_BASE_URL` and a matching relay auth mode (`none`, `api_key`, or `jwt`).
3. The gateway’s `POST /session` response will include an `udpRelay` field (base URL + endpoints + short‑lived token), and clients can optionally refresh the token via `POST /udp-relay/token`.

## Files

- `deploy/docker-compose.yml` – runs:
  - `aero-proxy` (Caddy) on `:80/:443`
  - `aero-gateway` (`backend/aero-gateway`) on the internal docker network
  - `aero-l2-proxy` (`crates/aero-l2-proxy`) on the internal docker network
  - `aero-webrtc-udp-relay` (`proxy/webrtc-udp-relay`) for WebRTC UDP relay (HTTP behind Caddy, UDP published)
  - (optional) `coturn` TURN server via compose profile
- `deploy/caddy/Caddyfile` – TLS termination, COOP/COEP headers, reverse proxy rules
- `deploy/scripts/smoke.sh` – builds + boots the compose stack and asserts key headers
- `deploy/static/index.html` – a small **smoke test page** to validate `window.crossOriginIsolated` and basic networking wiring (`/session`, `/tcp`, `/l2`)
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
- Static-host templates (browser host app):
  - `_headers` (Cloudflare Pages / Netlify-style):
    - `web/public/_headers` (copied to `dist/_headers` on build)
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
- Compose deployment manifests (labels + `docker compose config` validation; see `scripts/ci/check-deploy-manifests.mjs`)

Reproduce locally:

```bash
# Full reproduction (requires: node + docker compose + terraform + tflint + helm + kubeconform)
./scripts/ci/check-iac.sh
# Or, if you have `just` installed:
#   just check-iac

# Deploy manifest labelling/hygiene (requires docker compose; fails on compose warnings)
node scripts/ci/check-deploy-manifests.mjs

# Terraform (requires `terraform`; CI also runs `tflint`)
cd infra/aws-s3-cloudfront-range
terraform fmt -check -recursive
terraform init -backend=false -input=false -lockfile=readonly
terraform validate

# Optional: extra linting (requires `tflint`)
tflint --init
tflint

# Helm/Kubernetes (requires `helm` + `kubeconform`)
CHART=deploy/k8s/chart/aero-gateway
for values in \
  values-dev.yaml \
  values-prod.yaml \
  values-traefik.yaml \
  values-prod-certmanager.yaml \
  values-prod-certmanager-issuer.yaml \
  values-prod-appheaders.yaml; do
  helm lint "$CHART" --strict --kube-version 1.28.0 -f "$CHART/$values"
done

for values in \
  values-dev.yaml \
  values-prod.yaml \
  values-traefik.yaml \
  values-prod-certmanager.yaml \
  values-prod-certmanager-issuer.yaml \
  values-prod-appheaders.yaml; do
  out="/tmp/aero-${values%.yaml}.yaml"
  helm template aero-gateway "$CHART" -n aero --kube-version 1.28.0 -f "$CHART/$values" > "$out"
  kubeconform -strict \
    -schema-location default \
    -schema-location "https://raw.githubusercontent.com/datreeio/CRDs-catalog/main/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json" \
    -kubernetes-version 1.28.0 -summary "$out"
done
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
# For production, set a strong SESSION_SECRET explicitly (recommended).
# If unset, `deploy/docker-compose.yml` generates and persists a random secret in a Docker volume
# (sessions survive restarts until `docker compose down -v`).
# Example: SESSION_SECRET=$(openssl rand -hex 32)
```

- `AERO_DOMAIN` (default: `localhost`)
  - `localhost` for local dev
  - `aero.example.com` (or similar) for production
- `AERO_GATEWAY_IMAGE` (default: `aero-gateway:dev`)
  - By default, `deploy/docker-compose.yml` builds `backend/aero-gateway` from source.
  - For production, prefer a published image and remove the compose `build:` stanza (or override it).
- `AERO_GATEWAY_GIT_SHA` (default: `dev`)
  - Optional build arg used to populate `GET /version` in `backend/aero-gateway`.
- `AERO_L2_PROXY_IMAGE` (default: `aero-l2-proxy:dev`)
  - By default, `deploy/docker-compose.yml` builds `crates/aero-l2-proxy` from source.
  - For production, prefer a published image and remove/override the compose `build:` stanza.
- `AERO_GATEWAY_UPSTREAM` (default: `aero-gateway:8080`)
  - Only change if your gateway listens on a different port inside docker.
- `AERO_L2_PROXY_UPSTREAM` (default: `aero-l2-proxy:8090`)
  - Only change if your L2 proxy listens on a different port inside docker.
- `AERO_L2_ALLOWED_ORIGINS_EXTRA` (default: empty)
  - Optional comma-prefixed origins appended to the L2 proxy Origin allowlist.
  - Example: `,https://localhost:5173`
- `AERO_L2_AUTH_MODE` (default: `none`)
  - Authentication mode for `/l2` (handled by `crates/aero-l2-proxy`).
  - Supported values: `none`, `session`, `token`, `session_or_token`, `session_and_token`, `jwt`, `cookie_or_jwt`.
    - Legacy aliases: `cookie`, `api_key`, `cookie_or_api_key`, `cookie_and_api_key`.
- `AERO_L2_SESSION_SECRET` (optional override)
  - Secret for validating the `aero_session` cookie when `AERO_L2_AUTH_MODE=session|cookie_or_jwt|session_or_token|session_and_token`.
  - `crates/aero-l2-proxy` reads this from `AERO_GATEWAY_SESSION_SECRET` (preferred) and falls back to
    `SESSION_SECRET` / `AERO_L2_SESSION_SECRET` (legacy), so the deploy stack can share one secret
    across both services.
- `AERO_L2_API_KEY` / `AERO_L2_JWT_SECRET` (optional)
  - Credentials for `AERO_L2_AUTH_MODE=token|jwt|cookie_or_jwt|session_or_token|session_and_token`.
  - Credentials can be delivered via query params:
    - Token auth: `?token=...` (preferred) (or `?apiKey=...` for compatibility)
    - JWT auth: `?token=...`
    or an additional `Sec-WebSocket-Protocol` entry `aero-l2-token.<value>` (offered alongside
    `aero-l2-tunnel-v1`; prefer this form when possible to avoid putting secrets in URLs/logs).
- `AERO_L2_TOKEN` (optional, legacy)
  - Legacy alias for token auth.
  - `deploy/docker-compose.yml` defaults `AERO_L2_AUTH_MODE=none`, so `AERO_L2_TOKEN` has no effect unless you
    explicitly enable token auth (e.g. `AERO_L2_AUTH_MODE=token|session_or_token|session_and_token`).
  - Accepted as a fallback value for `AERO_L2_API_KEY` when `AERO_L2_AUTH_MODE=token|session_or_token|session_and_token`.
  - Ignored when `AERO_L2_AUTH_MODE` is set to `session` (legacy alias: `cookie`), `jwt`, `cookie_or_jwt`, or `none`.
- `AERO_WEBRTC_UDP_RELAY_IMAGE` (default: `aero-webrtc-udp-relay:dev`)
  - When unset, docker compose builds the UDP relay from `proxy/webrtc-udp-relay/`.
- `AERO_WEBRTC_UDP_RELAY_UPSTREAM` (default: `aero-webrtc-udp-relay:8080`)
  - Only change if your relay listens on a different port inside docker.
- `AERO_WEBRTC_UDP_RELAY_ALLOWED_ORIGINS_EXTRA` (default: empty)
  - Optional comma-prefixed origins appended to the relay `ALLOWED_ORIGINS` allowlist.
  - Example: `,https://localhost:5173`
- `AERO_HSTS_MAX_AGE` (default: `0`)
  - `0` disables HSTS (good for local dev)
  - Recommended production value: `31536000` (1 year)
- `AERO_FRONTEND_ROOT` (default: `./static`)
  - Which directory Caddy serves as `/` (mounted at `/srv` in the container).
  - Recommended: `../dist` (after building the real frontend)
- `AERO_CSP_CONNECT_SRC_EXTRA` (default: empty)
  - Optional additional origins to allow in the Caddy Content Security Policy `connect-src`.
  - Use this if the frontend needs to connect to a separate origin for networking (e.g. a TCP proxy service).
  - Example: `AERO_CSP_CONNECT_SRC_EXTRA="https://proxy.example.com wss://proxy.example.com"`

Gateway environment variables (used by `backend/aero-gateway` and passed through in
`deploy/docker-compose.yml`):

- `PUBLIC_BASE_URL` (default in compose: `https://${AERO_DOMAIN}`)
  - Used to derive the default `ALLOWED_ORIGINS` allowlist.
- `SESSION_SECRET` (strongly recommended for production)
  - HMAC secret used to sign the `aero_session` cookie minted by `POST /session`.
  - Used to authenticate privileged endpoints like `/tcp`, and `/l2` when the L2 proxy is configured for
    session-cookie auth (`AERO_L2_AUTH_MODE=session|cookie_or_jwt|session_or_token|session_and_token`).
  - If unset, the deploy stack generates and persists a random secret in a Docker volume (sessions survive
    restarts until `docker compose down -v`).
  - When using session-cookie auth for the L2 tunnel (`AERO_L2_AUTH_MODE=session` / `cookie_or_jwt` / `session_or_token` / `session_and_token`), `crates/aero-l2-proxy`
    must share the same signing secret so it can validate the `aero_session` cookie minted by the gateway.
- `ALLOWED_ORIGINS` (optional, comma-separated)
  - Set explicitly if you need to allow additional origins (e.g. a dev server).
- `TRUST_PROXY` (default in compose: `1`)
  - Set to `1` only when the gateway is reachable **only** via the reverse proxy.
  - Required if you want `request.ip` / rate limiting to use `X-Forwarded-For`.
- `CROSS_ORIGIN_ISOLATION` (default in compose: `0`)
  - Set to `1` only if you are not injecting COOP/COEP headers at the edge proxy.

### WebRTC UDP relay configuration

The UDP relay (`proxy/webrtc-udp-relay`) has two networking surfaces:

- **HTTP (same-origin)**: proxied behind Caddy for:
  - `GET /webrtc/ice`
  - `GET /webrtc/signal` (WebSocket)
  - `POST /webrtc/offer` and `POST /offer`
  - `GET /udp` (WebSocket UDP fallback; same datagram framing as the WebRTC DataChannel)
- **UDP (not proxyable)**: ICE + data plane UDP ports must be reachable by browser clients.

Defaults in `deploy/docker-compose.yml`:

- `WEBRTC_UDP_PORT_MIN=50000`
- `WEBRTC_UDP_PORT_MAX=50100`
- Host publishing: `50000-50100/udp`

If `docker compose up` fails with a message like `port is already allocated`, pick a different range by
overriding `WEBRTC_UDP_PORT_MIN/MAX` in `deploy/.env` (the compose file uses these vars for both the
container env and the published UDP port range).

If you change the ICE port range, you must update:

1) The env vars (`WEBRTC_UDP_PORT_MIN/MAX`), and
2) The published UDP port range (firewall + docker `ports:`).

The relay supports authentication via `AUTH_MODE` (and `API_KEY`/`JWT_SECRET`). These values
are also used by `backend/aero-gateway` to mint `udpRelay.token` in `POST /session` so the
frontend can discover how to authenticate to the relay.

The relay also enforces a **UDP destination policy** (to prevent accidental open-proxy deployments).
By default, `proxy/webrtc-udp-relay` uses `DESTINATION_POLICY_PRESET=production` (**deny by default**),
so you must configure an allowlist (CIDRs/ports) before UDP relay traffic will flow.

For local development/testing, you can set `DESTINATION_POLICY_PRESET=dev` to allow by default.

See:

- `proxy/webrtc-udp-relay/README.md` (authoritative)

### Optional: L2 tunnel over WebRTC (relay bridging)

`proxy/webrtc-udp-relay` can also bridge a **fully reliable and ordered** WebRTC DataChannel
labeled `l2` to an
L2 tunnel backend WebSocket (typically `aero-l2-proxy`):

```
browser DataChannel "l2"  <->  aero-webrtc-udp-relay  <->  aero-l2-proxy /l2
```

The `l2` DataChannel must be configured as:

- `ordered = true`
- do **not** set `maxRetransmits` / `maxPacketLifeTime` (partial reliability)

This is useful when you want to carry the L2 tunnel over a UDP-based transport (WebRTC) for
experimentation under loss/NAT traversal.

To enable it in the compose stack, set in `deploy/.env`:

```bash
L2_BACKEND_WS_URL=ws://aero-l2-proxy:8090/l2
```

The relay can forward the client’s **Origin** and **AUTH_MODE credential** (JWT/token) to the
backend dial. Relevant env vars are documented in `proxy/webrtc-udp-relay/README.md`, including:

- `L2_BACKEND_AUTH_FORWARD_MODE=none|query|subprotocol`
- `L2_BACKEND_FORWARD_ORIGIN=1`
- `L2_BACKEND_FORWARD_AERO_SESSION=1` (recommended for `AERO_L2_AUTH_MODE=session`; forwards `Cookie: aero_session=...` captured from signaling)
- `L2_BACKEND_ORIGIN_OVERRIDE=https://example.com` (optional)

Note: the backend `/l2` endpoint must be configured to accept whatever auth material the relay forwards. For
session-cookie `/l2` (`AERO_L2_AUTH_MODE=session`; legacy alias: `cookie`), enable
`L2_BACKEND_FORWARD_AERO_SESSION=1` so WebRTC L2 bridging continues to work while preserving session-cookie auth.

## Optional TURN server (coturn profile)

Some client networks require TURN for reliable UDP connectivity. This repo includes an opt-in `coturn` service:

```bash
docker compose -f deploy/docker-compose.yml --profile turn up --build
```

Ports published by the TURN profile:

- `3478/udp` (TURN listening port)
- `49152-49200/udp` (TURN relay range; must match `proxy/webrtc-udp-relay/turn/turnserver.conf`)

To have browsers actually use TURN, configure the relay’s ICE server list to include a TURN URL
pointing at your host (often the same as `AERO_DOMAIN`):

- `AERO_ICE_SERVERS_JSON`, or
- `AERO_TURN_URLS` + `AERO_TURN_USERNAME` + `AERO_TURN_CREDENTIAL`

> Note: `turnserver.conf` defaults to `user=aero:aero` for local dev. Change credentials and
> `external-ip=...` before exposing TURN to the public internet.

## Local dev (self-signed TLS)

Run:

```bash
docker compose -f deploy/docker-compose.yml up --build
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
#
# Step 1: create a cookie-backed session (copy the `aero_session=...` value from Set-Cookie).
curl -k -i -X POST https://localhost/session -H 'content-type: application/json' -d '{}'
#
# Canonical (v1):
#   /tcp?v=1&host=<hostname-or-ip>&port=<port>
NODE_TLS_REJECT_UNAUTHORIZED=0 npx wscat \
  -c "wss://localhost/tcp?v=1&host=example.com&port=80" \
  -H "Cookie: aero_session=<paste-from-Set-Cookie>" \
  -o https://localhost

# Compatibility form (legacy; also supported by the gateway):
# NODE_TLS_REJECT_UNAUTHORIZED=0 npx wscat \
#   -c "wss://localhost/tcp?v=1&target=example.com:80" \
#   -H "Cookie: aero_session=<paste-from-Set-Cookie>" \
#   -o https://localhost
```

If you see a successful handshake but the connection immediately closes, the
gateway may be rejecting the query parameters or target.

> Note: `/tcp` is a privileged endpoint; the gateway rejects upgrades without an `aero_session` cookie.

## L2 tunnel proxy (/l2)

The **L2 tunnel proxy** (`aero-l2-proxy`) provides an Ethernet (L2) tunnel over
WebSocket:

- The browser connects to `wss://<AERO_DOMAIN>/l2`
- Caddy proxies the WebSocket upgrade to the `aero-l2-proxy` container
- The connection uses subprotocol: `aero-l2-tunnel-v1`

### L2 tunnel auth (session cookie)

`/l2` enforces an Origin allowlist by default. For production deployments you should also enable
authentication. The recommended mode for same-origin browser clients is session-cookie auth
(`AERO_L2_AUTH_MODE=session`; legacy alias: `cookie`):

- `aero-l2-proxy` validates the `aero_session` cookie minted by the gateway (`POST /session`).
  - Browser WebSocket handshakes include cookies automatically for same-origin connections.
  - CLI clients must provide `Cookie: aero_session=...` on the WebSocket upgrade.

For non-browser clients / internal bridges, you can switch to token-based auth
(`AERO_L2_AUTH_MODE=token|jwt`), accept either a session cookie or a token/JWT
(`AERO_L2_AUTH_MODE=session_or_token|cookie_or_jwt`), or require both
(`AERO_L2_AUTH_MODE=session_and_token`; legacy alias: `cookie_and_api_key`).
Credentials can be provided via:

- Query params: `?token=<value>` (preferred; `?apiKey=<value>` for compatibility)
- Preferred (avoids secrets in URLs/logs): offer an additional `Sec-WebSocket-Protocol` entry
  `aero-l2-token.<value>` (offered alongside `aero-l2-tunnel-v1`)

To disable auth (local dev only; NOT recommended for internet-exposed deployments), set
`AERO_L2_AUTH_MODE=none` in `deploy/.env`.

See `deploy/.env.example` for copy/paste configuration examples.

Endpoint discovery note: browser clients should treat the gateway as the canonical bootstrap API and
avoid hardcoding `/l2`. The `POST /session` response includes `endpoints.l2` (a same-origin path)
and `limits.l2` (payload size caps) so the frontend can connect and tune buffering without baking in
paths or protocol constants.

This endpoint is intended for the “Option C” architecture (tunneling Ethernet frames to a server-side
network stack / NAT / policy layer).

### Run locally

```bash
docker compose -f deploy/docker-compose.yml up --build
```

### Validate the upgrade (WSS)

> Note: `aero-l2-proxy` enforces an Origin allowlist by default. CLI clients must
> send an `Origin` header that matches the allowlist (in this deploy stack:
> `https://localhost` unless you changed `AERO_DOMAIN`).

Using `wscat`:

```bash
# Step 1: create a cookie-backed session (copy the `aero_session=...` value from Set-Cookie):
curl -k -i -X POST https://localhost/session -H 'content-type: application/json' -d '{}'
#
# Step 2: connect to /l2 with the Cookie header:
NODE_TLS_REJECT_UNAUTHORIZED=0 npx wscat \
  -c "wss://localhost/l2" \
  -s aero-l2-tunnel-v1 \
  -H "Cookie: aero_session=<paste-from-Set-Cookie>" \
  -o https://localhost

# Token auth (optional): provide a credential via query param:
# NODE_TLS_REJECT_UNAUTHORIZED=0 npx wscat \
#   -c "wss://localhost/l2?token=<credential>" \
#   -s aero-l2-tunnel-v1 \
#   -o https://localhost
```

Using `websocat`:

```bash
# Some `websocat` versions do not send an Origin header by default. If you get a 403,
# add it explicitly (see `websocat --help` for the exact flag in your version).
websocat --insecure --protocol aero-l2-tunnel-v1 \
  -H "Origin: https://localhost" \
  -H "Cookie: aero_session=<paste-from-Set-Cookie>" \
  wss://localhost/l2
#
# Token/JWT auth:
# websocat --insecure --protocol aero-l2-tunnel-v1 \
#   -H "Origin: https://localhost" \
#   wss://localhost/l2?token=<credential>
```

### Production note: egress policy

`aero-l2-proxy` is a **network egress surface**. For production deployments you should configure a
deny-by-default policy (ports/domains) to avoid exposing an open proxy.

Supported env vars include:

- `AERO_L2_ALLOWED_TCP_PORTS` (comma-separated)
- `AERO_L2_ALLOWED_UDP_PORTS` (comma-separated)
- `AERO_L2_ALLOWED_DOMAINS` / `AERO_L2_BLOCKED_DOMAINS` (comma-separated suffixes)
- `AERO_L2_ALLOW_PRIVATE_IPS=1` (dev-only; disables private/reserved IP blocking)
- `AERO_L2_MAX_UDP_FLOWS_PER_TUNNEL` (default: `256`; `0` disables)
- `AERO_L2_UDP_FLOW_IDLE_TIMEOUT_MS` (default: `60000`; `0` disables)
- `AERO_L2_STACK_MAX_TCP_CONNECTIONS` (default: `1024`)
- `AERO_L2_STACK_MAX_PENDING_DNS` (default: `1024`)
- `AERO_L2_STACK_MAX_DNS_CACHE_ENTRIES` (default: `10000`)
- `AERO_L2_STACK_MAX_BUFFERED_TCP_BYTES_PER_CONN` (default: `262144`)

See also: `docs/l2-tunnel-runbook.md` (production checklist).

## Networking smoke tests

```bash
# Gateway liveness (should return 200 + {"ok":true}):
curl -k https://localhost/healthz

# UDP relay ICE discovery:
# - Default (AUTH_MODE=none): should return 200 + {"iceServers":[...]} with no credentials.
# - If AUTH_MODE=api_key: requires an API key (X-API-Key) matching API_KEY.
curl -k https://localhost/webrtc/ice
# Or (api_key mode):
# curl -k -H "X-API-Key: <API_KEY>" https://localhost/webrtc/ice

# Session bootstrap (sets a Secure cookie when behind the TLS proxy and returns relay config):
curl -k -i -X POST https://localhost/session -H 'content-type: application/json' -d '{}'
```

### `/session` and UDP relay integration notes

`backend/aero-gateway` implements `POST /session` as the session bootstrap endpoint.
It sets the `aero_session` cookie and returns a JSON payload that includes (when configured):

- `udpRelay.baseUrl` (expected to be the same origin in this deploy stack)
- `udpRelay.endpoints`:
  - `/webrtc/ice` (ICE server discovery)
  - `/webrtc/signal` (WebSocket signaling)
  - `/webrtc/offer` (HTTP offer/answer flow)
  - `/udp` (WebSocket UDP fallback)
- `udpRelay.token` / `udpRelay.expiresAt` when `AUTH_MODE` is enabled

The relay also exposes `POST /offer` as a legacy HTTP offer/answer endpoint; new clients should prefer `POST /webrtc/offer` (which is what the gateway advertises via `udpRelay.endpoints.webrtcOffer`).

In this deploy stack, the gateway is configured to advertise the relay at the same origin
(`https://$AERO_DOMAIN`) so the browser can stay single-origin and avoid CORS/cookie issues.

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
This repo’s repo-root Vite app already includes these headers in `vite.harness.config.ts`
(and the legacy `web/` Vite app does as well via `web/vite.config.ts`).

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
    npm run build:prod
```

2) Replace the volume mount in `deploy/docker-compose.yml`:

Set `AERO_FRONTEND_ROOT` (recommended; no compose edits required):

```bash
# in deploy/.env (or export it in your shell)
    AERO_FRONTEND_ROOT=../dist
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

- Netlify + Cloudflare Pages headers: `web/public/_headers` (copied into `dist/_headers` on build)
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

To validate the compose stack end-to-end (build + security headers + `/healthz` + `/webrtc/ice` + `/dns-query` + `/tcp` + `/l2` WebSocket upgrades + `/udp` WebSocket datagram roundtrip + wasm MIME/caching), run:

```bash
bash deploy/scripts/smoke.sh
```
