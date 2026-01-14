# Disk streaming conformance tool

This repo supports two ways of delivering disk image bytes to browsers:

- **Range mode**: a single object served with `Range: bytes=...` (default / legacy).
- **Chunked mode**: `manifest.json` + `chunks/<index>.bin` objects (no `Range` header; avoids CORS preflight).

This tool can validate both:

- `--mode range` (default): HTTP Range endpoint checks (existing behavior)
- `--mode chunked`: chunked disk image format conformance (see `docs/18-chunked-disk-image-format.md`)

## Range mode (`--mode range`)

Validates that a disk image streaming endpoint is compatible with Aero’s browser-side expectations:

- `HEAD` advertises byte ranges and provides a stable `Content-Length`
- `GET` Range requests work (`206` + correct `Content-Range`)
- `GET`/`416` responses also advertise `Accept-Ranges: bytes`
- `GET` Range responses are safe for byte-addressed reads (no compression transforms):
  - `Cache-Control` includes `no-transform`
  - `Content-Encoding` is absent or `identity`
- Recommended content headers are present (`GET`/`HEAD`):
  - `Content-Type: application/octet-stream`
  - `X-Content-Type-Options: nosniff`
- `If-Range` semantics (defence against mixed-version bytes):
  - `GET Range` with `If-Range: <strong-etag>` returns `206` (skipped if ETag is missing or weak)
  - `GET Range` with `If-Range: "mismatch"` returns `200` (preferred) or `412`
  - (Optional) `GET Range` with `If-Range: <last-modified>` (HTTP-date form) returns `206` (WARN if not)
- (Recommended) `ETag` is a strong validator (not `W/"..."`) so `If-Range` can be used safely
- Conditional caching works when validators are present:
  - `GET` with `If-None-Match: <etag>` returns `304 Not Modified` (skipped if ETag is missing)
  - `HEAD` with `If-None-Match: <etag>` returns `304 Not Modified` (skipped if ETag is missing)
  - (Recommended) `304` responses include `ETag` and it matches the current validator
  - (Optional) `GET` with `If-Modified-Since: <last-modified>` returns `304` (WARN if not)
- (Optional) `HEAD` with `If-Modified-Since: <last-modified>` returns `304` (WARN if not)
- Unsatisfiable ranges fail correctly (`416` + `Content-Range: bytes */<size>`)
- CORS preflight (`OPTIONS`) allows required request headers:
  - `Range`, `If-Range` (and `Authorization` when testing private images)
  - `If-None-Match` when an `ETag` is advertised (so conditional revalidation is usable from browsers)
  - (Optional) `If-Modified-Since` when `Last-Modified` is advertised (WARN if not)
- CORS preflight caching sanity:
  - Warn if `Access-Control-Max-Age` is missing or very low (preflights can be expensive for many small range reads)
  - Warn if `Vary` is missing (recommended for safe caching of preflight responses)
- CORS responses expose required headers (`Access-Control-Expose-Headers` for `Accept-Ranges`, `Content-Length`, `Content-Range`)
- (Recommended) if a response includes `ETag`, expose it via `Access-Control-Expose-Headers` so browser-side code can use validators (`If-Range`, `If-None-Match`)
- Note: `Last-Modified` is a CORS-safelisted response header and is exposed to JS by default (no `Expose-Headers` needed)
- (Recommended) if a response includes `Content-Encoding` (even `identity`), expose it via `Access-Control-Expose-Headers` so browser-side code can detect non-identity encodings.
- CORS sanity checks:
  - Warn if `Access-Control-Allow-Credentials: true` is used with `Access-Control-Allow-Origin: *`
  - Warn if `Access-Control-Allow-Origin` echoes a specific origin but `Vary: Origin` is missing
- COEP/CORP defence-in-depth: `Cross-Origin-Resource-Policy` is present on `GET`/`HEAD` (WARN-only by default; see `--strict` / `--expect-corp`)
  - Warns if the value is not one of `same-origin`, `same-site`, `cross-origin`
- (Private images) unauthenticated requests are denied, authenticated requests succeed
- (Private images) `206` responses are not publicly cacheable (`Cache-Control: no-store` recommended; WARN-only by default)

The script is dependency-free (Python stdlib only) and exits non-zero on failures (CI-friendly).

## Safety: response body read cap

This tool is intended to be run against real **20–40GB** disk image URLs.

To avoid accidental full-disk downloads (for example when a server/CDN ignores `Range` and returns `200 OK`), the conformance script enforces a **response body read cap** for non-`HEAD` requests and will close the connection after reading at most the configured number of bytes.

