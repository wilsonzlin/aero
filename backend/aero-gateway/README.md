# aero-gateway

Production-grade backend service for Aero.

This package includes a **DNS-over-HTTPS** endpoint (`RFC 8484`) intended for browser-based guest networking without relying on third-party DoH providers (and their CORS policies).

This gateway can run either:

- **Directly with built-in TLS** (HTTPS/WSS) for simpler local dev / single-binary deployments.
- **Behind a reverse proxy** (HTTP internally, HTTPS externally).

## Requirements

- Node.js (use the repo-pinned version from [`.nvmrc`](../../.nvmrc))

## Install

```bash
# From the repo root (npm workspaces)
npm ci
```

## Run (dev)

```bash
npm -w backend/aero-gateway run dev
```

## Run (prod)

```bash
npm -w backend/aero-gateway run build
npm -w backend/aero-gateway start
```

## Lint

```bash
npm -w backend/aero-gateway run lint
```

## Test

```bash
npm -w backend/aero-gateway test
```

## Docker

Build:

```bash
docker build -f backend/aero-gateway/Dockerfile -t aero-gateway . \
  --build-arg NODE_VERSION="$(tr -d '\r\n' < .nvmrc | sed 's/^v//')" \
  --build-arg VERSION="dev" \
  --build-arg GIT_SHA="$(git rev-parse HEAD)" \
  --build-arg BUILD_TIMESTAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
```

Run:

```bash
docker run --rm -p 8080:8080 aero-gateway
```

## Endpoints

- `GET /healthz` liveness
- `GET /readyz` readiness
- `GET /version` build/version info
- `GET /metrics` Prometheus metrics
- `POST /session` issues the `aero_session` cookie used by `/dns-query`, `/tcp`, and `/tcp-mux` (and advertises base-path-aware endpoint paths for browser clients via the `endpoints` object)
  - `endpoints.l2` points at the Option C L2 tunnel endpoint (`/l2`, subprotocol `aero-l2-tunnel-v1`), which is served by `aero-l2-proxy` behind the reverse proxy (not by the Node gateway process itself).
  - `limits.l2` provides protocol payload size caps (`FRAME` vs control messages) so clients can tune buffering.
  - When the gateway is configured with `UDP_RELAY_BASE_URL`, the JSON response also includes `udpRelay` metadata (base URL + endpoints + short-lived token) for `proxy/webrtc-udp-relay`.
- `GET /session` (dev-only helper; not part of the backend contract) sets a session cookie so Secure-cookie behavior is easy to validate in local TLS / reverse-proxy setups
- `GET|POST /dns-query` DNS-over-HTTPS (`RFC 8484`; requires `aero_session` cookie)
- `POST /udp-relay/token` mint a fresh short-lived UDP relay credential for the current session (optional; requires `aero_session` cookie + `Origin`; only enabled when `UDP_RELAY_BASE_URL` is configured)
- `GET ws(s)://<host>/tcp?...` TCP proxy upgrade endpoint (WebSocket; see `docs/backend/01-aero-gateway-api.md` and `deploy/README.md`)
- `GET ws(s)://<host>/tcp-mux` Multiplexed TCP proxy upgrade endpoint (WebSocket; subprotocol `aero-tcp-mux-v1`)

## Examples

### `/tcp-mux` client

After starting the gateway locally, you can run a simple Node client that:

- bootstraps a session cookie via `POST /session`
- opens a `/tcp-mux` WebSocket using the `aero-tcp-mux-v1` framing
- sends a basic HTTP/1.1 request and prints the response bytes

```bash
node backend/aero-gateway/examples/tcp-mux-client.js http://127.0.0.1:8080 example.com 80
```

Note: the gateway blocks private/loopback IPs by default. To test against a local TCP echo server, run the gateway with `TCP_ALLOW_PRIVATE_IPS=1` (local-dev only).

## DNS-over-HTTPS (`/dns-query`)

### Supported request formats

- `GET /dns-query?dns=<base64url>` (`RFC 8484` GET)
- `POST /dns-query` with `Content-Type: application/dns-message` (`RFC 8484` POST)

Successful responses are `Content-Type: application/dns-message`.

### Quick test with curl (GET)

First, bootstrap a session cookie:

```bash
COOKIE=$(curl -sS -D - -o /dev/null -X POST 'http://127.0.0.1:8080/session' \
  | awk -F': ' 'tolower($1)=="set-cookie"{print $2}' \
  | head -n1 \
  | cut -d';' -f1 \
  | tr -d '\r')
echo "$COOKIE"
```

The following `dns` query is a standard `A` query for `example.com` with ID `0x0000`:

