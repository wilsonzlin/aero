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
    AERO_GATEWAY_UPSTREAM=aero-gateway:8080 \
    AERO_L2_PROXY_UPSTREAM=aero-l2-proxy:8090 \
    AERO_WEBRTC_UDP_RELAY_UPSTREAM=aero-webrtc-udp-relay:8080 \
    AERO_HSTS_MAX_AGE=0 \
    AERO_CSP_CONNECT_SRC_EXTRA= \
    AERO_GATEWAY_UPSTREAM=aero-gateway:8080 \
    AERO_L2_PROXY_UPSTREAM=aero-l2-proxy:8090 \
    AERO_GATEWAY_IMAGE="aero-gateway:${PROJECT_NAME}" \
    AERO_L2_PROXY_IMAGE="aero-l2-proxy:${PROJECT_NAME}" \
    AERO_WEBRTC_UDP_RELAY_IMAGE="aero-webrtc-udp-relay:${PROJECT_NAME}" \
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

echo "deploy smoke: waiting for https://localhost/webrtc/ice" >&2
for _ in $(seq 1 60); do
  if curl -kfsS https://localhost/webrtc/ice >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

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
webrtc_headers="$(fetch_headers https://localhost/webrtc/ice)"
wasm_headers="$(fetch_headers "https://localhost/assets/$SMOKE_WASM_NAME")"

assert_header_exact "Cache-Control" "no-cache" "$root_headers"

for headers in "$root_headers" "$health_headers" "$webrtc_headers" "$wasm_headers"; do
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

webrtc_body="$(curl -kfsS https://localhost/webrtc/ice)"
if ! echo "$webrtc_body" | grep -Eq '\"iceServers\"[[:space:]]*:'; then
  echo "deploy smoke: unexpected /webrtc/ice body: $webrtc_body" >&2
  exit 1
fi
assert_header_exact "Content-Type" "application/json" "$webrtc_headers"

# /tcp WebSocket upgrade check (requires session cookie).
#
# We validate the TLS + Upgrade path and cookie/session enforcement without relying on
# external tools like `wscat`/`websocat`.
if command -v node >/dev/null 2>&1; then
  echo "deploy smoke: verifying wss://localhost/tcp upgrade (session cookie)" >&2
  node --input-type=commonjs - <<'NODE'
const https = require("node:https");
const tls = require("node:tls");
const crypto = require("node:crypto");

const host = "localhost";
const port = 443;

function requestSessionCookie() {
  return new Promise((resolve, reject) => {
    const body = Buffer.from("{}", "utf8");
    const req = https.request(
      {
        host,
        port,
        method: "POST",
        path: "/session",
        rejectUnauthorized: false,
        headers: {
          "content-type": "application/json",
          "content-length": String(body.length),
        },
      },
      (res) => {
        const status = res.statusCode ?? 0;
        res.setEncoding("utf8");
        let bodyText = "";
        res.on("data", (chunk) => {
          bodyText += chunk;
        });
        res.on("end", () => {
          let payload;
          try {
            payload = JSON.parse(bodyText || "{}");
          } catch (err) {
            reject(new Error(`invalid JSON from /session: ${err}`));
            return;
          }
          if (!payload || typeof payload !== "object") {
            reject(new Error("invalid /session response body"));
            return;
          }
          const udpRelay = payload.udpRelay;
          if (!udpRelay || typeof udpRelay !== "object") {
            reject(new Error("missing udpRelay in /session response (gateway should be configured for same-origin UDP relay)"));
            return;
          }
          const endpoints = udpRelay.endpoints;
          if (!endpoints || typeof endpoints !== "object") {
            reject(new Error("missing udpRelay.endpoints in /session response"));
            return;
          }
          if (endpoints.webrtcIce !== "/webrtc/ice") {
            reject(new Error(`unexpected udpRelay.endpoints.webrtcIce: ${endpoints.webrtcIce}`));
            return;
          }
          if (endpoints.udp !== "/udp") {
            reject(new Error(`unexpected udpRelay.endpoints.udp: ${endpoints.udp}`));
            return;
          }

          const setCookie = res.headers["set-cookie"];
          if (!setCookie) {
            reject(new Error("missing Set-Cookie from /session"));
            return;
          }
          const cookieLine = Array.isArray(setCookie) ? setCookie[0] : setCookie;
          if (typeof cookieLine !== "string" || cookieLine.length === 0) {
            reject(new Error("invalid Set-Cookie from /session"));
            return;
          }
          const cookiePair = cookieLine.split(";", 1)[0] ?? "";
          if (!cookiePair.startsWith("aero_session=")) {
            reject(new Error(`unexpected session cookie from /session: ${cookiePair}`));
            return;
          }
          if (status < 200 || status >= 400) {
            reject(new Error(`unexpected /session status: ${status}`));
            return;
          }
          resolve(cookiePair);
        });
      },
    );
    req.on("error", reject);
    req.end(body);
  });
}

function checkTcpUpgrade(cookiePair) {
  return new Promise((resolve, reject) => {
    const path = "/tcp?v=1&host=example.com&port=80";
    const guid = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    const key = crypto.randomBytes(16).toString("base64");
    const expectedAccept = crypto.createHash("sha1").update(key + guid).digest("base64");

    const req = [
      `GET ${path} HTTP/1.1`,
      `Host: ${host}`,
      "Connection: Upgrade",
      "Upgrade: websocket",
      "Sec-WebSocket-Version: 13",
      `Sec-WebSocket-Key: ${key}`,
      `Cookie: ${cookiePair}`,
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
      socket.destroy();
      reject(new Error("timeout waiting for /tcp upgrade response"));
    }, 5000);

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
        reject(new Error(`unexpected status line from /tcp: ${statusLine}`));
        return;
      }

      const acceptLine = lines.find((line) => /^sec-websocket-accept:/i.test(line));
      if (!acceptLine) {
        reject(new Error("missing Sec-WebSocket-Accept in /tcp upgrade response"));
        return;
      }
      const accept = acceptLine.split(":", 2)[1]?.trim() ?? "";
      if (accept !== expectedAccept) {
        reject(new Error(`unexpected Sec-WebSocket-Accept for /tcp (expected ${expectedAccept}, got ${accept})`));
        return;
      }

      resolve();
    });

    socket.on("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
}

(async () => {
  const cookie = await requestSessionCookie();
  await checkTcpUpgrade(cookie);
})().catch((err) => {
  console.error("tcp upgrade check failed:", err);
  process.exit(1);
});
NODE
else
  echo "deploy smoke: node not found; skipping /tcp WebSocket validation" >&2
fi

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
