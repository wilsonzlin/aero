# Local object store (MinIO) for Range + CORS testing

This directory provides a self-contained local environment to validate:

- **HTTP Range** behavior (`206 Partial Content`, `Content-Range`, etc)
- **CORS / preflight** behavior for `Range` requests (browser `OPTIONS` flow)
- (Optional) **CDN/proxy “edge” behavior** (CORS header overrides, preflight handling)

It is intended for local development workflows where disk images (multi‑GB) are stored in an S3-compatible object store.

## Services

| Service | Purpose | URL |
| --- | --- | --- |
| MinIO (S3 API) | S3-compatible origin | `http://localhost:9000` |
| MinIO Console | Upload/browse objects via UI | `http://localhost:9001` |
| `minio-proxy` (optional) | Reverse proxy in front of MinIO (edge/CDN emulation) | `http://localhost:9002` |

Default credentials:

- `MINIO_ROOT_USER=minioadmin`
- `MINIO_ROOT_PASSWORD=minioadmin`

Default bucket:

- `BUCKET_NAME=disk-images`

Default CORS origin:

- `CORS_ALLOWED_ORIGIN=http://localhost:5173` (Vite default)

## Start / stop

From this directory:

```bash
docker compose up
```

You can override defaults by exporting environment variables before starting (or by creating a `.env` file in this directory), for example:

```bash
export CORS_ALLOWED_ORIGIN=http://localhost:3000
docker compose up
```

If you change `MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD`, also reset volumes so the persisted `mc` config is regenerated:

```bash
docker compose down -v
```

Stop and remove containers (keeps volumes by default):

```bash
docker compose down
```

To also remove persisted data:

```bash
docker compose down -v
```

### Optional proxy (“CDN”) layer

Enable the proxy container with a Compose profile:

```bash
docker compose --profile proxy up
```

The proxy is useful for reproducing “edge” behaviors (for example, overriding CORS headers and handling preflights at the proxy instead of the origin).

## Upload a large file

### Option A: MinIO Console UI (no extra tooling)

1. Open `http://localhost:9001`
2. Log in with the credentials above
3. Open the `disk-images` bucket
4. Upload a file (for example, a multi‑GB disk image)

### Option B: Use `mc` via Docker Compose (no local install)

Create a large file:

```bash
dd if=/dev/zero of=./large.bin bs=1M count=64
```

Upload it to MinIO:

```bash
docker compose --profile tools run --rm mc cp ./large.bin local/disk-images/large.bin
```

List objects:

```bash
docker compose --profile tools run --rm mc ls local/disk-images
```

## Verify Range responses (206 Partial Content)

> These examples assume you uploaded `large.bin` to `disk-images/large.bin`.

### Direct to MinIO origin

```bash
curl -s -D - -o /dev/null \
  -H 'Range: bytes=0-15' \
  http://localhost:9000/disk-images/large.bin
```

To validate the **CORS + ExposeHeaders** behavior, include an `Origin` header and look for `Access-Control-Expose-Headers: ... Content-Range ...`:

```bash
curl -s -D - -o /dev/null \
  -H 'Origin: http://localhost:5173' \
  -H 'Range: bytes=0-15' \
  http://localhost:9000/disk-images/large.bin
```

Expected:

- `HTTP/1.1 206 Partial Content`
- `Content-Range: bytes 0-15/<full-size>`

### Via proxy (optional)

Start the proxy:

```bash
docker compose --profile proxy up
```

Then:

```bash
curl -s -D - -o /dev/null \
  -H 'Range: bytes=0-15' \
  http://localhost:9002/disk-images/large.bin
```

## Reproduce browser preflight behavior (CORS + Range)

Browsers typically preflight a CORS request when you send a non-simple header like `Range`.

### Preflight against MinIO origin

```bash
curl -i -X OPTIONS \
  -H 'Origin: http://localhost:5173' \
  -H 'Access-Control-Request-Method: GET' \
  -H 'Access-Control-Request-Headers: range' \
  http://localhost:9000/disk-images/large.bin
```

### Preflight against the proxy (optional)

```bash
curl -i -X OPTIONS \
  -H 'Origin: http://localhost:5173' \
  -H 'Access-Control-Request-Method: GET' \
  -H 'Access-Control-Request-Headers: range' \
  http://localhost:9002/disk-images/large.bin
```

For actual `GET` responses, ensure you can see (and access from JS) these headers:

- `Access-Control-Allow-Origin` (matches your app origin)
- `Access-Control-Expose-Headers: ... Content-Range ...`

## Notes: MinIO vs AWS S3

- **Addressing style:** MinIO commonly uses *path-style* URLs (`/bucket/key`). AWS S3 increasingly prefers *virtual-hosted-style* (`bucket.s3.amazonaws.com/key`).
- **CORS configuration:** AWS S3 CORS is configured per-bucket. MinIO’s CORS behavior is configured at the API layer (this compose setup wires `CORS_ALLOWED_ORIGIN` into `MINIO_API_CORS_ALLOW_ORIGIN`).
- **Auth:** This compose setup makes the bucket **public-read** (`mc anonymous set download`) so that browser/curl tests don’t require request signing. Production buckets should generally require auth and/or be fronted by a CDN.
- **Proxy/CDN behavior:** Real CDNs (e.g. CloudFront) can:
  - Handle/terminate `OPTIONS` at the edge
  - Add/remove CORS headers
  - Cache (and sometimes break) `Range` responses depending on configuration