```bash
curl -sS \
  'http://127.0.0.1:8080/dns-query?dns=AAABAAABAAAAAAAAB2V4YW1wbGUDY29tAAABAAE' \
  -H "Cookie: $COOKIE" \
  -H 'Accept: application/dns-message' \
  --output response.bin

xxd response.bin | head
```

## Environment variables

Required / commonly used:

- `HOST` (default: `0.0.0.0`)
- `PORT` (default: `8080`)
- `LOG_LEVEL` (default: `info`)
- `PUBLIC_BASE_URL` (default: `http://localhost:${PORT}`, or `https://localhost:${PORT}` when `TLS_ENABLED=1`)
  - May include a path prefix when served behind a reverse proxy at a subpath (e.g. `https://example.com/aero`).
  - `POST /session` endpoint discovery prefixes `endpoints.*` with the `.pathname` of this URL.
- `ALLOWED_ORIGINS` (comma-separated origins; default: `PUBLIC_BASE_URL` origin)
- `CROSS_ORIGIN_ISOLATION=1` to enable COOP/COEP headers
- `TRUST_PROXY=1` to trust `X-Forwarded-*` headers from a reverse proxy (only enable when not directly exposed)
- `SHUTDOWN_GRACE_MS` (default: `10000`)

Security:

- `RATE_LIMIT_REQUESTS_PER_MINUTE` (default: `0` = disabled; applies to all routes)

Sessions (cookies):

