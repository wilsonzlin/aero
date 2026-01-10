# aero-gateway

Production-grade backend service for Aero.

This package includes a **DNS-over-HTTPS** endpoint (`RFC 8484`) intended for browser-based guest networking without relying on third-party DoH providers (and their CORS policies).

This gateway can run either:

- **Directly with built-in TLS** (HTTPS/WSS) for simpler local dev / single-binary deployments.
- **Behind a reverse proxy** (HTTP internally, HTTPS externally).

## Requirements

- Node.js 20+

## Install

```bash
cd backend/aero-gateway
npm install
```

## Run (dev)

```bash
cd backend/aero-gateway
npm run dev
```

## Run (prod)

```bash
cd backend/aero-gateway
npm run build
npm start
```

## Lint

```bash
cd backend/aero-gateway
npm run lint
```

## Test

```bash
cd backend/aero-gateway
npm test
```

## Docker

Build:

```bash
docker build -t aero-gateway backend/aero-gateway \
  --build-arg GIT_SHA="$(git rev-parse HEAD)"
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
- `GET /session` sets a demo cookie so Secure-cookie behavior is easy to validate in local TLS / reverse-proxy setups
- `GET|POST /dns-query` DNS-over-HTTPS (`RFC 8484`)
- `GET ws(s)://<host>/tcp?...` TCP proxy upgrade endpoint (WebSocket; see `docs/backend/01-aero-gateway-api.md` and `deploy/README.md`)
- `GET ws(s)://<host>/tcp-mux` Multiplexed TCP proxy upgrade endpoint (WebSocket; subprotocol `aero-tcp-mux-v1`)

## DNS-over-HTTPS (`/dns-query`)

### Supported request formats

- `GET /dns-query?dns=<base64url>` (`RFC 8484` GET)
- `POST /dns-query` with `Content-Type: application/dns-message` (`RFC 8484` POST)

Successful responses are `Content-Type: application/dns-message`.

### Quick test with curl (GET)

The following `dns` query is a standard `A` query for `example.com` with ID `0x0000`:

```bash
curl -sS \
  'http://127.0.0.1:8080/dns-query?dns=AAABAAABAAAAAAAAB2V4YW1wbGUDY29tAAABAAE' \
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
- `ALLOWED_ORIGINS` (comma-separated origins; default: `PUBLIC_BASE_URL` origin)
- `CROSS_ORIGIN_ISOLATION=1` to enable COOP/COEP headers
- `TRUST_PROXY=1` to trust `X-Forwarded-*` headers from a reverse proxy (only enable when not directly exposed)
- `SHUTDOWN_GRACE_MS` (default: `10000`)

Security:

- `RATE_LIMIT_REQUESTS_PER_MINUTE` (default: `0` = disabled; applies to all routes)

TCP proxy (`/tcp`, `/tcp-mux`):

- `TCP_ALLOWED_HOSTS` (comma-separated; default: allow all)
- `TCP_ALLOWED_PORTS` (comma-separated; default: allow all)
- `TCP_BLOCKED_CLIENT_IPS` (comma-separated; default: none)
- `TCP_MUX_MAX_STREAMS` (default: `1024`)
- `TCP_MUX_MAX_STREAM_BUFFER_BYTES` (default: `1048576`)
- `TCP_MUX_MAX_FRAME_PAYLOAD_BYTES` (default: `16777216`)

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

Placeholders (not implemented yet):

- `TCP_PROXY_MAX_CONNECTIONS`
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
cd backend/aero-gateway
npm run build

TLS_ENABLED=1 \
TLS_CERT_PATH="$PWD/.certs/localhost.crt" \
TLS_KEY_PATH="$PWD/.certs/localhost.key" \
npm start
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
