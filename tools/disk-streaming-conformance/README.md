# Disk streaming conformance tool

Validates that a disk image streaming endpoint is compatible with Aero’s browser-side expectations:

- `HEAD` advertises byte ranges and provides a stable `Content-Length`
- `GET` Range requests work (`206` + correct `Content-Range`)
- `GET` Range responses are safe for byte-addressed reads (no compression transforms):
  - `Cache-Control` includes `no-transform`
  - `Content-Encoding` is absent or `identity`
- Unsatisfiable ranges fail correctly (`416` + `Content-Range: bytes */<size>`)
- CORS preflight (`OPTIONS`) allows the `Range` and `Authorization` headers
- CORS responses expose required headers (`Access-Control-Expose-Headers` for `Accept-Ranges`, `Content-Length`, `Content-Range`, `ETag`)
- (Private images) unauthenticated requests are denied, authenticated requests succeed

The script is dependency-free (Python stdlib only) and exits non-zero on failures (CI-friendly).

## Usage

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

### Private image (Authorization header)

Provide `TOKEN`/`--token` only when testing a **private** image. The tool will assert that requests without the token are denied (401/403) and that requests with the token succeed.

```bash
BASE_URL='https://aero.example.com/disk/private-image' \
TOKEN='Bearer eyJ...' \
ORIGIN='https://app.example.com' \
python3 tools/disk-streaming-conformance/conformance.py
```

If you pass a token **without** whitespace (e.g. `TOKEN='eyJ...'`), the tool will assume a Bearer token and send `Authorization: Bearer <TOKEN>`.

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
  AUTH:     (none)

PASS HEAD: Accept-Ranges=bytes and Content-Length is present - size=2147483648 (2.00 GiB)
PASS GET: valid Range (first byte) returns 206 with correct Content-Range and body length - Content-Range='bytes 0-0/2147483648'
PASS GET: valid Range (mid-file) returns 206 with correct Content-Range and body length - Content-Range='bytes 1073741824-1073741824/2147483648'
PASS GET: unsatisfiable Range returns 416 and Content-Range bytes */<size> - Content-Range='bytes */2147483648'
PASS OPTIONS: CORS preflight allows Range + Authorization headers - status=204

Summary: 5 passed, 0 failed, 0 warned, 0 skipped
```

## Strict mode

`--strict` fails on `WARN` conditions. Currently this includes `Transfer-Encoding: chunked` on `206` responses (some CDNs mishandle it).

## Running against the reference `server/disk-gateway`

The repo includes a reference implementation at `server/disk-gateway` which is intended to pass all checks.

### Public image

```bash
cd server/disk-gateway

export DISK_GATEWAY_TOKEN_SECRET='dev-secret-change-me'
export DISK_GATEWAY_CORS_ALLOWED_ORIGINS='*'

mkdir -p public-images
truncate -s 1M public-images/win7.img

cargo run
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

cargo run
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
cd services/image-gateway

export AUTH_MODE=none
export CORS_ALLOW_ORIGIN='*'

export S3_BUCKET='aero-images'
export AWS_REGION='us-east-1'
export AWS_ACCESS_KEY_ID='minioadmin'
export AWS_SECRET_ACCESS_KEY='minioadmin'
export S3_ENDPOINT='http://127.0.0.1:9000'
export S3_FORCE_PATH_STYLE='true'

npm install
npm run dev
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