You can override the cap with:

- `--max-body-bytes`
- `MAX_BODY_BYTES`

Defaults:

- **Range mode**: **1 MiB** (enough for 1-byte range probes, while preventing full-body downloads)
- **Chunked mode**: **64 MiB** (matches the reference client safety bounds; enough to fetch typical 4 MiB chunks and large manifests)

## Usage

### Self-test against the repo dev server (`server/range_server.js`)

For a quick local sanity check (no MinIO/S3 required), run:

```bash
python3 tools/disk-streaming-conformance/selftest_range_server.py
```

This starts `server/range_server.js` on a random local port with a temporary test file, then runs the conformance suite in `--strict` mode against it.

### Self-test against the repo dev chunk server (`server/chunk_server.js`)

For a quick local sanity check of chunked mode, run:

```bash
python3 tools/disk-streaming-conformance/selftest_chunk_server.py
```

This starts `server/chunk_server.js` on a random local port with a temporary `manifest.json` + `chunks/*.bin`, then runs the conformance suite in `--mode chunked --strict` against it.

### Public image

```bash
python3 tools/disk-streaming-conformance/conformance.py \
  --base-url 'https://aero.example.com/disk/my-image' \
  --origin 'https://app.example.com'
```

Note: `--origin` / `ORIGIN` defaults to `https://example.com`. Set it to your real app origin to test your deployed CORS allowlist.

You can also use environment variables:

```bash
BASE_URL='https://aero.example.com/disk/my-image' \
ORIGIN='https://app.example.com' \
python3 tools/disk-streaming-conformance/conformance.py
```

For chunked mode, you can use:

- `MODE=chunked`
- `MANIFEST_URL=...` (or `BASE_URL=...` as a prefix containing `manifest.json`)
- `MAX_BYTES_PER_CHUNK=...`

### Private image (Authorization header)

Provide `TOKEN`/`--token` only when testing a **private** image. The tool will assert that requests without the token are denied (401/403) and that requests with the token succeed.

```bash
BASE_URL='https://aero.example.com/disk/private-image' \
TOKEN='Bearer eyJ...' \
ORIGIN='https://app.example.com' \
python3 tools/disk-streaming-conformance/conformance.py
```

If you pass a token **without** whitespace (e.g. `TOKEN='eyJ...'`), the tool will assume a Bearer token and send `Authorization: Bearer <TOKEN>`.

### Chunked image (manifest + chunks, no Range)

Validate a chunked image using an explicit manifest URL:

```bash
python3 tools/disk-streaming-conformance/conformance.py \
  --mode chunked \
  --manifest-url 'https://cdn.example.com/images/demo/sha256-acde.../manifest.json' \
  --origin 'https://app.example.com'
```

Or provide a base prefix (the tool will fetch `<base-url>/manifest.json`):

```bash
python3 tools/disk-streaming-conformance/conformance.py \
  --mode chunked \
  --base-url 'https://cdn.example.com/images/demo/sha256-acde...' \
  --origin 'https://app.example.com'
```

Fetch a few extra chunks for better coverage:

```bash
python3 tools/disk-streaming-conformance/conformance.py \
  --mode chunked \
  --manifest-url 'https://cdn.example.com/images/demo/sha256-acde.../manifest.json' \
  --sample-chunks 3
```

Chunked mode will verify `chunks[i].sha256` for the sampled chunks when present.

Note: The tool sends a browser-like `Accept-Encoding` (e.g. `gzip, deflate, br, zstd`) to match real `fetch()` behavior. For compatibility with Aero’s reference clients and tooling, it requires both `manifest.json` and chunk objects to be served with `Content-Encoding` absent or `identity` (i.e. no compression transforms).

### Private chunked image (Authorization header)

If you provide `--token` / `TOKEN` in chunked mode, the tool treats the image as private and will additionally check:

- unauthenticated GETs to `manifest.json` and a sample chunk are denied (`401/403`)
- authenticated responses are not publicly cacheable (`Cache-Control` must not include `public`; `no-store` is recommended)

Note: Using `Authorization` on cross-origin chunk GETs will reintroduce CORS preflight. Prefer signed URLs/cookies if you are using chunked mode specifically to avoid preflight.

Safety knobs:

- `--max-body-bytes`: per-request read cap (manifest + chunk bodies)
- `--max-bytes-per-chunk`: refuse to download chunks larger than this (default 64 MiB)

### Running chunked mode against local MinIO (direct object URLs)

