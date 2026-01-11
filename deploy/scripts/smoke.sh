#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/deploy/docker-compose.yml"

PROJECT_NAME="aero-smoke-$RANDOM$RANDOM"
SMOKE_WASM_NAME="__aero_smoke_${PROJECT_NAME}.wasm"
SMOKE_WASM_PATH="$ROOT_DIR/deploy/static/assets/$SMOKE_WASM_NAME"
SMOKE_WASM_DIR="$(dirname "$SMOKE_WASM_PATH")"

compose() {
  docker compose -f "$COMPOSE_FILE" -p "$PROJECT_NAME" "$@"
}

on_exit() {
  status=$?
  if [[ $status -ne 0 ]]; then
    echo "deploy smoke: FAILED (status=$status)" >&2
    echo "deploy smoke: docker compose ps" >&2
    compose ps >&2 || true
    echo "deploy smoke: docker compose logs" >&2
    compose logs --no-color >&2 || true
  fi

  compose down -v --remove-orphans >/dev/null 2>&1 || true

  rm -f "$SMOKE_WASM_PATH" >/dev/null 2>&1 || true
  rmdir "$SMOKE_WASM_DIR" >/dev/null 2>&1 || true
}
trap on_exit EXIT

if ! command -v docker >/dev/null 2>&1; then
  echo "deploy smoke: docker not found" >&2
  exit 1
fi

mkdir -p "$SMOKE_WASM_DIR"
# Minimal valid WebAssembly module header: \0asm + version 1.
printf '\x00asm\x01\x00\x00\x00' >"$SMOKE_WASM_PATH"

echo "deploy smoke: starting stack ($PROJECT_NAME)" >&2
compose up -d --build

echo "deploy smoke: waiting for https://localhost/healthz" >&2
for _ in $(seq 1 60); do
  if curl -kfsS https://localhost/healthz >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

health_body="$(curl -kfsS https://localhost/healthz)"
if ! echo "$health_body" | grep -Eq '(^ok$|\"ok\"[[:space:]]*:[[:space:]]*true)'; then
  echo "deploy smoke: unexpected /healthz body: $health_body" >&2
  exit 1
fi

fetch_headers() {
  local url="$1"
  curl -kfsS -D- -o /dev/null "$url" | tr -d '\r'
}

assert_header_exact() {
  local header="$1"
  local value="$2"
  local headers="$3"
  local line
  line="$(echo "$headers" | grep -Fi "${header}:" | head -n1 || true)"
  if [[ -z "$line" ]]; then
    echo "deploy smoke: missing header: $header: $value" >&2
    echo "--- headers ---" >&2
    echo "$headers" >&2
    exit 1
  fi

  local got="${line#*:}"
  got="$(echo "$got" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
  if [[ "$got" != "$value" ]]; then
    echo "deploy smoke: header mismatch for $header (expected '$value', got '$got')" >&2
    echo "--- headers ---" >&2
    echo "$headers" >&2
    exit 1
  fi
}

root_headers="$(fetch_headers https://localhost/)"
health_headers="$(fetch_headers https://localhost/healthz)"
wasm_headers="$(fetch_headers "https://localhost/assets/$SMOKE_WASM_NAME")"

for headers in "$root_headers" "$health_headers" "$wasm_headers"; do
  assert_header_exact "Cross-Origin-Opener-Policy" "same-origin" "$headers"
  assert_header_exact "Cross-Origin-Embedder-Policy" "require-corp" "$headers"
  assert_header_exact "Cross-Origin-Resource-Policy" "same-origin" "$headers"
  assert_header_exact "Origin-Agent-Cluster" "?1" "$headers"

  if ! echo "$headers" | grep -Eiq "^Content-Security-Policy: .*connect-src 'self'"; then
    echo "deploy smoke: missing/invalid Content-Security-Policy connect-src" >&2
    echo "--- headers ---" >&2
    echo "$headers" >&2
    exit 1
  fi
done

assert_header_exact "Cache-Control" "public, max-age=31536000, immutable" "$wasm_headers"
assert_header_exact "Content-Type" "application/wasm" "$wasm_headers"

echo "deploy smoke: OK" >&2
