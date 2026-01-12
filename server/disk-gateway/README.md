# disk-gateway (reference)

An authenticated, HTTP Range-capable disk-image gateway intended for the browser `StreamingDisk` design.

This server is a **reference implementation** for deployments that need:

- Efficient random-access reads via `Range: bytes=start-end` (single + multipart multi-range).
- Public and private images on local filesystem.
- Short-lived signed “disk access lease” tokens (JWT HS256).
- Correct CORS preflights (including `Range` + `Authorization`).
- `Cross-Origin-Resource-Policy` headers for COEP/CORP compatibility.

## Running locally

```bash
cd server/disk-gateway

export DISK_GATEWAY_BIND=127.0.0.1:3000
export DISK_GATEWAY_PUBLIC_DIR=./public-images
export DISK_GATEWAY_PRIVATE_DIR=./private-images
export DISK_GATEWAY_TOKEN_SECRET='dev-secret-change-me'

# CORS allowlist (comma-separated). Use "*" to allow any Origin (no credentials).
export DISK_GATEWAY_CORS_ALLOWED_ORIGINS='http://localhost:5173'

# CORP policy for /disk/* responses: "same-site" (default) or "cross-origin"
export DISK_GATEWAY_CORP='same-site'

# Multi-range abuse guards (defaults shown).
export DISK_GATEWAY_MAX_RANGES=16
export DISK_GATEWAY_MAX_TOTAL_BYTES=$((512 * 1024 * 1024))  # 512 MiB

cargo run --locked
```

### File layout (dev filesystem backend)

This reference server uses a simple naming convention:

- **Public** image id `win7` → `${DISK_GATEWAY_PUBLIC_DIR}/win7.img`
- **Private** image id `secret` for user `alice` → `${DISK_GATEWAY_PRIVATE_DIR}/alice/secret.img`

Both `{id}` and `{user}` are validated as “path segments” (letters/digits/`._-`, excluding `.` and `..`) to avoid path traversal.

## API

### `POST /api/images/{id}/lease`

Issues a short-lived signed “lease” token for private images.

- Public images: returns a `url` and no token.
- Private images: **requires a caller identity** via one of:
  - `X-Debug-User: <user-id>` (placeholder auth)
  - `Authorization: Bearer <user-id>` (placeholder auth)
  and returns a JWT.

Response:

```json
{ "url": "/disk/<id>", "token": "<jwt>", "expiresAt": "2026-01-01T00:00:00Z" }
```

Warning: `X-Debug-User` is **not production auth**. Replace this with real authentication/authorization before deploying.

### `GET /disk/{id}` (bytes)

- Supports `Range: bytes=start-end` and multipart multi-range (`bytes=0-0,2-2`).
- Returns `206 Partial Content` with correct `Content-Range` when Range is present.
- Returns `416 Range Not Satisfiable` with `Content-Range: bytes */<size>` for invalid/unsatisfiable ranges.
- Returns `413 Payload Too Large` when multi-range limits are exceeded (`DISK_GATEWAY_MAX_RANGES`, `DISK_GATEWAY_MAX_TOTAL_BYTES`).
- Sets `Accept-Ranges: bytes`, `Content-Length`, and a (strong) `ETag`.
- Sets `Content-Type: application/octet-stream` and `X-Content-Type-Options: nosniff`.
- Supports basic conditional requests:
  - `If-None-Match` → `304 Not Modified`
  - `If-Range` + `Range` → `206` when matched, otherwise ignores `Range` and returns a full `200`
- Sets `Cache-Control: no-transform` to prevent intermediaries from applying compression to raw disk bytes.
  - For authenticated requests (private images), also sets `Cache-Control: private, no-store, no-transform`.

Private images require a valid lease token:

- Preferred: `Authorization: Bearer <token>`
- Optional: `?token=<token>`

Security note: Query-string tokens can leak via logs, caches, and `Referer` headers. Prefer `Authorization`.

### `HEAD /disk/{id}`

Returns headers (e.g. `Content-Length`, `Accept-Ranges`, `ETag`) without a body.

### CORS preflight

- `OPTIONS /disk/{id}` and `OPTIONS /api/*` return `204` with:
  - `Access-Control-Allow-Methods`
  - `Access-Control-Allow-Headers` including `Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type`
  - `Access-Control-Allow-Origin` from `DISK_GATEWAY_CORS_ALLOWED_ORIGINS`
  - `Access-Control-Max-Age: 86400`

## Examples

### Range read (public)

```bash
curl -v -H 'Range: bytes=0-15' \
  http://127.0.0.1:3000/disk/win7 \
  -o /tmp/first-16-bytes.bin
```

### Lease + Range read (private)

```bash
TOKEN="$(curl -s -X POST \
  -H 'X-Debug-User: alice' \
  http://127.0.0.1:3000/api/images/secret/lease \
  | jq -r .token)"

curl -v -H "Authorization: Bearer $TOKEN" \
  -H 'Range: bytes=0-1023' \
  http://127.0.0.1:3000/disk/secret \
  -o /tmp/first-1k.bin
```

### Browser fetch snippet (Range)

```js
const token = "<jwt>";
const res = await fetch("https://disk.example.com/disk/secret", {
  headers: {
    "Range": "bytes=0-1048575",
    "Authorization": `Bearer ${token}`,
  },
});
if (res.status !== 206) throw new Error(`Expected 206, got ${res.status}`);
const chunk = new Uint8Array(await res.arrayBuffer());
```

### CORS preflight smoke test

```bash
curl -i -X OPTIONS \
  -H 'Origin: https://app.example' \
  -H 'Access-Control-Request-Method: GET' \
  -H 'Access-Control-Request-Headers: Range, Authorization' \
  http://127.0.0.1:3000/disk/win7
```

## COEP/CORP + CORS header matrix

This server always sets CORS headers on `/disk/*` responses (when the request `Origin` is allowed) and sets
`Cross-Origin-Resource-Policy` based on `DISK_GATEWAY_CORP`.

| Deployment relationship (app → disk host) | Suggested `DISK_GATEWAY_CORP` | CORS (`DISK_GATEWAY_CORS_ALLOWED_ORIGINS`) |
| --- | --- | --- |
| Same-origin | `same-site` | Not required (but harmless) |
| Same-site (e.g. `app.example.com` → `disk.example.com`) | `same-site` | Allow the app origin |
| Cross-site | `cross-origin` | Allow the app origin (or `*` for non-credentialed requests) |

For a cross-origin isolated app (`Cross-Origin-Embedder-Policy: require-corp`), the disk resource must be
either CORS-enabled and/or explicitly allow embedding via CORP. Configure both correctly for your topology.
