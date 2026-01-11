#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/deploy/docker-compose.yml"

PROJECT_NAME="aero-smoke-$RANDOM$RANDOM"
SMOKE_FRONTEND_ROOT="$ROOT_DIR/deploy/static"
SMOKE_WASM_NAME="__aero_smoke_${PROJECT_NAME}.wasm"
SMOKE_WASM_PATH="$SMOKE_FRONTEND_ROOT/assets/$SMOKE_WASM_NAME"
SMOKE_WASM_DIR="$(dirname "$SMOKE_WASM_PATH")"

compose() {
  AERO_DOMAIN=localhost \
    AERO_HSTS_MAX_AGE=0 \
    AERO_CSP_CONNECT_SRC_EXTRA= \
    AERO_GATEWAY_UPSTREAM=aero-gateway:8080 \
    AERO_L2_PROXY_UPSTREAM=aero-l2-proxy:8090 \
    AERO_GATEWAY_IMAGE="aero-gateway:${PROJECT_NAME}" \
    AERO_L2_PROXY_IMAGE="aero-l2-proxy:${PROJECT_NAME}" \
    TRUST_PROXY=1 \
    CROSS_ORIGIN_ISOLATION=0 \
    AERO_FRONTEND_ROOT="$SMOKE_FRONTEND_ROOT" \
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

assert_header_exact "Cache-Control" "no-cache" "$root_headers"

for headers in "$root_headers" "$health_headers" "$wasm_headers"; do
  assert_header_exact "Cross-Origin-Opener-Policy" "same-origin" "$headers"
  assert_header_exact "Cross-Origin-Embedder-Policy" "require-corp" "$headers"
  assert_header_exact "Cross-Origin-Resource-Policy" "same-origin" "$headers"
  assert_header_exact "Origin-Agent-Cluster" "?1" "$headers"

  assert_header_exact "X-Content-Type-Options" "nosniff" "$headers"
  assert_header_exact "Referrer-Policy" "no-referrer" "$headers"
  assert_header_exact "Permissions-Policy" "camera=(), geolocation=(), microphone=(self), usb=(self)" "$headers"

  if ! echo "$headers" | grep -Eiq "^Content-Security-Policy: .*connect-src 'self'"; then
    echo "deploy smoke: missing/invalid Content-Security-Policy connect-src" >&2
    echo "--- headers ---" >&2
    echo "$headers" >&2
    exit 1
  fi
  if ! echo "$headers" | grep -Eiq "^Content-Security-Policy: .*script-src 'self' 'wasm-unsafe-eval'"; then
    echo "deploy smoke: missing/invalid Content-Security-Policy script-src wasm-unsafe-eval" >&2
    echo "--- headers ---" >&2
    echo "$headers" >&2
    exit 1
  fi
  if ! echo "$headers" | grep -Eiq "^Content-Security-Policy: .*worker-src 'self' blob:"; then
    echo "deploy smoke: missing/invalid Content-Security-Policy worker-src" >&2
    echo "--- headers ---" >&2
    echo "$headers" >&2
    exit 1
  fi
done

assert_header_exact "Cache-Control" "public, max-age=31536000, immutable" "$wasm_headers"
assert_header_exact "Content-Type" "application/wasm" "$wasm_headers"

# /l2 WebSocket upgrade check (L2 tunnel).
#
# We validate the TLS + Upgrade path and subprotocol negotiation without relying
# on external tools like `wscat`/`websocat`.
#
# If `node` is unavailable, skip this check (local dev convenience script).
if command -v node >/dev/null 2>&1; then
  echo "deploy smoke: verifying wss://localhost/l2 upgrade (aero-l2-tunnel-v1)" >&2
  l2_ok=0
  l2_last_error=""
  for _ in $(seq 1 30); do
    if l2_last_error="$(
      node --input-type=commonjs - 2>&1 <<'NODE'
const tls = require("node:tls");
const crypto = require("node:crypto");

const host = "localhost";
const port = 443;
const path = "/l2";
const expectedProtocol = "aero-l2-tunnel-v1";

const key = crypto.randomBytes(16).toString("base64");
const req = [
  `GET ${path} HTTP/1.1`,
  `Host: ${host}`,
  "Connection: Upgrade",
  "Upgrade: websocket",
  "Sec-WebSocket-Version: 13",
  `Sec-WebSocket-Key: ${key}`,
  `Sec-WebSocket-Protocol: ${expectedProtocol}`,
  `Origin: https://${host}`,
  "",
  "",
].join("\r\n");

const socket = tls.connect({
  host,
  port,
  servername: host,
  rejectUnauthorized: false,
});

const timeout = setTimeout(() => {
  console.error("timeout waiting for /l2 upgrade response");
  process.exit(1);
}, 2_000);

let buf = "";
socket.on("secureConnect", () => {
  socket.write(req);
});

socket.on("data", (chunk) => {
  buf += chunk.toString("utf8");
  const idx = buf.indexOf("\r\n\r\n");
  if (idx === -1) return;

  clearTimeout(timeout);
  socket.end();

  const headerBlock = buf.slice(0, idx);
  const lines = headerBlock.split("\r\n");
  const statusLine = lines[0] ?? "";
  if (!statusLine.includes(" 101 ")) {
    console.error(`unexpected status line from /l2: ${statusLine}`);
    process.exit(1);
  }

  const protoLine = lines.find((line) => /^sec-websocket-protocol:/i.test(line));
  if (!protoLine) {
    console.error("missing Sec-WebSocket-Protocol in /l2 upgrade response");
    process.exit(1);
  }
  const proto = protoLine.split(":", 2)[1]?.trim() ?? "";
  if (proto !== expectedProtocol) {
    console.error(`unexpected subprotocol for /l2 (expected ${expectedProtocol}, got ${proto})`);
    process.exit(1);
  }

  process.exit(0);
});

socket.on("error", (err) => {
  clearTimeout(timeout);
  console.error("error waiting for /l2 upgrade response:", err);
  process.exit(1);
});
NODE
    )"; then
      l2_ok=1
      break
    fi
    sleep 1
  done

  if [[ $l2_ok -ne 1 ]]; then
    echo "deploy smoke: /l2 WebSocket upgrade failed" >&2
    if [[ -n "$l2_last_error" ]]; then
      echo "$l2_last_error" >&2
    fi
    exit 1
  fi
else
  echo "deploy smoke: node not found; skipping /l2 WebSocket validation" >&2
fi

echo "deploy smoke: OK" >&2