You can publish a chunked image to a local MinIO/S3 endpoint using `tools/image-chunker`, then run the conformance tool directly against the public object URLs.

1) Start the local object store (MinIO):

```bash
cd infra/local-object-store
docker compose up -d
```

2) Build the chunker and publish a small test image:

```bash
cargo build --release --locked --manifest-path tools/image-chunker/Cargo.toml

truncate -s 16M disk.img

export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin

./tools/image-chunker/target/release/aero-image-chunker publish \
  --file ./disk.img \
  --bucket disk-images \
  --prefix images/demo/v1/ \
  --image-id demo \
  --image-version v1 \
  --chunk-size 4194304 \
  --endpoint http://localhost:9000 \
  --force-path-style \
  --region us-east-1
```

3) Run conformance against the manifest URL:

```bash
python3 tools/disk-streaming-conformance/conformance.py \
  --mode chunked \
  --manifest-url 'http://localhost:9000/disk-images/images/demo/v1/manifest.json' \
  --origin 'http://localhost:5173'
```

Note: When serving directly from MinIO/S3 without a proxy/CDN layer, some best-practice headers (e.g. `X-Content-Type-Options: nosniff`, `Cross-Origin-Resource-Policy`) may be missing, which will show up as `WARN` (and fail under `--strict`).

## CI notes

- Exit code `0` = all checks passed
- Exit code `1` = one or more checks failed
- Some checks may emit `WARN` (exit code is still `0` unless you pass `--strict`)

Example output:

```text
Disk streaming conformance
  BASE_URL: https://aero.example.com/disk/my-image
  ORIGIN:   https://app.example.com
  STRICT:   False
  CORP:     (not required)
  MAX_BODY_BYTES: 1048576 (1.00 MiB)
  AUTH:     (none)

PASS HEAD: Accept-Ranges=bytes and Content-Length is present - size=2147483648 (2.00 GiB)
PASS HEAD: ETag is strong (recommended for If-Range)
PASS GET: ETag matches HEAD ETag
PASS HEAD: Content-Type is application/octet-stream and X-Content-Type-Options=nosniff
PASS CORS: Allow-Credentials does not contradict Allow-Origin - (no Allow-Credentials)
SKIP CORS: Vary includes Origin when Allow-Origin echoes a specific origin - skipped (Allow-Origin is '*')
PASS HEAD: If-None-Match returns 304 Not Modified - status=304
PASS HEAD: If-Modified-Since returns 304 Not Modified - status=304
PASS HEAD: Cross-Origin-Resource-Policy is set - value='same-site'
PASS GET: Cross-Origin-Resource-Policy is set - value='same-site'
PASS GET: Content-Type is application/octet-stream and X-Content-Type-Options=nosniff
PASS GET: valid Range (first byte) returns 206 with correct Content-Range and body length - Content-Range='bytes 0-0/2147483648'
SKIP private: 206 responses are not publicly cacheable (Cache-Control) - skipped (no --token provided)
PASS GET: valid Range (mid-file) returns 206 with correct Content-Range and body length - Content-Range='bytes 1073741824-1073741824/2147483648'
PASS GET: unsatisfiable Range returns 416 and Content-Range bytes */<size> - Content-Range='bytes */2147483648'
PASS GET: Range + If-Range (matching ETag) returns 206 - Content-Range='bytes 0-0/2147483648'
PASS GET: Range + If-Range ("mismatch") does not return mixed-version 206 - status=200 (Range ignored)
PASS GET: If-None-Match returns 304 Not Modified - status=304
PASS GET: If-Modified-Since returns 304 Not Modified - status=304
PASS OPTIONS: CORS preflight allows Range + If-Range headers + If-None-Match - status=204
PASS OPTIONS: CORS preflight allows If-Modified-Since header - status=204

Summary: 19 passed, 0 failed, 0 warned, 2 skipped
```

## Strict mode

`--strict` fails on `WARN` conditions. This includes things like:

- `Transfer-Encoding: chunked` on `206` responses (some CDNs mishandle it)
- Missing `Cross-Origin-Resource-Policy`
- `Cross-Origin-Resource-Policy` present but with an unexpected value
- Weak `ETag` validators (If-Range requires strong ETag)
- ETag mismatch between HEAD and GET responses
- Missing/mismatched `ETag` on `304 Not Modified` responses
- Missing recommended content headers (e.g. `X-Content-Type-Options: nosniff`)
- Unexpected `Content-Encoding` (disk bytes must be served as identity / no compression transforms)
- (Chunked mode) manifest/chunk caching headers missing `immutable` and/or `no-transform` (recommended for versioned, CDN-hosted artifacts)
- Private responses missing `Cache-Control: no-store`
- `If-Range` mismatch returning `412` instead of `200`
- `If-Modified-Since` not returning `304` (this check is WARN-only by default)
- Preflight caching issues like missing/low `Access-Control-Max-Age` or missing `Vary`
- CORS header issues like:
  - `Access-Control-Allow-Credentials: true` with `Access-Control-Allow-Origin: *`
  - missing `Vary: Origin` when echoing a specific `Access-Control-Allow-Origin`

