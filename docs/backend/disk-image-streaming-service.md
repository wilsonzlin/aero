# Disk Image Streaming Service (Runbook)

This document covers operational concerns for the **disk image streaming service**: how it is used by the browser client, the required HTTP/CORS headers for `Range` support, how to deploy it behind a reverse proxy, and how to troubleshoot the most common failures (especially around cross-origin isolation).

## Purpose

The disk image streaming service exists to make multi‑GB disk images usable in the browser without downloading them up front.

## Reference implementation in this repo

This repository includes a minimal reference service at [`services/image-gateway/`](../../services/image-gateway/) that implements:

- S3 multipart uploads (presigned `UploadPart` URLs)
- immutable/versioned object keys for stable CDN URLs
- CloudFront signed cookies (preferred) or signed URLs for viewer authorization
- a local-dev `Range` proxy fallback endpoint

The exact paths differ from the “planned” section below (which focuses on a simplified pure-streaming service),
but the HTTP semantics and header requirements are the same.

On the client side, the storage subsystem uses `StreamingDisk` (see [05 - Storage Subsystem](../05-storage-subsystem.md)) to:

1. Read sectors from a *virtual* disk.
2. Lazily fetch missing byte ranges from a remote image over HTTP `Range`.
3. Cache fetched chunks locally (e.g. OPFS + sparse format).

This allows the emulator to boot quickly and only download the parts of the OS/image that are actually accessed.

For the hosted-service model (user uploads, ownership/visibility, lease scopes, and writeback options), see: [Disk Image Lifecycle and Access Control](../17-disk-image-lifecycle-and-access-control.md).

For the normative protocol/auth/CORS contract of the disk bytes endpoint, see: [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](../16-disk-image-streaming-auth.md).

## How it interacts with `StreamingDisk`

`StreamingDisk` issues HTTP requests like:

```http
GET /disks/disk_123/bytes HTTP/1.1
Host: disks.examplecdn.com
Range: bytes=1048576-2097151
Origin: https://app.example.com
```

The streaming service must:

- Return **`206 Partial Content`** for satisfiable ranges.
- Include **`Content-Range`** and **`Accept-Ranges`** headers.
- Support **CORS preflight** (because `Range` is not a CORS-safelisted header).
- Expose required response headers to the browser (so `StreamingDisk` can discover total size / validate responses).

## Endpoint summary

These endpoints are recommended for operability. Exact paths may vary, but the semantics should be consistent.

### Health / readiness / metrics

| Endpoint | Method | Purpose | Notes |
|---|---:|---|---|
| `/healthz` | GET | Liveness | “Process is up”; should not check external deps. |
| `/readyz` | GET | Readiness | “Can serve images”; should check storage backend connectivity (filesystem/S3/etc). |
| `/metrics` | GET | Prometheus metrics | Protect this endpoint (network policy or auth). |

### Control-plane: image catalog + metadata (implemented)

| Endpoint | Method | Purpose | Notes |
|---|---:|---|---|
| `/v1/images` | GET | List available disk images (JSON) | Includes size/etag/last_modified; response uses `Cache-Control: no-cache`. |
| `/v1/images/{image_id}/meta` | GET | Fetch metadata for one image (JSON) | Same fields as the list response. |

Implementation note:

- Treat `image_id` as an opaque identifier and keep it **bounded** for operability and DoS safety
  (the reference `aero-storage-server` implementation enforces **`[A-Za-z0-9._-]{1,128}`**).

Metadata response fields:

- `id`, `name`, `description` (optional)
- `size_bytes`, `etag`, `last_modified`, `content_type`
- `recommended_chunk_size_bytes` (optional)
- `public` (boolean)

### Image streaming (planned)

| Endpoint | Method | Purpose | Notes |
|---|---:|---|---|
| `/disks/{disk_id}/bytes` | GET | Stream raw disk bytes | Must support `Range`. (Some deployments route this as `/disk/{id}` or `/v1/images/{id}`.) |
| `/disks/{disk_id}/bytes` | HEAD | Fetch metadata headers only | Useful for verifying `Content-Length`/`ETag`. |
| `/disks/{disk_id}/bytes` | OPTIONS | CORS preflight | Must include `Access-Control-*` headers. |
| `/v1/images/{image_id}/meta` | GET | JSON metadata (control-plane) | Image discovery + `size_bytes`/`etag` prior to `Range` reads. |

