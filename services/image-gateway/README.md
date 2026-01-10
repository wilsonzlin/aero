# image-gateway (reference implementation)

`image-gateway` is a small backend service that enables **secure browser streaming of large disk images** stored in S3 (or S3-compatible) via **HTTP Range**.

The intended production path is:

1. Client uploads the disk image to S3 using **multipart upload** with presigned `PUT` URLs.
2. Client requests a **stable** CloudFront URL for the image (no query-string signing) plus **CloudFront signed-cookie auth material**.
3. Browser streams the image directly from CloudFront using `Range` requests (no proxying all bytes through the app).

This service implements a minimal, swappable auth + owner model (dev stub) and stores image records **in memory** (reference only).

API documentation: see [`openapi.yaml`](./openapi.yaml).

## Setup

```bash
cd services/image-gateway
npm install
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
- `CORS_ALLOW_ORIGIN` (default `*`, used by the range-proxy fallback endpoint)
- `MULTIPART_PART_SIZE_BYTES` (default `67108864` / 64MiB; must be 5MiB–5GiB)

### Local MinIO (optional)

For local development without AWS, you can run MinIO:

```bash
docker compose -f docker-compose.minio.yml up
```

Then set:

- `S3_ENDPOINT=http://127.0.0.1:9000`
- `S3_FORCE_PATH_STYLE=true`
- `S3_BUCKET=aero-images` (or change the bucket name in `.env` and in `docker-compose.minio.yml`)
- `AWS_ACCESS_KEY_ID=minioadmin`
- `AWS_SECRET_ACCESS_KEY=minioadmin`

### Run

```bash
npm run dev
```

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

The endpoint includes CORS headers and supports `OPTIONS` preflight. Set `CORS_ALLOW_ORIGIN` as needed.

## Notes for browser `StreamingDisk`

The browser side should:

1. Call `GET /v1/images/:imageId/stream-url` once to obtain `url` and apply `auth` (cookies or signed URL).
2. Use `fetch(url, { headers: { Range: "bytes=start-end" } })` for chunk reads.
3. Prefer `ETag` + `Content-Range` to validate ranges.

CloudFront must be configured to:

- allow `Range` requests (forward the `Range` header to S3)
- return `206 Partial Content` responses for ranged reads
