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
- `GET|HEAD /v1/images/:image_id/chunked/manifest.json` (or `/manifest`) – fetch a pre-chunked image manifest (no `Range`)
- `GET|HEAD /v1/images/:image_id/chunked/chunks/:chunkName` – fetch a single chunk object (no `Range`)
- `GET|HEAD /v1/images/:image_id/chunked/:version/manifest.json` (or `/manifest`) – fetch a versioned chunk manifest (recommended)
- `GET|HEAD /v1/images/:image_id/chunked/:version/chunks/:chunkName` – fetch a versioned chunk object (recommended)

Notes:

- `image_id` is treated as an opaque identifier and must match **`[A-Za-z0-9._-]{1,128}`**
  (and must not be `.` or `..`).
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
| `--max-chunk-bytes` | `AERO_STORAGE_MAX_CHUNK_BYTES` | `8388608` (8 MiB) |
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
- `--require-range` rejects `GET` requests that would otherwise stream the full image body.
  - Missing/invalid/unsupported `Range` → `416 Range Not Satisfiable`.
  - `If-Range` present but not usable (mismatch/weak/invalid/no validator) → `412 Precondition Failed`.

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

## Chunked disk images (no-Range delivery)

In addition to HTTP `Range` streaming, `aero-storage-server` can serve **pre-chunked** disk images
using the chunked format described in [`docs/18-chunked-disk-image-format.md`](../../docs/18-chunked-disk-image-format.md).

On-disk layout under `--images-root`:

```text
chunked/<image_id>/
  manifest.json
  chunks/
    00000000.bin
    00000001.bin
    ...
```

Notes:

- `image_id` uses the same validation rules as the main bytes endpoints:
  `[A-Za-z0-9._-]{1,128}` (and must not be `.` or `..`).
- `chunkName` is validated as `^[0-9]{1,32}\\.bin$` (zero-padded decimal chunk index; width should
  match the manifest `chunkIndexWidth`).
- Chunk responses are capped by `--max-chunk-bytes` (default: 8 MiB) as basic DoS hardening.
- Public chunked responses use long-lived `Cache-Control` (including `immutable`). Treat the
  on-disk `chunked/<image_id>/...` contents as immutable, or publish new content under a new
  `image_id` (or include a version string in the `image_id`).

If you want to follow the recommended versioned layout from the chunked format docs, you can set
`chunked_version` in the image catalog `manifest.json` entry and store chunked artifacts under:

```text
chunked/<image_id>/<chunked_version>/
  manifest.json
  chunks/...
```

When using the versioned layout, you can also point clients at the versioned manifest endpoint:
`/v1/images/<image_id>/chunked/<chunked_version>/manifest.json`. Chunk URLs are derived relative to
the manifest URL, so this yields stable versioned chunk URLs under
`/v1/images/<image_id>/chunked/<chunked_version>/chunks/...`.

The manifest supports optional per-image cache validator overrides to enable stable browser (OPFS)
and CDN caching even if filesystem mtimes change during copy/restore:

- `etag`: a quoted HTTP entity-tag (e.g. `"win7-sp1-x64-v1"` or `W/"win7-sp1-x64-v1"`). Prefer a
  strong (non-`W/`) tag so `If-Range` can be used for range resumption. The value must be visible
  ASCII (so clients can round-trip it through `If-None-Match`/`If-Range`) and is subject to a
  server-side maximum length limit (currently 1024 bytes).
- `last_modified`: an RFC3339 timestamp (e.g. `2026-01-10T00:00:00Z`). Must be at or after the Unix
  epoch so it can be represented in an HTTP `Last-Modified` header.

In production, strongly consider enabling `--require-manifest` (or `AERO_STORAGE_REQUIRE_MANIFEST`)
to **disable directory listing fallback**. This prevents accidentally exposing arbitrary files
placed in the images directory.

## Reverse proxy (TLS + HTTP/2)

See `deploy/nginx/aero-storage-server.conf` for an example nginx configuration. It highlights the
important bits for disk image streaming:

- keep compression disabled on `/v1/images/…` (compression breaks byte ranges)
- increase timeouts and avoid buffering whole responses
