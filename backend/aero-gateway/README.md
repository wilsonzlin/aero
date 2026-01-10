# aero-gateway

Production-grade backend service skeleton for Aero.

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

## Environment variables

Required / commonly used:

- `HOST` (default: `0.0.0.0`)
- `PORT` (default: `8080`)
- `LOG_LEVEL` (default: `info`)
- `PUBLIC_BASE_URL` (default: `http://localhost:${PORT}`)
- `ALLOWED_ORIGINS` (comma-separated origins; default: `PUBLIC_BASE_URL` origin)
- `CROSS_ORIGIN_ISOLATION=1` to enable COOP/COEP headers
- `SHUTDOWN_GRACE_MS` (default: `10000`)

Security:

- `RATE_LIMIT_REQUESTS_PER_MINUTE` (default: `0` = disabled)

Placeholders (not implemented yet):

- `TCP_PROXY_MAX_CONNECTIONS`
- `TCP_PROXY_MAX_CONNECTIONS_PER_IP`
- `DNS_UPSTREAMS` (comma-separated)

## Endpoints

- `GET /healthz` liveness
- `GET /readyz` readiness
- `GET /version` build/version info
- `GET /metrics` Prometheus metrics

## Local dev Origin allowlist

Browser-initiated requests that include an `Origin` header are rejected unless the origin
is in `ALLOWED_ORIGINS`.

For example, if your frontend dev server runs on `http://localhost:5173`:

```bash
export ALLOWED_ORIGINS="http://localhost:5173"
```

If you run the frontend via the gateway's own static hosting (same-origin), leaving
`ALLOWED_ORIGINS` unset will default it to `PUBLIC_BASE_URL`'s origin.

## Running behind an HTTPS reverse proxy

In production you typically terminate TLS in a reverse proxy (nginx, Caddy, Cloudflare)
and forward requests to `aero-gateway` over HTTP.

Make sure to set:

- `PUBLIC_BASE_URL=https://<your-domain>`
- `ALLOWED_ORIGINS=https://<your-domain>` (or leave unset to default to `PUBLIC_BASE_URL`)

