# image-gateway (reference implementation)

`image-gateway` is a small backend service that enables **secure browser streaming of large disk images** stored in S3 (or S3-compatible) via **HTTP Range**.

Related docs:
- [Disk Image Lifecycle and Access Control](../../docs/17-disk-image-lifecycle-and-access-control.md) (hosted uploads/ownership/sharing/writeback)
- [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](../../docs/16-disk-image-streaming-auth.md) (normative disk-bytes endpoint behavior)
- [Chunked Disk Image Format (no Range)](../../docs/18-chunked-disk-image-format.md) (manifest + chunk objects; avoids CORS preflight)
- [deployment/cloudfront-disk-streaming.md](../../docs/deployment/cloudfront-disk-streaming.md) (concrete CloudFront setup)

The intended production path is:

1. Client uploads the disk image to S3 using **multipart upload** with presigned `PUT` URLs.
2. Client requests a **stable** CloudFront URL for the image (no query-string signing) plus **CloudFront signed-cookie auth material**.
3. Browser streams the image directly from CloudFront using `Range` requests (no proxying all bytes through the app).

This service implements a minimal, swappable auth + owner model (dev stub) and stores image records **in memory** (reference only).

API documentation: see [`openapi.yaml`](./openapi.yaml).

## Disk object headers (CloudFront/S3 fast path)

In production, the high-throughput path is **CloudFront → S3** (the client streams bytes directly from the CDN).
That means response headers do **not** come from `GET /v1/images/:id/range` (the proxy fallback); they come from the
S3 object metadata that was set when the object was created.

When starting the multipart upload, `image-gateway` sets these S3 object headers:

- `Content-Type: application/octet-stream`
- `Cache-Control: …, no-transform` (see `IMAGE_CACHE_CONTROL` below)
- `Content-Encoding: identity`

These are required defence-in-depth to prevent CDNs/intermediaries from applying transforms (especially compression)
that would make disk `Range` offsets meaningless.

This only affects **newly created** objects; existing S3 objects will keep whatever metadata they currently have.

### CloudFront “Compress objects automatically”

For disk images, CloudFront compression must be **disabled**. If “Compress objects automatically” is enabled for the
disk cache behavior, CloudFront may return a `Content-Encoding` other than `identity`, breaking deterministic ranges.

## Setup

```bash
# From the repo root (npm workspaces)
npm ci
```

Copy `.env.example` to `.env` and fill in values:

```bash
cp .env.example .env
```

### Environment variables

Required:

- `S3_BUCKET`
- `AWS_REGION`

Credentials:

The AWS SDK uses the standard default credential provider chain (env vars, shared config files, instance roles, etc).
For local MinIO, set:

- `AWS_ACCESS_KEY_ID`
- `AWS_SECRET_ACCESS_KEY`

Optional (S3-compatible / MinIO):

- `S3_ENDPOINT` (e.g. `http://127.0.0.1:9000`)
- `S3_FORCE_PATH_STYLE=true|false` (often `true` for MinIO)

CloudFront (required for `/v1/images/:id/stream-url` in `cookie`/`url` mode):

- `CLOUDFRONT_DOMAIN` (e.g. `dxxxxx.cloudfront.net` or `images.example.com`)
- `CLOUDFRONT_KEY_PAIR_ID`
- `CLOUDFRONT_PRIVATE_KEY_PEM` (either the PEM string **or** a filesystem path to a PEM file)

Service:

- `IMAGE_BASE_PATH` (default `/images`)
- `AUTH_MODE=dev|none` (default `dev`)
- `PORT` (default `3000`)

Useful optional knobs:

