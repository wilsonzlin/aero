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

echo "==> Starting MinIO (origin)..."
docker compose up -d

tmpfile="$(mktemp -t aero-minio-range-test.XXXXXX.bin)"
trap 'rm -f "$tmpfile"' EXIT
dd if=/dev/urandom of="$tmpfile" bs=1M count=2 status=none

obj="range-test-$(date +%s).bin"
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

echo "==> Verifying Range GET against proxy..."
curl -fsS -D - -o /dev/null -H 'Range: bytes=0-15' "${proxy}/${bucket}/${obj}" | grep -iE '^(HTTP/|content-range:|access-control-allow-origin:)'

echo "==> Verifying CORS preflight against proxy..."
curl -fsS -D - -o /dev/null -X OPTIONS \
  -H "Origin: ${allowed_origin}" \
  -H "Access-Control-Request-Method: GET" \
  -H "Access-Control-Request-Headers: range" \
  "${proxy}/${bucket}/${obj}" | grep -iE '^(HTTP/|access-control-allow-origin:)'

echo "==> Success."

if [[ "$want_cleanup" == "1" ]]; then
  echo "==> Stopping containers (docker compose down)..."
  docker compose --profile proxy down
fi
