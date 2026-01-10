# aero-storage-server

HTTP server for streaming large (20GB+) disk images to the browser.

This crate currently serves images from a **local filesystem directory** and exposes Prometheus
metrics and basic health endpoints.

## Endpoints

- `GET /healthz` – liveness probe, returns `200 OK` JSON `{ "status": "ok" }`
- `GET /readyz` – readiness probe, returns `200 OK` JSON `{ "status": "ok" }`
- `GET /metrics` – Prometheus text exposition format (`text/plain; version=0.0.4`)
- `GET /v1/images` – list available images
- `GET /v1/images/:id/meta` – image metadata (size, etag, last_modified, etc)
- `GET|HEAD /v1/images/:image_id` (or `/v1/images/:image_id/data`) – stream image bytes
  (supports `Range` requests)

## Configuration (canonical)

Configuration is via **CLI flags with env var fallbacks** (powered by `clap`).

| Flag | Env var | Default |
| --- | --- | --- |
| `--listen-addr` | `AERO_STORAGE_LISTEN_ADDR` | `0.0.0.0:8080` |
| `--cors-origin` | `AERO_STORAGE_CORS_ORIGIN` | _(unset)_ |
| `--images-root` | `AERO_STORAGE_IMAGE_ROOT` | `./images` |
| `--log-level` | `AERO_STORAGE_LOG_LEVEL` | `info` |

## Run

From the repo root:

```bash
cargo run -p aero-storage-server
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

## Reverse proxy (TLS + HTTP/2)

See `deploy/nginx/nginx.conf` for an example nginx configuration. It highlights the important bits
for disk image streaming:

- keep compression disabled on `/v1/images/…` (compression breaks byte ranges)
- increase timeouts and avoid buffering whole responses
