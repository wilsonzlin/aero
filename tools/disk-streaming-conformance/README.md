# Disk streaming conformance tool

Validates that a disk image streaming endpoint is compatible with Aero’s browser-side expectations:

- `HEAD` advertises byte ranges and provides a stable `Content-Length`
- `GET` Range requests work (`206` + correct `Content-Range`)
- Unsatisfiable ranges fail correctly (`416` + `Content-Range: bytes */<size>`)
- CORS preflight (`OPTIONS`) allows the `Range` and `Authorization` headers
- (Private images) unauthenticated requests are denied, authenticated requests succeed

The script is dependency-free (Python stdlib only) and exits non-zero on failures (CI-friendly).

## Usage

### Public image

```bash
python3 tools/disk-streaming-conformance/conformance.py \
  --base-url 'https://aero.example.com/disk/my-image' \
  --origin 'https://app.example.com'
```

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
PASS GET: valid Range returns 206 with correct Content-Range and body length - Content-Range='bytes 0-0/2147483648'
PASS GET: unsatisfiable Range returns 416 and Content-Range bytes */<size> - Content-Range='bytes */2147483648'
PASS OPTIONS: CORS preflight allows Range + Authorization headers - status=204

Summary: 4 passed, 0 failed, 0 skipped
```