- `CLOUDFRONT_AUTH_MODE=cookie|url` (default `cookie`)
- `CLOUDFRONT_COOKIE_DOMAIN` (optional; e.g. `.example.com` when API runs on `api.example.com` and CloudFront on `images.example.com`)
- `CLOUDFRONT_COOKIE_SAMESITE=None|Lax|Strict` (default `None`; use `Lax`/`Strict` when streaming is same-site and you don't need third-party cookies)
- `CLOUDFRONT_COOKIE_PARTITIONED=true|false` (default `false`; adds the `Partitioned` attribute for CHIPS-capable browsers, requires `CLOUDFRONT_COOKIE_SAMESITE=None`)
- `CORS_ALLOW_ORIGIN` (default `*`, used for browser CORS; set an explicit origin if you need credentialed requests / cookie-based auth)
- `CROSS_ORIGIN_RESOURCE_POLICY` (default `same-site`, sent on the range-proxy responses as defence-in-depth for COEP; see `docs/16-disk-image-streaming-auth.md`)
- `MULTIPART_PART_SIZE_BYTES` (default `67108864` / 64MiB; must be 5MiB–5GiB)
- `IMAGE_CACHE_CONTROL=private-no-store|public-immutable` (default `private-no-store`)
  - `private-no-store` sets `Cache-Control: private, no-store, no-transform` (safe for private images)
  - `public-immutable` sets `Cache-Control: public, max-age=31536000, immutable, no-transform` (only safe when keys are immutable/versioned and access control is enforced elsewhere, e.g. signed CloudFront URL/cookie)

### Local MinIO (optional)

For local development without AWS, you can run MinIO:

```bash
docker compose -f docker-compose.minio.yml up
```

Alternatively, if you just want a general-purpose local S3-compatible object store for Range + CORS testing,
the repo also provides [`infra/local-object-store/`](../../infra/local-object-store/README.md).

That setup includes an optional nginx “edge” proxy and a `verify.sh` smoke test. If you use it with `image-gateway`,
set `BUCKET_NAME` to match your `S3_BUCKET` (defaults differ), and note it configures the bucket for anonymous download
by default to simplify browser/curl testing.

Then set:

- `S3_ENDPOINT=http://127.0.0.1:9000`
- `S3_FORCE_PATH_STYLE=true`
- `S3_BUCKET=aero-images` (or change the bucket name in `.env` and in `docker-compose.minio.yml`)
- `AWS_ACCESS_KEY_ID=minioadmin`
- `AWS_SECRET_ACCESS_KEY=minioadmin`

### Run

```bash
npm -w services/image-gateway run dev
```

Health endpoints:

- `GET /health` / `GET /healthz` (liveness)
- `GET /readyz` (checks S3 bucket reachability)

## Multipart upload flow (curl)

These are illustrative; real clients should upload parts from the browser using `File.slice()` and `fetch()` / `PUT`.

Assuming `AUTH_MODE=dev`:

```bash
export USER_ID=dev-user
export API=http://localhost:3000
```

### 1) Create an image + start multipart upload

```bash
curl -sS -X POST "$API/v1/images" \
  -H "X-User-Id: $USER_ID" | jq
```

Response:

```json
{ "imageId": "...", "uploadId": "...", "partSize": 67108864 }
```

### 2) Request an upload URL for a part

```bash
curl -sS -X POST "$API/v1/images/<imageId>/upload-url" \
  -H "X-User-Id: $USER_ID" \
  -H "content-type: application/json" \
  -d '{"uploadId":"<uploadId>","partNumber":1}' | jq -r .url
```

### 3) Upload the part to S3

```bash
UPLOAD_URL="$(curl -sS -X POST "$API/v1/images/<imageId>/upload-url" \
  -H "X-User-Id: $USER_ID" \
  -H "content-type: application/json" \
  -d '{"uploadId":"<uploadId>","partNumber":1}' | jq -r .url)"

# Example: upload first 64MiB from disk.img
dd if=disk.img of=part1.bin bs=1m count=64

curl -i -X PUT --upload-file part1.bin "$UPLOAD_URL"
# Capture the `ETag` response header for completion.
```

### 4) Complete multipart upload

```bash
curl -sS -X POST "$API/v1/images/<imageId>/complete" \
  -H "X-User-Id: $USER_ID" \
  -H "content-type: application/json" \
  -d '{"uploadId":"<uploadId>","parts":[{"partNumber":1,"etag":"\"<etag-from-put>\""}]}' | jq
```

## Getting a stream URL (CloudFront)

```bash
curl -i -sS "$API/v1/images/<imageId>/stream-url" \
  -H "X-User-Id: $USER_ID"
```

If `CLOUDFRONT_AUTH_MODE=cookie`, the response includes:

- a stable `url` (no query string)
- `Set-Cookie` headers (CloudFront signed cookies)
- the same cookie strings in JSON under `auth.cookies`

The browser can then issue `Range` requests to `url` and CloudFront will authorize using the signed cookies.

If your API host cannot set cookies for the CloudFront domain (common when using the default `*.cloudfront.net` domain),
set `CLOUDFRONT_AUTH_MODE=url` to return a signed URL instead.

## Range proxy fallback (local dev)

`GET /v1/images/:imageId/range` streams bytes from S3 using `GetObject` + `Range`.

This is useful when you don't have CloudFront locally, but it proxies all bytes through the app (not recommended for production).

The service includes CORS headers and supports `OPTIONS` preflight for browser use. Set `CORS_ALLOW_ORIGIN` as needed.

`HEAD /v1/images/:imageId/range` is also supported for size discovery (mirrors what `StreamingDisk` does against the CloudFront URL).

Note: only **single-range** requests are supported (no `Range: bytes=a-b,c-d`).

## Notes for browser `StreamingDisk`

The browser side should:

1. Call `GET /v1/images/:imageId/stream-url` once to obtain `url` and apply `auth` (cookies or signed URL).
2. Use `fetch(url, { headers: { Range: "bytes=start-end" } })` for chunk reads.
3. Prefer `ETag` + `Content-Range` to validate ranges.

CloudFront must be configured to:

- allow `Range` requests (forward the `Range` header to S3)
- return `206 Partial Content` responses for ranged reads

## Chunked disk image delivery (no `Range`)

In addition to `Range` streaming, `image-gateway` can serve disk images in a **chunked** format:

- `manifest.json` + `chunks/00000000.bin`, `chunks/00000001.bin`, ...
- Plain `GET` requests only (no `Range` header), which avoids CORS preflight for cross-origin deployments.

See [`docs/18-chunked-disk-image-format.md`](../../docs/18-chunked-disk-image-format.md) and the publisher CLI at
[`tools/image-chunker/`](../../tools/image-chunker/README.md).

Endpoints:

- `GET/HEAD /v1/images/:imageId/chunked/manifest`
- `GET/HEAD /v1/images/:imageId/chunked/chunks/:chunkIndex` (`:chunkIndex` can be `42` or `00000042.bin`)

If CloudFront is configured, these endpoints redirect to CloudFront (stable URLs for cookie mode; signed URLs for url mode).

`GET /v1/images/:imageId/stream-url` may include a `chunked` section:

```json
{
  "chunked": {
    "delivery": "chunked",
    "manifestUrl": "..."
  }
}
```

Notes:

- In `CLOUDFRONT_AUTH_MODE=cookie`, `manifestUrl` points directly at the CloudFront URL for `manifest.json`.
- In `CLOUDFRONT_AUTH_MODE=url`, `manifestUrl` points at the gateway endpoint (`/v1/images/:id/chunked/manifest`) so that
  chunk URLs resolved relative to the manifest (e.g. `new URL("chunks/00000000.bin", manifestUrl)`) also hit the gateway,
  which then redirects each request to a per-object signed CloudFront URL. This avoids needing query-string auth to propagate
  through relative URL resolution.
- disable compression / transformations (no `Content-Encoding` other than `identity`)
- include the streaming-safe headers described in `docs/16-disk-image-streaming-auth.md`:
  - `Cache-Control: no-transform`
  - `Content-Type: application/octet-stream`
  - `X-Content-Type-Options: nosniff`
  - `Cross-Origin-Resource-Policy: same-site` (or `cross-origin` depending on deployment)
  - CORS headers (if the app is cross-origin to the disk URL)

See `docs/deployment/cloudfront-disk-streaming.md` for a concrete CloudFront response headers policy.
