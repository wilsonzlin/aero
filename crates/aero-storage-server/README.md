# aero-storage-server

HTTP server for streaming large disk images from a local filesystem directory.

## Endpoints

- `GET /healthz` – health probe, returns `200 OK`
- `GET /v1/images/<image_id>` – streams files rooted at `AERO_STORAGE_IMAGE_ROOT` (supports `Range` requests)

## Configuration

Environment variables:

- `AERO_STORAGE_LISTEN_ADDR` (default: `0.0.0.0:8080`)
- `AERO_STORAGE_IMAGE_ROOT` (default: `./images`)

## Local development (Docker Compose)

From the repo root:

```bash
docker compose up --build
```

Put disk images under `./images` on the host, then:

```bash
curl -fsS http://localhost:8080/healthz
curl -I http://localhost:8080/v1/images/my-disk.img
```

## Reverse proxy (TLS + HTTP/2)

See `deploy/nginx/nginx.conf` for an example nginx configuration. It highlights the important bits for disk image streaming:

- keep compression disabled on `/v1/images/…` (compression breaks byte ranges)
- increase timeouts and avoid buffering whole responses