## Configuration

The service should be configurable enough to support multiple storage backends and safe production deployments. At minimum, plan for configuration knobs in these areas:

- **Network**
  - Listen address/port (and optional base path/prefix if mounted behind a proxy).
  - TLS termination strategy (direct TLS vs behind a reverse proxy).
- **Image store**
  - Filesystem: root directory containing image files.
  - Object storage: bucket name + prefix, credentials, and region/endpoint.
  - Optional integrity checks (checksums, signed manifests).
- **Image identification**
  - Map a stable `image_id` (e.g. `win7-sp1-x64`) to an underlying object key/path.
  - Avoid exposing raw filesystem paths to callers (see security guidance).
  - Keep `image_id` values bounded and restricted to safe characters to avoid
    path traversal and to prevent log/metrics amplification attacks.
- **CORS**
  - Allowlist of app origins allowed to fetch images (or `*` only for non-credentialed public images).
  - Whether to include `Access-Control-Allow-Credentials`.
  - `Access-Control-Max-Age` for caching preflight responses.
- **Range policy**
  - Maximum allowed range length (protects against huge single requests).
  - Maximum concurrent requests per client/token.
  - Whether to support only single-range requests (recommended).
- **AuthN/AuthZ**
  - Signed URL validation, bearer token/JWT verification, or mTLS requirements.
  - Per-image authorization policy (who can fetch which `image_id`).
- **Observability**
  - Access logging (include method/path/status and `Range`).
  - Metrics (bytes served, request counts, 206/416 rates, error rates).

## HTTP requirements for browser compatibility

### Range support (the “206 contract”)

The service must implement **single-range** requests with the `bytes` unit.

Minimum behavior:

- **Requests:**
  - `Range: bytes=start-end` (inclusive, 0-indexed)
  - `Range: bytes=start-` (open-ended) is strongly recommended.
- **Responses (success):**
  - Status: `206 Partial Content`
  - `Accept-Ranges: bytes`
  - `Content-Range: bytes start-end/total_size`
  - `Content-Length: end-start+1`
- **Responses (unsatisfiable):**
  - Status: `416 Range Not Satisfiable`
  - `Content-Range: bytes */total_size`

Notes:

- Avoid content transformations. **Disable compression** (see below). Byte ranges are defined over the *wire representation*; a `Content-Encoding: gzip` response makes “disk byte offsets” meaningless.
- If using caching/CDNs, ensure `Range` is forwarded and not normalized away.

### HTTP caching (ETag / Last-Modified / Cache-Control)

To make `StreamingDisk` range reads cache-friendly (browser cache + CDN) while remaining correct:

- **Always include validators when available**
  - `ETag` (strongly recommended)
  - `Last-Modified` (when the underlying image has a meaningful mtime)
- **Return `304 Not Modified`** for conditional `GET`/`HEAD` where applicable.
- **Set explicit `Cache-Control`**
  - Metadata endpoints should be revalidated (`no-cache`) rather than cached blindly.
  - Raw bytes can be cached aggressively for public/immutable images, but must not be cached for
    authenticated/private responses.
- Ensure cache-aware responses include the correct `Vary` headers (at minimum `Vary: Origin` if the
  service varies CORS responses by `Origin`).

Recommended policy:

- **Metadata** (`/v1/images`, `/v1/images/{image_id}/meta`)
  - `Cache-Control: no-cache`
  - Always send `ETag`
  - Support `If-None-Match` on `GET`/`HEAD` (return `304` when matched)
- **Data** (`/disks/{disk_id}/bytes`)
  - Send `ETag` and `Last-Modified` when available
  - `Accept-Ranges: bytes`
  - Public images: `Cache-Control: public, max-age=<n>, no-transform` (max-age configurable)
  - Authenticated/private images: `Cache-Control: private, no-store, no-transform`
  - `HEAD` should support `If-None-Match` / `If-Modified-Since` and return `304` when matched.
  - If implementing `If-Range`, follow RFC 9110: if the validator doesn't match the current image
    version, ignore the `Range` header and return a full `200` response (to avoid mixed-version
    bytes).

### Required CORS headers (including `Range`)

Because browsers preflight cross-origin `Range` requests, you must support `OPTIONS` and include the correct headers on both the preflight and the actual response.

Recommended **preflight response** headers:

