#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

bucket="${BUCKET_NAME:-disk-images}"
origin="${ORIGIN_URL:-http://localhost:9000}"
proxy="${PROXY_URL:-http://localhost:9002}"
allowed_origin="${CORS_ALLOWED_ORIGIN:-http://localhost:5173}"

want_cleanup=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --down)
      want_cleanup=1
      shift
      ;;
    *)
      echo "usage: $0 [--down]" >&2
      exit 2
      ;;
  esac
done

cleanup() {
  if [[ -n "${tmpfile:-}" ]]; then
    rm -f "$tmpfile"
  fi
  if [[ "$want_cleanup" == "1" ]]; then
    echo "==> Stopping containers (docker compose down)..."
    docker compose --profile proxy down
  fi
}

trap cleanup EXIT

wait_for_http() {
  local url="$1"
  local tries="${2:-50}"
  local delay="${3:-0.2}"
  local i=0
  while [[ $i -lt $tries ]]; do
    if curl -fsS -o /dev/null "$url"; then
      return 0
    fi
    sleep "$delay"
    i=$((i + 1))
  done
  echo "error: timed out waiting for $url" >&2
  return 1
}

echo "==> Starting MinIO (origin)..."
docker compose up -d

# The `mc` helper container only has access to this directory (mounted at /work),
# so the temporary file must be created *here* (not in /tmp).
tmpfile="_smoke-upload.$$.$RANDOM.bin"
dd if=/dev/urandom of="$tmpfile" bs=1M count=2 >/dev/null 2>&1

obj="${SMOKE_OBJECT_KEY:-_smoke/range-test.bin}"
echo "==> Uploading test object: s3://${bucket}/${obj}"
docker compose --profile tools run --rm mc cp "$tmpfile" "local/${bucket}/${obj}" >/dev/null

echo "==> Verifying HEAD (Content-Length + Accept-Ranges)..."
curl -fsS -D - -o /dev/null -I "${origin}/${bucket}/${obj}" | grep -iE '^(HTTP/|content-length:|accept-ranges:)'

echo "==> Verifying Range GET (206 + Content-Range) against origin..."
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-15' "${origin}/${bucket}/${obj}" | grep -iE '^(HTTP/|content-range:)'

echo "==> Verifying browser-style CORS preflight against origin..."
curl -fsS -D - -o /dev/null -X OPTIONS \
  -H "Origin: ${allowed_origin}" \
  -H "Access-Control-Request-Method: GET" \
  -H "Access-Control-Request-Headers: range" \
  "${origin}/${bucket}/${obj}" | grep -iE '^(HTTP/|access-control-allow-origin:)'

echo "==> Starting proxy (optional CDN/edge emulation)..."
docker compose --profile proxy up -d minio-proxy

wait_for_http "${proxy}/minio/health/ready"

echo "==> Verifying Range GET against proxy..."
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-15' "${proxy}/${bucket}/${obj}" | grep -iE '^(HTTP/|content-range:|access-control-allow-origin:)'

echo "==> Verifying CORS preflight against proxy..."
curl -fsS -D - -o /dev/null -X OPTIONS \
  -H "Origin: ${allowed_origin}" \
  -H "Access-Control-Request-Method: GET" \
  -H "Access-Control-Request-Headers: range" \
  "${proxy}/${bucket}/${obj}" | grep -iE '^(HTTP/|access-control-allow-origin:)'

echo "==> Success."