- `AERO_GATEWAY_SESSION_SECRET` / `SESSION_SECRET` (recommended; if unset the gateway generates a temporary secret and sessions won't survive restarts)
- `SESSION_TTL_SECONDS` (default: `86400`)
- `SESSION_COOKIE_SAMESITE` (default: `Lax`; set to `None` for cross-site deployments, which also requires HTTPS + `Secure`)

TCP proxy (`/tcp`, `/tcp-mux`):

- `TCP_ALLOWED_HOSTS` (comma-separated patterns; default: allow all)
  - Supports exact matches (`example.com`) and wildcard subdomain matches (`*.example.com`).
  - If non-empty, the target must match at least one pattern.
- `TCP_BLOCKED_HOSTS` (comma-separated patterns; default: none)
  - Always enforced; deny overrides allow.
- `TCP_REQUIRE_DNS_NAME=1|0` (default: `0`)
  - When enabled, disallow IP-literal targets entirely (force DNS names).
- `TCP_ALLOW_PRIVATE_IPS=1|0` (default: `0`)
  - When enabled, allow dialing loopback/RFC1918/link-local/etc IP ranges.
  - **Security note:** this disables the default DNS-rebinding/SSRF mitigation (public-IP-only filtering) and should only be enabled in trusted/local-dev environments.
- `TCP_ALLOWED_PORTS` (comma-separated; default: allow all)
- `TCP_BLOCKED_CLIENT_IPS` (comma-separated; default: none)
- `TCP_PROXY_MAX_CONNECTIONS` (default: `64`; max concurrent TCP connections per session; `/tcp-mux` streams count as connections)
- `TCP_PROXY_MAX_MESSAGE_BYTES` (default: `1048576`)
- `TCP_PROXY_CONNECT_TIMEOUT_MS` (default: `10000`)
- `TCP_PROXY_IDLE_TIMEOUT_MS` (default: `300000`)
- `TCP_MUX_MAX_STREAMS` (default: `1024`)
- `TCP_MUX_MAX_STREAM_BUFFER_BYTES` (default: `1048576`)
- `TCP_MUX_MAX_FRAME_PAYLOAD_BYTES` (default: `16777216`)

L2 tunnel (`/l2`) payload limits (advertised via `POST /session` `limits.l2.*`):

- `AERO_L2_MAX_FRAME_PAYLOAD` (default: `2048`; legacy alias: `AERO_L2_MAX_FRAME_SIZE`)
  - Maximum payload bytes for L2 tunnel `FRAME` messages.
  - This value is surfaced to clients as `limits.l2.maxFramePayloadBytes`.
- `AERO_L2_MAX_CONTROL_PAYLOAD` (default: `256`)
  - Maximum payload bytes for L2 tunnel control messages (`PING`/`PONG`/`ERROR`).
  - This value is surfaced to clients as `limits.l2.maxControlPayloadBytes`.
  - Values must be positive integers; `0`/blank are treated as unset (defaults apply).

Note: the gateway does **not** terminate `/l2` itself; `aero-l2-proxy` enforces these limits at runtime. Keep
the gateway's advertised values in sync with the actual proxy configuration.

DNS-over-HTTPS:

- `DNS_UPSTREAMS` (default: `1.1.1.1:53,8.8.8.8:53`)
- `DNS_UPSTREAM_TIMEOUT_MS` (default: `2000`)
- `DNS_CACHE_MAX_ENTRIES` (default: `10000`)
- `DNS_CACHE_MAX_TTL_SECONDS` (default: `300`)
- `DNS_CACHE_NEGATIVE_TTL_SECONDS` (default: `60`)
- `DNS_MAX_QUERY_BYTES` (default: `4096`)
- `DNS_MAX_RESPONSE_BYTES` (default: `4096`)
- `DNS_QPS_PER_IP` (default: `10`)
- `DNS_BURST_PER_IP` (default: `20`)
- `DNS_ALLOW_ANY=1` to allow `ANY` queries (default: blocked)
- `DNS_ALLOW_PRIVATE_PTR=1` to allow PTR queries to private ranges (default: blocked)

### UDP relay integration (optional)

When configured, the gateway includes an `udpRelay` field in `POST /session` responses, allowing the browser to discover the UDP relay service (`proxy/webrtc-udp-relay`) and obtain short-lived credentials.

- `UDP_RELAY_BASE_URL` (default: unset; accepts `http(s)://` or `ws(s)://`)
- `UDP_RELAY_AUTH_MODE` (`none`, `api_key`, or `jwt`; default: `none`)
- `UDP_RELAY_API_KEY` (used when `UDP_RELAY_AUTH_MODE=api_key`)
- `UDP_RELAY_JWT_SECRET` (used when `UDP_RELAY_AUTH_MODE=jwt`, HS256)
- `UDP_RELAY_TOKEN_TTL_SECONDS` (default: `300`)
- `UDP_RELAY_AUDIENCE` (optional; JWT `aud`)
- `UDP_RELAY_ISSUER` (optional; JWT `iss`)

### Built-in TLS (HTTPS/WSS)

- `TLS_ENABLED=1|0` (default: `0`)
- `TLS_CERT_PATH` (required when `TLS_ENABLED=1`)
- `TLS_KEY_PATH` (required when `TLS_ENABLED=1`)

When `TLS_ENABLED=1`, the gateway listens on **HTTPS** and `/tcp` upgrades are **WSS**.

### Reverse proxy support (TLS termination)

- `TRUST_PROXY=1|0` (default: `0`)

When `TRUST_PROXY=1`, the gateway will trust `X-Forwarded-Proto: https` for determining whether a
request is “secure” (e.g. when deciding whether to add the `Secure` attribute to cookies).

Only enable `TRUST_PROXY=1` when the gateway is **only reachable via a trusted reverse proxy**,
otherwise clients can spoof `X-Forwarded-Proto`.

Not implemented yet:

- `TCP_PROXY_MAX_CONNECTIONS_PER_IP`

## Local dev: generate a self-signed cert

This repo includes an OpenSSL-only helper script:

```bash
backend/aero-gateway/scripts/generate-dev-cert.sh
```

It writes a self-signed `localhost` certificate to:

```
backend/aero-gateway/.certs/localhost.crt
backend/aero-gateway/.certs/localhost.key
```

Those files are gitignored.

### Running with TLS

```bash
npm -w backend/aero-gateway run build

TLS_ENABLED=1 \
TLS_CERT_PATH=".certs/localhost.crt" \
TLS_KEY_PATH=".certs/localhost.key" \
npm -w backend/aero-gateway start
```

Then:

- `https://localhost:8080/healthz`
- `wss://localhost:8080/tcp?v=1&host=example.com&port=80`

### Trusting the certificate in your browser/OS

Browsers will warn on self-signed certs by default. For a trusted local certificate experience,
use a reverse proxy like **Caddy** (or similar) that can manage local trust, or add the generated
certificate to your OS trust store.

## Local dev Origin allowlist

Browser-initiated requests that include an `Origin` header are rejected unless the origin is in `ALLOWED_ORIGINS`.

For example, if your frontend dev server runs on `http://localhost:5173`:

```bash
export ALLOWED_ORIGINS="http://localhost:5173"
```

If you run the frontend via the gateway's own static hosting (same-origin), leaving `ALLOWED_ORIGINS` unset will default it to `PUBLIC_BASE_URL`'s origin.

## Running behind an HTTPS reverse proxy

In production you typically terminate TLS in a reverse proxy (nginx, Caddy, Cloudflare) and
forward requests to `aero-gateway` over HTTP.

Make sure to set:

- `PUBLIC_BASE_URL=https://<your-domain>`
- `ALLOWED_ORIGINS=https://<your-domain>` (or leave unset to default to `PUBLIC_BASE_URL`)
- `TRUST_PROXY=1` (so rate limiting/logs use the real client IP via `X-Forwarded-For`, and secure cookies can rely on `X-Forwarded-Proto`)

See [`deploy/README.md`](../../deploy/README.md) for a ready-to-run Caddy + docker-compose setup that terminates TLS, enforces COOP/COEP, and proxies `/tcp` + HTTP APIs to the gateway.