```http
HTTP/1.1 204 No Content
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
Access-Control-Allow-Headers: Range, If-Range, If-None-Match, If-Modified-Since, Authorization
Access-Control-Max-Age: 600
Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers
```

Recommended **GET/HEAD response** headers:

```http
Access-Control-Allow-Origin: https://app.example.com
Access-Control-Expose-Headers: Accept-Ranges, Content-Range, Content-Length, ETag
Vary: Origin
Accept-Ranges: bytes
```

Important details:

- `Content-Range`, `Accept-Ranges`, and `Content-Length` are **not** CORS-safelisted response headers. If they are not listed in `Access-Control-Expose-Headers`, the fetch may succeed but the browser will hide these headers from JS.
- Request headers like `If-None-Match` and `If-Modified-Since` are **not** CORS-safelisted. If you use conditional requests (for metadata revalidation or full-body reads), ensure your preflight response allows them.
- If you need cookies or other credentials, you must **not** use `Access-Control-Allow-Origin: *`; instead echo a specific origin and also include `Access-Control-Allow-Credentials: true`.

### Cross-origin isolation: COOP/COEP vs CORS/CORP

To use `SharedArrayBuffer` (WASM threads), the **app origin** must be cross-origin isolated. This is usually done with:

- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp` (or `credentialless`)

However, cross-origin isolation changes how the browser treats **cross-origin subresources**, including disk image fetches.

Recommended deployment model:

- **App origin** (HTML/JS/WASM):
  - Serve the emulator UI here.
  - Set COOP/COEP on the HTML (and ensure the document is eligible for cross-origin isolation).
- **Image origin** (disk images):
  - Serve raw images here.
  - Enable CORS for the app origin.
  - Consider setting `Cross-Origin-Resource-Policy` to explicitly allow the app to load the resource:
    - `Cross-Origin-Resource-Policy: same-site` if `app.example.com` and `images.example.com` share the same “site”.
    - `Cross-Origin-Resource-Policy: cross-origin` if the image host must be embeddable from multiple sites.

If you see COEP-related console errors, it almost always means the image origin is missing CORS/CORP (see troubleshooting).

## Recommended reverse proxy settings

Even if the service itself is correct, reverse proxies and CDNs commonly break `Range` in subtle ways.

General recommendations:

- **Prefer HTTP/2** end-to-end (browser → edge → service) for multiplexing many small range requests.
- **Increase timeouts** on the image route (range reads are small but can be numerous).
- **Disable compression** on image routes (to preserve byte offsets).
- Avoid buffering huge responses in memory; pass through streaming responses.

### Example: NGINX (proxying to the service)

```nginx
# Disk images are large and use byte ranges; avoid transformations.
location /disks/ {
  proxy_pass http://disk-image-service;

  # Keep range semantics intact.
  gzip off;
  proxy_buffering off;

  # Timeouts appropriate for large downloads / slow networks.
  proxy_read_timeout 3600s;
  proxy_send_timeout 3600s;
  send_timeout 3600s;

  # CORS preflight must reach the service or be handled consistently here.
  # (If handled here, make sure headers match the backend.)
}
```

If serving images directly from NGINX (no upstream), NGINX supports `Range` for static files by default; still ensure `gzip` is disabled for these locations.

### CDN notes (CloudFront / Cloudflare / etc.)

If a CDN is in front:

- Ensure it forwards `Range` and does not collapse multiple `Range` requests into a cached 200.
- Ensure CORS response headers are preserved.
- Avoid “automatic compression” features on binary routes.
- See [17 - HTTP Range + CDN Behavior](../17-range-cdn-behavior.md) for CloudFront/Cloudflare limits and an operator validation checklist.

## Security recommendations

Disk images are large, valuable, and easy to abuse. Treat this service as an internet-facing file server unless proven otherwise.

### Authentication and authorization

Recommended approaches (pick one):

- **Signed URLs** (recommended for public distribution): time-bound, scoped to a single `image_id`.
- **Bearer token/JWT**: validate and authorize per image.
- **mTLS**: for internal deployments (cluster-to-cluster).

Make authorization decisions on a stable identifier (`image_id`), not a filesystem path.

### Least privilege

- Run the service as a non-root user.
- Grant read-only access to the image store.
- If using S3/GCS/etc., use credentials that can *only* read the required bucket/prefix.
- Keep `/metrics` and admin endpoints behind auth or internal networking.

### Rate limiting and DoS hardening

- Enforce a maximum concurrent requests per IP/token.
- Enforce a maximum bytes/sec per IP/token if serving publicly.
- Enforce a maximum `Range` length (e.g. 1–8 MiB) to align with the client’s chunking strategy.

### Image path hardening

If images are on disk:

- Do not accept arbitrary paths from the request.
- Store an allowlist mapping `{image_id -> absolute_path}` in config.
- Reject `..`, `%2e%2e`, and other traversal patterns if paths are ever user-influenced.
- Avoid following symlinks out of the image root.

## LocalFS catalog (`manifest.json`)

For `LocalFS` mode, the image catalog can be driven by a `manifest.json` file stored alongside the
image files. This provides deterministic IDs and friendly names.

Example:

```json
{
  "images": [
    {
      "id": "win7",
      "file": "win7.img",
      "name": "Windows 7 SP1",
      "description": "Clean install",
      "public": true,
      "etag": "\"win7-sp1-x64-v1\"",
      "last_modified": "2026-01-10T00:00:00Z",
      "recommended_chunk_size_bytes": 1048576,
      "content_type": "application/octet-stream"
    }
  ]
}
```

`etag` and `last_modified` are optional. When provided, they override the server’s default
filesystem-derived validators, allowing stable caching for immutable/versioned images even if file
mtimes change during copy/restore.

Notes:

- `etag` must be a valid HTTP **entity-tag**, including quotes (e.g. `"v1"` or `W/"v1"`). Prefer a
  **strong** ETag (no `W/`) so clients can use `If-Range` for safe range resumption.
- `last_modified` must be an RFC3339 timestamp (e.g. `2026-01-10T00:00:00Z`) and must be at or
  after `1970-01-01T00:00:00Z` (pre-epoch times cannot be represented in an HTTP `Last-Modified`
  header).

If no manifest is present, the server may fall back to a stable directory listing (development
only).

## Troubleshooting

### “Why am I getting 200 OK instead of 206 Partial Content?”

Symptoms:

- Network tab shows `200` for requests that include a `Range` header.
- The browser downloads the entire disk image.
- `StreamingDisk` behaves as if caching is ineffective.

Most common causes:

1. The origin server does not implement `Range` (returns 200 and ignores it).
2. A reverse proxy/CDN strips the `Range` header.
3. Compression or other transformations are enabled (`Content-Encoding`).

How to debug:

```bash
# Does the service return 206 for a trivial range?
curl -v -H 'Range: bytes=0-0' https://disks.examplecdn.com/disks/disk_123/bytes

