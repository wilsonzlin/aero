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

Default CORP policy (proxy only):

- `CROSS_ORIGIN_RESOURCE_POLICY=cross-origin`

## Start / stop

From this directory:

```bash
docker compose up
```

From the repo root (using `just`):

```bash
just object-store-up
```

You can override defaults by exporting environment variables before starting (or by creating a `.env` file in this directory), for example:

```bash
export CORS_ALLOWED_ORIGIN=http://localhost:3000
docker compose up
```

There is an `.env.example` in this directory that you can copy to `.env` to get started.

If you change `MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD`, also reset volumes so the persisted `mc` config is regenerated:

```bash
docker compose --profile proxy down -v
```

Stop and remove containers (keeps volumes by default):

```bash
docker compose --profile proxy down
```

To also remove persisted data:

```bash
docker compose --profile proxy down -v
```

### Optional proxy (“CDN”) layer

Enable the proxy container with a Compose profile:

```bash
docker compose --profile proxy up
```

Or from the repo root:

```bash
just object-store-up-proxy
```

The proxy is useful for reproducing “edge” behaviors (for example, overriding CORS headers and handling preflights at the proxy instead of the origin).

It also injects a `Cross-Origin-Resource-Policy` (CORP) header (configurable via `CROSS_ORIGIN_RESOURCE_POLICY`) to make it easier to test disk streaming under `Cross-Origin-Embedder-Policy: require-corp`.

For caching correctness, the proxy also sets:

- `Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers`

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

### Automated smoke check (origin + proxy)

This directory includes a small script that will:

- start the containers
- upload a small random file
- (by default) upload it to `disk-images/_smoke/range-test.bin` (overwriting on each run)
- verify `HEAD`, `206` Range responses, and preflight behavior against both the origin and the proxy

```bash
bash ./verify.sh
```

Or from the repo root:

```bash
just object-store-verify
```

To stop containers at the end:

```bash
bash ./verify.sh --down
```

### Verify HEAD / size discovery

The streaming disk client typically starts with a `HEAD` request to discover the object size (`Content-Length`).

```bash
curl -s -D - -o /dev/null -I \
  http://localhost:9000/disk-images/large.bin
```

Look for:

- `HTTP/1.1 200 OK`
- `Content-Length: ...`
- `Accept-Ranges: bytes`

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

## Benchmark Range throughput (optional)

This repo also includes a small Node-based Range harness for benchmarking chunked reads:

```bash
# Direct to MinIO origin:
node tools/range-harness/index.js \
  --url "http://localhost:9000/disk-images/large.bin" \
  --chunk-size 1048576 --count 32 --concurrency 4 --random

# Via the optional proxy (“edge”):
node tools/range-harness/index.js \
  --url "http://localhost:9002/disk-images/large.bin" \
  --chunk-size 1048576 --count 32 --concurrency 4 --random
```

Note: MinIO/Nginx may not emit a CDN-style `X-Cache` header; in that case the harness still provides latency/throughput metrics.

Browsers typically preflight a CORS request when you send a non-simple header like `Range`.

### Browser console snippet (shows actual preflight)

From a page served at your configured origin (default `http://localhost:5173`), run:

```js
const url = "http://localhost:9002/disk-images/large.bin"; // proxy (recommended)
// const url = "http://localhost:9000/disk-images/large.bin"; // origin

const res = await fetch(url, { headers: { Range: "bytes=0-15" } });
console.log("status", res.status);
console.log("content-range", res.headers.get("content-range"));
console.log("bytes", new Uint8Array(await res.arrayBuffer()));
```

In DevTools → Network you should see an `OPTIONS` preflight followed by a `GET`, and `content-range` should be readable in JS.

### Preflight against MinIO origin

```bash
curl -i -X OPTIONS \
  -H 'Origin: http://localhost:5173' \
  -H 'Access-Control-Request-Method: GET' \
  -H 'Access-Control-Request-Headers: range, if-range, if-none-match, if-modified-since' \
  http://localhost:9000/disk-images/large.bin
```

### Preflight against the proxy (optional)

```bash
curl -i -X OPTIONS \
  -H 'Origin: http://localhost:5173' \
  -H 'Access-Control-Request-Method: GET' \
  -H 'Access-Control-Request-Headers: range, if-range, if-none-match, if-modified-since' \
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

## Publish a chunked disk image (no HTTP Range)

This repo also supports serving disk images without HTTP `Range` by publishing the image as many fixed-size chunk objects plus a `manifest.json` (see [`docs/18-chunked-disk-image-format.md`](../../docs/18-chunked-disk-image-format.md)).

### Publish with `aero-image-chunker`

From the repo root:

```bash
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin

# Create a sample file (5 MiB).
dd if=/dev/urandom of=./scratch.img bs=1M count=5

# Default chunk size is 4 MiB (4194304). Pass --chunk-size to override.
cargo run --locked --manifest-path tools/image-chunker/Cargo.toml -- publish \
  --file ./scratch.img \
  --bucket disk-images \
  --prefix images/demo/sha256-test/ \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1 \
  --concurrency 4
```

Then verify the published manifest + chunks end-to-end:

```bash
cargo run --locked --manifest-path tools/image-chunker/Cargo.toml -- verify \
  --bucket disk-images \
  --prefix images/demo/sha256-test/ \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1 \
  --concurrency 4
```

### Verify with `curl`

This compose setup makes the bucket anonymously readable, so you can verify without signing:

```bash
curl -fSs http://localhost:9000/disk-images/images/demo/sha256-test/manifest.json | head
curl -fSsI http://localhost:9000/disk-images/images/demo/sha256-test/chunks/00000000.bin
```
