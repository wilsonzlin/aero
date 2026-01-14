#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

bucket="${BUCKET_NAME:-disk-images}"
origin="${ORIGIN_URL:-http://localhost:9000}"
proxy="${PROXY_URL:-http://localhost:9002}"
allowed_origin="${CORS_ALLOWED_ORIGIN:-http://localhost:5173}"
corp="${CROSS_ORIGIN_RESOURCE_POLICY:-cross-origin}"

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
    if curl -fsS -o /dev/null "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep "$delay"
    i=$((i + 1))
  done
  echo "error: timed out waiting for $url" >&2
  return 1
}

require_status() {
  local headers="$1"
  local pattern="$2"
  echo "$headers" | head -n 1 | grep -qE "$pattern"
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
head_origin="$(curl -fsS -D - -o /dev/null -I -H "Origin: ${allowed_origin}" "${origin}/${bucket}/${obj}")"
require_status "$head_origin" '^HTTP/[^ ]+ 200 '
echo "$head_origin" | grep -i '^content-length:'
echo "$head_origin" | grep -i '^accept-ranges: *bytes'
echo "$head_origin" | grep -i "^access-control-allow-origin: ${allowed_origin}"
echo "$head_origin" | grep -i '^access-control-expose-headers:.*content-length'
echo "$head_origin" | grep -i '^access-control-expose-headers:.*etag'

echo "==> Verifying Range GET (206 + Content-Range) against origin..."
range_origin="$(curl -fsS -D - -o /dev/null -H "Origin: ${allowed_origin}" -H 'Range: bytes=0-15' "${origin}/${bucket}/${obj}")"
require_status "$range_origin" '^HTTP/[^ ]+ 206 '
echo "$range_origin" | grep -i '^content-range:'
echo "$range_origin" | grep -i "^access-control-allow-origin: ${allowed_origin}"
echo "$range_origin" | grep -i '^access-control-expose-headers:.*content-range'
echo "$range_origin" | grep -i '^access-control-expose-headers:.*etag'

echo "==> Verifying browser-style CORS preflight against origin..."
preflight_origin="$(curl -fsS -D - -o /dev/null -X OPTIONS \
  -H "Origin: ${allowed_origin}" \
  -H "Access-Control-Request-Method: GET" \
  -H "Access-Control-Request-Headers: range, if-range, if-none-match, if-modified-since" \
  "${origin}/${bucket}/${obj}")"
require_status "$preflight_origin" '^HTTP/[^ ]+ (200|204) '
echo "$preflight_origin" | grep -i "^access-control-allow-origin: ${allowed_origin}"
echo "$preflight_origin" | grep -i '^access-control-allow-methods:.*GET'
echo "$preflight_origin" | grep -i '^access-control-allow-headers:.*range'
echo "$preflight_origin" | grep -i '^access-control-allow-headers:.*if-range'
echo "$preflight_origin" | grep -i '^access-control-allow-headers:.*if-none-match'
echo "$preflight_origin" | grep -i '^access-control-allow-headers:.*if-modified-since'

echo "==> Verifying origin does not allow arbitrary CORS origins..."
preflight_origin_bad="$(curl -fsS -D - -o /dev/null -X OPTIONS \
  -H "Origin: http://example.com" \
  -H "Access-Control-Request-Method: GET" \
  -H "Access-Control-Request-Headers: range, if-range, if-none-match, if-modified-since" \
  "${origin}/${bucket}/${obj}")"
require_status "$preflight_origin_bad" '^HTTP/[^ ]+ (200|204) '
if echo "$preflight_origin_bad" | grep -qi '^access-control-allow-origin:'; then
  echo "error: origin unexpectedly returned Access-Control-Allow-Origin for disallowed Origin" >&2
  echo "$preflight_origin_bad" >&2
  exit 1
fi

echo "==> Starting proxy (optional CDN/edge emulation)..."
docker compose --profile proxy up -d minio-proxy

wait_for_http "${proxy}/minio/health/ready"

echo "==> Verifying HEAD against proxy..."
head_proxy="$(curl -fsS -D - -o /dev/null -I -H "Origin: ${allowed_origin}" "${proxy}/${bucket}/${obj}")"
require_status "$head_proxy" '^HTTP/[^ ]+ 200 '
echo "$head_proxy" | grep -i '^content-length:'
echo "$head_proxy" | grep -i '^accept-ranges: *bytes'
echo "$head_proxy" | grep -i "^access-control-allow-origin: ${allowed_origin}"
echo "$head_proxy" | grep -i '^access-control-expose-headers:.*content-length'
echo "$head_proxy" | grep -i '^access-control-expose-headers:.*etag'
echo "$head_proxy" | grep -i '^vary:.*access-control-request-method'
echo "$head_proxy" | grep -i "^cross-origin-resource-policy: ${corp}"

echo "==> Verifying Range GET against proxy..."
range_proxy="$(curl -fsS -D - -o /dev/null -H "Origin: ${allowed_origin}" -H 'Range: bytes=0-15' "${proxy}/${bucket}/${obj}")"
require_status "$range_proxy" '^HTTP/[^ ]+ 206 '
echo "$range_proxy" | grep -i '^content-range:'
echo "$range_proxy" | grep -i "^access-control-allow-origin: ${allowed_origin}"
echo "$range_proxy" | grep -i '^access-control-expose-headers:.*content-range'
echo "$range_proxy" | grep -i '^access-control-expose-headers:.*etag'
echo "$range_proxy" | grep -i '^vary:.*access-control-request-method'
echo "$range_proxy" | grep -i "^cross-origin-resource-policy: ${corp}"

echo "==> Verifying CORS preflight against proxy..."
preflight_proxy="$(curl -fsS -D - -o /dev/null -X OPTIONS \
  -H "Origin: ${allowed_origin}" \
  -H "Access-Control-Request-Method: GET" \
  -H "Access-Control-Request-Headers: range, if-range, if-none-match, if-modified-since" \
  "${proxy}/${bucket}/${obj}")"
require_status "$preflight_proxy" '^HTTP/[^ ]+ (200|204) '
echo "$preflight_proxy" | grep -i "^access-control-allow-origin: ${allowed_origin}"
echo "$preflight_proxy" | grep -i '^access-control-allow-methods:.*GET'
echo "$preflight_proxy" | grep -i '^access-control-allow-headers:.*range'
echo "$preflight_proxy" | grep -i '^access-control-allow-headers:.*if-range'
echo "$preflight_proxy" | grep -i '^access-control-allow-headers:.*if-none-match'
echo "$preflight_proxy" | grep -i '^access-control-allow-headers:.*if-modified-since'
echo "$preflight_proxy" | grep -i '^vary:.*access-control-request-method'

echo "==> Success."