# Check key headers (you want 206 + Content-Range).
curl -I -H 'Range: bytes=0-0' https://disks.examplecdn.com/disks/disk_123/bytes
```

Expected (example):

```http
HTTP/2 206
accept-ranges: bytes
content-range: bytes 0-0/34359738368
content-length: 1
```

### CORS preflight failures for `Range`

Symptoms:

- The GET never happens; you only see an `OPTIONS` request.
- Browser console: CORS errors mentioning `Range` or “Request header field range is not allowed…”.

How to debug preflight:

```bash
curl -i -X OPTIONS \
  -H 'Origin: https://app.example.com' \
  -H 'Access-Control-Request-Method: GET' \
  -H 'Access-Control-Request-Headers: range' \
  https://disks.examplecdn.com/disks/disk_123/bytes
```

The response must include:

- `Access-Control-Allow-Origin` matching the requesting origin
- `Access-Control-Allow-Methods` including `GET`
- `Access-Control-Allow-Headers` including `Range` (case-insensitive)

### “Blocked by Cross-Origin-Embedder-Policy” (COEP) errors

Symptoms:

- Browser console errors like:
  - `Blocked by Cross-Origin-Embedder-Policy: ...`
  - `Cross-Origin-Embedder-Policy policy would block the resource ...`
- `SharedArrayBuffer` becomes unavailable or the page is not cross-origin isolated.

Common causes:

1. App origin has `COOP: same-origin` + `COEP: require-corp`, but the image response is missing CORS headers.
2. The image response has restrictive `Cross-Origin-Resource-Policy` (e.g. `same-origin`) that does not allow the app’s site.
3. Mixed content: app is HTTPS but image is HTTP.

How to debug:

- Confirm the app document response includes COOP/COEP.
- Confirm the image response includes **either**:
  - valid CORS headers **or**
  - a CORP header compatible with the app’s site (`same-site`/`cross-origin`).
