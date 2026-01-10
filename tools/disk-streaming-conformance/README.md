# Disk streaming conformance tool

Validates that a disk image streaming endpoint is compatible with Aero’s browser-side expectations:

- `HEAD` advertises byte ranges and provides a stable `Content-Length`
- `GET` Range requests work (`206` + correct `Content-Range`)
- Unsatisfiable ranges fail correctly (`416` + `Content-Range: bytes */<size>`)
- CORS preflight (`OPTIONS`) allows the `Range` and `Authorization` headers
- CORS responses expose required headers (`Access-Control-Expose-Headers` for `Accept-Ranges`, `Content-Length`, `Content-Range`)
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

Example output:

```text
Disk streaming conformance
  BASE_URL: https://aero.example.com/disk/my-image
  ORIGIN:   https://app.example.com
  AUTH:     (none)

PASS HEAD: Accept-Ranges=bytes and Content-Length is present - size=2147483648 (2.00 GiB)
PASS GET: valid Range (first byte) returns 206 with correct Content-Range and body length - Content-Range='bytes 0-0/2147483648'
PASS GET: valid Range (mid-file) returns 206 with correct Content-Range and body length - Content-Range='bytes 1073741824-1073741824/2147483648'
PASS GET: unsatisfiable Range returns 416 and Content-Range bytes */<size> - Content-Range='bytes */2147483648'
PASS OPTIONS: CORS preflight allows Range + Authorization headers - status=204

Summary: 5 passed, 0 failed, 0 skipped
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