## CORP expectations

By default, the tool only warns if `Cross-Origin-Resource-Policy` is missing.

You can require a specific value:

```bash
python3 tools/disk-streaming-conformance/conformance.py \
  --base-url 'https://aero.example.com/disk/my-image' \
  --expect-corp 'same-site'
```

## Running against the reference `server/disk-gateway`

The repo includes a reference implementation at `server/disk-gateway` which is intended to pass all checks.

### Public image

```bash
cd server/disk-gateway

export DISK_GATEWAY_TOKEN_SECRET='dev-secret-change-me'
export DISK_GATEWAY_CORS_ALLOWED_ORIGINS='*'

mkdir -p public-images
truncate -s 1M public-images/win7.img

cargo run --locked
```

In another terminal:

```bash
BASE_URL='http://127.0.0.1:3000/disk/win7' \
ORIGIN='https://example.com' \
python3 tools/disk-streaming-conformance/conformance.py
```

### Private image

```bash
cd server/disk-gateway

export DISK_GATEWAY_TOKEN_SECRET='dev-secret-change-me'
export DISK_GATEWAY_CORS_ALLOWED_ORIGINS='*'

mkdir -p private-images/alice
truncate -s 1M private-images/alice/secret.img

cargo run --locked
```

In another terminal:

```bash
TOKEN="$(curl -s -X POST -H 'X-Debug-User: alice' \
  http://127.0.0.1:3000/api/images/secret/lease \
  | jq -r .token)"

BASE_URL='http://127.0.0.1:3000/disk/secret' \
TOKEN="$TOKEN" \
ORIGIN='https://example.com' \
python3 tools/disk-streaming-conformance/conformance.py
```

## Running against `services/image-gateway` (local dev + MinIO)

Start MinIO (creates the `aero-images` bucket by default):

```bash
cd services/image-gateway
docker compose -f docker-compose.minio.yml up -d
```

In another terminal, start the gateway pointed at MinIO (disable auth for this local conformance run):

```bash
# From the repo root (npm workspaces)
npm ci

export AUTH_MODE=none
export CORS_ALLOW_ORIGIN='*'

export S3_BUCKET='aero-images'
export AWS_REGION='us-east-1'
export AWS_ACCESS_KEY_ID='minioadmin'
export AWS_SECRET_ACCESS_KEY='minioadmin'
export S3_ENDPOINT='http://127.0.0.1:9000'
export S3_FORCE_PATH_STYLE='true'

npm -w services/image-gateway run dev
```

Create a small image via the API, upload a single part, and complete the upload (example uses `jq`):

```bash
API='http://127.0.0.1:3000'

IMG="$(curl -sS -X POST "$API/v1/images")"
IMAGE_ID="$(echo "$IMG" | jq -r .imageId)"
UPLOAD_ID="$(echo "$IMG" | jq -r .uploadId)"

UPLOAD_URL="$(curl -sS -X POST "$API/v1/images/$IMAGE_ID/upload-url" \
  -H 'content-type: application/json' \
  -d "{\"uploadId\":\"$UPLOAD_ID\",\"partNumber\":1}" \
  | jq -r .url)"

truncate -s 1M part.bin
ETAG="$(curl -sS -D - -o /dev/null -X PUT --upload-file part.bin "$UPLOAD_URL" \
  | awk -F': ' 'tolower($1)=="etag" {print $2}' | tr -d '\r\"')"

curl -sS -X POST "$API/v1/images/$IMAGE_ID/complete" \
  -H 'content-type: application/json' \
  -d "{\"uploadId\":\"$UPLOAD_ID\",\"parts\":[{\"partNumber\":1,\"etag\":\"$ETAG\"}]}" \
  > /dev/null
```

Now run conformance against the Range proxy endpoint:

```bash
BASE_URL="$API/v1/images/$IMAGE_ID/range" \
ORIGIN='https://example.com' \
python3 ../../tools/disk-streaming-conformance/conformance.py
```
