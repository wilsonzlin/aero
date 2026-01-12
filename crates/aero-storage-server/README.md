# aero-storage-server

HTTP server for streaming large (20GB+) disk images to the browser.

This crate currently serves images from a **local filesystem directory** and exposes Prometheus
metrics and basic health endpoints.

## Endpoints

- `GET /healthz` – liveness probe, returns `200 OK` JSON `{ "status": "ok" }`
- `GET /readyz` – readiness probe, returns `200 OK` JSON `{ "status": "ok" }` when ready, and
  `503 Service Unavailable` JSON `{ "status": "error" }` when not ready
- `GET /metrics` – Prometheus text exposition format (`text/plain; version=0.0.4`)
  - Can be disabled (`--disable-metrics`) or protected with a bearer token
    (`--metrics-auth-token`).
- `GET /v1/images` – list available images
- `GET /v1/images/:id/meta` – image metadata (size, etag, last_modified, etc)
- `GET|HEAD /v1/images/:image_id` (or `/v1/images/:image_id/data`) – stream image bytes
  (supports `Range` requests)

Notes:

- `image_id` is treated as an opaque identifier and must match **`[A-Za-z0-9._-]{1,128}`**.
  Requests with invalid/overlong IDs are rejected early (and the server avoids recording unbounded
  ID values in logs/metrics).
- The image bytes endpoints are protected by a per-process concurrency cap and may return
  `429 Too Many Requests` under load (see `--max-concurrent-bytes-requests`).

## Configuration (canonical)

Configuration is via **CLI flags with env var fallbacks** (powered by `clap`).

| Flag | Env var | Default |
| --- | --- | --- |
| `--listen-addr` | `AERO_STORAGE_LISTEN_ADDR` | `0.0.0.0:8080` |
| `--cors-origin` | `AERO_STORAGE_CORS_ORIGIN` | _(unset → `Access-Control-Allow-Origin: *`)_ |
| `--cross-origin-resource-policy` | `AERO_STORAGE_CROSS_ORIGIN_RESOURCE_POLICY` | `same-site` |
| `--images-root` | `AERO_STORAGE_IMAGE_ROOT` | `./images` |
| `--require-manifest` | `AERO_STORAGE_REQUIRE_MANIFEST` | `false` |
| `--log-level` | `AERO_STORAGE_LOG_LEVEL` | `info` |
| `--max-concurrent-bytes-requests` | `AERO_STORAGE_MAX_CONCURRENT_BYTES_REQUESTS` | `64` (0 = unlimited) |
| `--max-range-bytes` | `AERO_STORAGE_MAX_RANGE_BYTES` | `8388608` (8 MiB) |
| `--public-cache-max-age-secs` | `AERO_STORAGE_PUBLIC_CACHE_MAX_AGE_SECS` | `3600` |
| `--cors-preflight-max-age-secs` | `AERO_STORAGE_CORS_PREFLIGHT_MAX_AGE_SECS` | `86400` |
| `--require-range` | `AERO_STORAGE_REQUIRE_RANGE` | `false` |
| `--disable-metrics` | `AERO_STORAGE_DISABLE_METRICS` | `false` |
| `--metrics-auth-token` | `AERO_STORAGE_METRICS_AUTH_TOKEN` | _(unset)_ |

Notes:

- `--cors-origin` can be repeated or provided as a comma-separated list.
  - If set to `*`, the server responds with `Access-Control-Allow-Origin: *` and does **not** send
    `Access-Control-Allow-Credentials`.
  - If set to an allowlist, the server will echo back the request `Origin` only when it is in the
    allowlist.
  - When configured with an allowlist (not `*`), the server defaults to sending
    `Access-Control-Allow-Credentials: true` so cookie-authenticated cross-origin requests can
    succeed.
- `--cross-origin-resource-policy` controls the `Cross-Origin-Resource-Policy` response header on
  image bytes responses (defence-in-depth for `COEP: require-corp`). The default `same-site` works
  well when the app and storage server are on the same eTLD+1 (e.g. `app.example.com` and
  `images.example.com`).
- `/metrics` is intended for Prometheus scraping and **should not be publicly exposed**. In
  production, either restrict access at the network layer (e.g. private service / network policy),
  protect it with `--metrics-auth-token`, or disable it with `--disable-metrics`.
  - If both `--disable-metrics` and `--metrics-auth-token` are set, disablement wins (the endpoint
    will not be mounted).

## Run

From the repo root:

```bash
cargo run --locked -p aero-storage-server
```

Then in another terminal:

```bash
curl -sSf http://localhost:8080/healthz
curl -sSf http://localhost:8080/readyz
curl -sSf http://localhost:8080/metrics
```

Put disk images under `./images` (or the configured `--images-root`). If a `manifest.json` exists
under the images root, it is used as the image catalog; otherwise the server falls back to a
directory listing (development only).

The manifest supports optional per-image cache validator overrides to enable stable browser (OPFS)
and CDN caching even if filesystem mtimes change during copy/restore:

- `etag`: a quoted HTTP entity-tag (e.g. `"win7-sp1-x64-v1"` or `W/"win7-sp1-x64-v1"`). Prefer a
  strong (non-`W/`) tag so `If-Range` can be used for range resumption.
- `last_modified`: an RFC3339 timestamp (e.g. `2026-01-10T00:00:00Z`)

In production, strongly consider enabling `--require-manifest` (or `AERO_STORAGE_REQUIRE_MANIFEST`)
to **disable directory listing fallback**. This prevents accidentally exposing arbitrary files
placed in the images directory.

## Reverse proxy (TLS + HTTP/2)

See `deploy/nginx/aero-storage-server.conf` for an example nginx configuration. It highlights the
important bits for disk image streaming:

- keep compression disabled on `/v1/images/…` (compression breaks byte ranges)
- increase timeouts and avoid buffering whole responses
