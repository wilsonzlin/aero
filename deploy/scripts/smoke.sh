#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMPOSE_FILE="$ROOT_DIR/deploy/docker-compose.yml"

PROJECT_NAME="aero-smoke-$RANDOM$RANDOM"
SMOKE_FRONTEND_ROOT="$ROOT_DIR/deploy/static"
SMOKE_WASM_NAME="__aero_smoke_${PROJECT_NAME}.wasm"
SMOKE_WASM_PATH="$SMOKE_FRONTEND_ROOT/assets/$SMOKE_WASM_NAME"
SMOKE_WASM_DIR="$(dirname "$SMOKE_WASM_PATH")"
SMOKE_WEBRTC_API_KEY="__aero_smoke_${PROJECT_NAME}"
SMOKE_WEBRTC_UDP_PORT_RANGE_SIZE=101
SMOKE_WEBRTC_UDP_PORT_MIN=50000
SMOKE_WEBRTC_UDP_PORT_MAX=$((SMOKE_WEBRTC_UDP_PORT_MIN + SMOKE_WEBRTC_UDP_PORT_RANGE_SIZE - 1))

compose() {
  # The /udp smoke test sends a UDP datagram to the host via the docker network.
  # Use the relay dev destination policy so the packet isn't denied by default.
  env \
    -u AERO_L2_ALLOWED_TCP_PORTS \
    -u AERO_L2_ALLOWED_UDP_PORTS \
    -u AERO_L2_ALLOWED_DOMAINS \
    -u AERO_L2_BLOCKED_DOMAINS \
    -u AERO_L2_TOKEN \
    -u AERO_L2_AUTH_MODE \
    -u AERO_L2_SESSION_SECRET \
    -u SESSION_SECRET \
    -u AERO_L2_API_KEY \
    -u AERO_L2_JWT_SECRET \
    -u L2_BACKEND_WS_URL \
    -u L2_BACKEND_ORIGIN \
    -u L2_BACKEND_ORIGIN_OVERRIDE \
    -u L2_BACKEND_WS_ORIGIN \
    -u L2_BACKEND_TOKEN \
    -u L2_BACKEND_WS_TOKEN \
    -u L2_BACKEND_AUTH_FORWARD_MODE \
    -u L2_BACKEND_FORWARD_ORIGIN \
    -u L2_MAX_MESSAGE_BYTES \
    AERO_DOMAIN=localhost \
    AERO_GATEWAY_UPSTREAM=aero-gateway:8080 \
    AERO_GATEWAY_GIT_SHA= \
    AERO_L2_PROXY_UPSTREAM=aero-l2-proxy:8090 \
    AERO_L2_PROXY_LISTEN_ADDR= \
    AERO_L2_ALLOW_PRIVATE_IPS=0 \
    AERO_L2_AUTH_MODE=session \
    AERO_WEBRTC_UDP_RELAY_UPSTREAM=aero-webrtc-udp-relay:8080 \
    BUILD_COMMIT= \
    BUILD_TIME= \
    WEBRTC_UDP_PORT_MIN="$SMOKE_WEBRTC_UDP_PORT_MIN" \
    WEBRTC_UDP_PORT_MAX="$SMOKE_WEBRTC_UDP_PORT_MAX" \
    AERO_STUN_URLS= \
    AERO_HSTS_MAX_AGE=0 \
    AERO_CSP_CONNECT_SRC_EXTRA= \
    AERO_L2_ALLOWED_ORIGINS_EXTRA= \
    AERO_WEBRTC_UDP_RELAY_ALLOWED_ORIGINS_EXTRA= \
    JWT_SECRET= \
    UDP_RELAY_TOKEN_TTL_SECONDS= \
    UDP_RELAY_AUDIENCE= \
    UDP_RELAY_ISSUER= \
    SIGNALING_AUTH_TIMEOUT= \
    MAX_SIGNALING_MESSAGE_BYTES= \
    MAX_SIGNALING_MESSAGES_PER_SECOND= \
    DESTINATION_POLICY_PRESET=dev \
    ALLOW_PRIVATE_NETWORKS=true \
    ALLOW_UDP_CIDRS= \
    DENY_UDP_CIDRS= \
    ALLOW_UDP_PORTS= \
    DENY_UDP_PORTS= \
    AERO_ICE_SERVERS_JSON= \
    AERO_TURN_URLS= \
    AERO_TURN_USERNAME= \
    AERO_TURN_CREDENTIAL= \
    TURN_REST_SHARED_SECRET= \
    TURN_REST_TTL_SECONDS= \
    TURN_REST_USERNAME_PREFIX= \
    TURN_REST_REALM= \
    WEBRTC_NAT_1TO1_IPS= \
    WEBRTC_NAT_1TO1_IP_CANDIDATE_TYPE= \
    WEBRTC_UDP_LISTEN_IP= \
    ALLOWED_ORIGINS= \
    AERO_GATEWAY_IMAGE="aero-gateway:${PROJECT_NAME}" \
    AERO_L2_PROXY_IMAGE="aero-l2-proxy:${PROJECT_NAME}" \
    AERO_WEBRTC_UDP_RELAY_IMAGE="aero-webrtc-udp-relay:${PROJECT_NAME}" \
    AUTH_MODE=api_key \
    API_KEY="$SMOKE_WEBRTC_API_KEY" \
    TRUST_PROXY=1 \
    CROSS_ORIGIN_ISOLATION=0 \
    AERO_FRONTEND_ROOT="$SMOKE_FRONTEND_ROOT" \
    docker compose --env-file /dev/null -f "$COMPOSE_FILE" -p "$PROJECT_NAME" "$@"
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

echo "deploy smoke: checking deploy/docker-compose.yml defaults" >&2
# Ensure the canonical deploy stack continues to work out-of-the-box:
# - `/l2` should not require session-cookie auth unless explicitly enabled.
# - Operators can opt into session-cookie auth by setting `AERO_L2_AUTH_MODE=session` (legacy alias: `cookie`) and should set
#   `SESSION_SECRET` explicitly for production; the deploy stack can also generate one automatically.
env \
  -u AERO_L2_AUTH_MODE \
  -u AERO_L2_SESSION_SECRET \
  -u AERO_L2_API_KEY \
  -u AERO_L2_JWT_SECRET \
  -u AERO_L2_TOKEN \
  -u SESSION_SECRET \
  -u AERO_GATEWAY_SESSION_SECRET \
  docker compose --env-file /dev/null -f "$COMPOSE_FILE" config --format json \
  | python3 -c 'import json, sys; cfg=json.load(sys.stdin); env=(cfg.get("services", {}).get("aero-l2-proxy", {}) or {}).get("environment", {}) or {}; mode=env.get("AERO_L2_AUTH_MODE"); expected="none"; \
print(f"deploy smoke: expected deploy/docker-compose.yml to default AERO_L2_AUTH_MODE to {expected!r}, got: {mode!r}", file=sys.stderr) if mode != expected else None; sys.exit(1 if mode != expected else 0)'

pick_udp_port_range() {
  local size="$SMOKE_WEBRTC_UDP_PORT_RANGE_SIZE"
  local min_start=40000
  local max_port=65535
  local max_start=$((max_port - size + 1))

  if [[ $max_start -le $min_start ]]; then
    echo "deploy smoke: invalid WebRTC UDP port range size: $size" >&2
    exit 1
  fi

  declare -A used_udp=()
  if command -v ss >/dev/null 2>&1; then
    while read -r addr _; do
      local port="${addr##*:}"
      if [[ $port =~ ^[0-9]+$ ]]; then
        used_udp["$port"]=1
      fi
    done < <(ss -unaH | awk '{print $4}')
  fi

  for _ in $(seq 1 50); do
    local start=$((min_start + RANDOM % (max_start - min_start + 1)))
    local end=$((start + size - 1))
    local ok=1
    local p
    for ((p = start; p <= end; p++)); do
      if [[ -n "${used_udp[$p]:-}" ]]; then
        ok=0
        break
      fi
    done
    if [[ $ok -eq 1 ]]; then
      SMOKE_WEBRTC_UDP_PORT_MIN="$start"
      SMOKE_WEBRTC_UDP_PORT_MAX="$end"
      return 0
    fi
  done

  # Should be extremely rare (requires a very "busy" host). Fall back to a random
  # range without checking so we at least avoid the fixed 50000-50100 defaults.
  local start=$((min_start + RANDOM % (max_start - min_start + 1)))
  SMOKE_WEBRTC_UDP_PORT_MIN="$start"
  SMOKE_WEBRTC_UDP_PORT_MAX=$((start + size - 1))
  return 0
}

pick_udp_port_range
echo "deploy smoke: using WebRTC UDP port range ${SMOKE_WEBRTC_UDP_PORT_MIN}-${SMOKE_WEBRTC_UDP_PORT_MAX}" >&2

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
  if curl -kfsS -H "X-API-Key: $SMOKE_WEBRTC_API_KEY" https://localhost/webrtc/ice >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

fetch_headers() {
  local url="$1"
  shift
  curl -kfsS "$@" -D- -o /dev/null "$url" | tr -d '\r'
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
webrtc_headers="$(fetch_headers https://localhost/webrtc/ice -H "X-API-Key: $SMOKE_WEBRTC_API_KEY")"
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

webrtc_body="$(curl -kfsS -H "X-API-Key: $SMOKE_WEBRTC_API_KEY" https://localhost/webrtc/ice)"
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
  echo "deploy smoke: verifying wss://localhost/tcp and /l2 upgrades (session cookie)" >&2
  # The /udp smoke test sends a UDP datagram to the host via the docker network
  # gateway, so we need the gateway IP.
  #
  # Compose uses "${PROJECT_NAME}_default" as the network name.
  SMOKE_DOCKER_GATEWAY_IP="$(docker network inspect -f '{{(index .IPAM.Config 0).Gateway}}' "${PROJECT_NAME}_default" 2>/dev/null || true)"
  if [[ -z "$SMOKE_DOCKER_GATEWAY_IP" ]]; then
    echo "deploy smoke: failed to resolve docker gateway IP for ${PROJECT_NAME}_default" >&2
    exit 1
  fi

  AERO_SMOKE_DOCKER_GATEWAY_IP="$SMOKE_DOCKER_GATEWAY_IP" node --input-type=commonjs - <<'NODE'
 const https = require("node:https");
 const tls = require("node:tls");
 const crypto = require("node:crypto");
 const dgram = require("node:dgram");
 const { once } = require("node:events");

 const host = "localhost";
 const port = 443;
 const l2Protocol = "aero-l2-tunnel-v1";
 const dockerGatewayIP = process.env.AERO_SMOKE_DOCKER_GATEWAY_IP;
 
 function parseStatusCode(statusLine) {
   const match = /^HTTP\/\d+(?:\.\d+)?\s+(\d{3})\b/.exec(statusLine);
   if (!match) return null;
   return Number(match[1]);
 }

 function sleep(ms) {
   return new Promise((resolve) => setTimeout(resolve, ms));
 }

 function readSocketChunk(socket, timeoutMs, label) {
   return new Promise((resolve, reject) => {
     const onData = (chunk) => {
       cleanup();
       resolve(chunk);
     };
     const onError = (err) => {
       cleanup();
       reject(err);
     };
     const onClose = () => {
       cleanup();
       reject(new Error(`socket closed while waiting for ${label}`));
     };
 
     const timer = setTimeout(() => {
       cleanup();
       reject(new Error(`timeout waiting for ${label}`));
     }, timeoutMs);
 
     const cleanup = () => {
       clearTimeout(timer);
       socket.off("data", onData);
       socket.off("error", onError);
       socket.off("close", onClose);
     };
 
     socket.once("data", onData);
     socket.once("error", onError);
     socket.once("close", onClose);
   });
 }

 function requestSessionInfo() {
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

            const l2Path = payload?.endpoints?.l2;
            if (typeof l2Path !== "string" || l2Path.length === 0) {
              reject(new Error("missing endpoints.l2 in /session response"));
              return;
            }

            const l2Limits = payload?.limits?.l2;
            if (!l2Limits || typeof l2Limits !== "object") {
              reject(new Error("missing limits.l2 in /session response"));
              return;
            }
            const maxFramePayloadBytes = l2Limits.maxFramePayloadBytes;
            const maxControlPayloadBytes = l2Limits.maxControlPayloadBytes;
            if (!Number.isInteger(maxFramePayloadBytes) || maxFramePayloadBytes <= 0) {
              reject(new Error(`invalid limits.l2.maxFramePayloadBytes: ${maxFramePayloadBytes}`));
              return;
            }
            if (!Number.isInteger(maxControlPayloadBytes) || maxControlPayloadBytes <= 0) {
              reject(new Error(`invalid limits.l2.maxControlPayloadBytes: ${maxControlPayloadBytes}`));
              return;
            }
 
            const udpRelay = payload.udpRelay;
            if (!udpRelay || typeof udpRelay !== "object") {
              reject(new Error("missing udpRelay in /session response (gateway should be configured for same-origin UDP relay)"));
             return;
           }
          if (udpRelay.baseUrl !== `https://${host}`) {
            reject(new Error(`unexpected udpRelay.baseUrl: ${udpRelay.baseUrl}`));
            return;
          }
          if (udpRelay.authMode !== "api_key") {
            reject(new Error(`unexpected udpRelay.authMode: ${udpRelay.authMode}`));
            return;
          }
          const endpoints = udpRelay.endpoints;
          if (!endpoints || typeof endpoints !== "object") {
            reject(new Error("missing udpRelay.endpoints in /session response"));
            return;
          }
          if (endpoints.webrtcSignal !== "/webrtc/signal") {
            reject(new Error(`unexpected udpRelay.endpoints.webrtcSignal: ${endpoints.webrtcSignal}`));
            return;
          }
          if (endpoints.webrtcOffer !== "/webrtc/offer") {
            reject(new Error(`unexpected udpRelay.endpoints.webrtcOffer: ${endpoints.webrtcOffer}`));
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

          const token = udpRelay.token;
          if (typeof token !== "string" || token.length === 0) {
            reject(new Error(`missing udpRelay.token in /session response: ${JSON.stringify(udpRelay)}`));
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
           resolve({ cookiePair, udpRelayToken: token, l2Path });
         });
       },
     );
     req.on("error", reject);
    req.end(body);
   });
 }

 function startUDPEchoServer() {
   return new Promise((resolve, reject) => {
     const socket = dgram.createSocket("udp4");
     socket.on("error", reject);
     socket.on("message", (msg, rinfo) => {
       socket.send(msg, rinfo.port, rinfo.address);
     });
     socket.bind(0, "0.0.0.0", () => {
       const addr = socket.address();
       resolve({ socket, port: addr.port });
     });
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

function checkL2Upgrade(cookiePair, path) {
  return new Promise((resolve, reject) => {
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
      `Sec-WebSocket-Protocol: ${l2Protocol}`,
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
      reject(new Error(`timeout waiting for ${path} upgrade response`));
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
      const status = parseStatusCode(statusLine);
      if (status !== 101) {
        reject(new Error(`unexpected status line from ${path}: ${statusLine}`));
        return;
      }

      const acceptLine = lines.find((line) => /^sec-websocket-accept:/i.test(line));
      if (!acceptLine) {
        reject(new Error(`missing Sec-WebSocket-Accept in ${path} upgrade response`));
        return;
      }
      const accept = acceptLine.split(":", 2)[1]?.trim() ?? "";
      if (accept !== expectedAccept) {
        reject(new Error(`unexpected Sec-WebSocket-Accept for ${path} (expected ${expectedAccept}, got ${accept})`));
        return;
      }

      const protoLine = lines.find((line) => /^sec-websocket-protocol:/i.test(line));
      if (!protoLine) {
        reject(new Error(`missing Sec-WebSocket-Protocol in ${path} upgrade response`));
        return;
      }
      const proto = protoLine.split(":", 2)[1]?.trim() ?? "";
      if (proto !== l2Protocol) {
        reject(new Error(`unexpected subprotocol for ${path} (expected ${l2Protocol}, got ${proto})`));
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

function checkL2RejectsMissingCookie(path) {
  return new Promise((resolve, reject) => {
    const key = crypto.randomBytes(16).toString("base64");

    const req = [
      `GET ${path} HTTP/1.1`,
      `Host: ${host}`,
      "Connection: Upgrade",
      "Upgrade: websocket",
      "Sec-WebSocket-Version: 13",
      `Sec-WebSocket-Key: ${key}`,
      `Sec-WebSocket-Protocol: ${l2Protocol}`,
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
      reject(new Error(`timeout waiting for ${path} unauthenticated response`));
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
      const status = parseStatusCode(statusLine);
      if (status === 401) {
        resolve();
        return;
      }
      reject(new Error(`expected ${path} without Cookie to be rejected with 401 (got: ${statusLine})`));
    });

    socket.on("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
}

function checkL2RejectsMissingOrigin(cookiePair, path) {
  return new Promise((resolve, reject) => {
    const key = crypto.randomBytes(16).toString("base64");

    const req = [
      `GET ${path} HTTP/1.1`,
      `Host: ${host}`,
      "Connection: Upgrade",
      "Upgrade: websocket",
      "Sec-WebSocket-Version: 13",
      `Sec-WebSocket-Key: ${key}`,
      `Sec-WebSocket-Protocol: ${l2Protocol}`,
      `Cookie: ${cookiePair}`,
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
      reject(new Error(`timeout waiting for ${path} unauthenticated response`));
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
      const status = parseStatusCode(statusLine);
      if (status === 403) {
        resolve();
        return;
      }
      reject(new Error(`expected ${path} without Origin to be rejected with 403 (got: ${statusLine})`));
    });

    socket.on("error", (err) => {
      clearTimeout(timeout);
      reject(err);
    });
  });
}

function buildDnsQueryAny(name) {
  const id = Math.floor(Math.random() * 65536);
  const bytes = [];
  bytes.push((id >> 8) & 0xff, id & 0xff); // ID
  bytes.push(0x01, 0x00); // flags: RD
  bytes.push(0x00, 0x01); // QDCOUNT
  bytes.push(0x00, 0x00); // ANCOUNT
  bytes.push(0x00, 0x00); // NSCOUNT
  bytes.push(0x00, 0x00); // ARCOUNT

  for (const label of name.split(".")) {
    const encoded = Buffer.from(label, "utf8");
    if (encoded.length === 0 || encoded.length > 63) throw new Error(`invalid DNS label: ${label}`);
    bytes.push(encoded.length);
    for (const b of encoded) bytes.push(b);
  }
  bytes.push(0x00); // end of QNAME

  bytes.push(0x00, 0xff); // QTYPE=ANY
  bytes.push(0x00, 0x01); // QCLASS=IN

  const message = Buffer.from(bytes);
  const dnsParam = message
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/g, "");
  return { id, dnsParam };
}

function checkDnsQuery(cookiePair) {
  return new Promise((resolve, reject) => {
    let id;
    let dnsParam;
    try {
      ({ id, dnsParam } = buildDnsQueryAny("example.com"));
    } catch (err) {
      reject(err);
      return;
    }

    const path = `/dns-query?dns=${encodeURIComponent(dnsParam)}`;
    const req = https.request(
      {
        host,
        port,
        method: "GET",
        path,
        rejectUnauthorized: false,
        headers: {
          origin: `https://${host}`,
          cookie: cookiePair,
          accept: "application/dns-message",
        },
      },
      (res) => {
        const status = res.statusCode ?? 0;
        const contentType = String(res.headers["content-type"] ?? "")
          .split(";", 1)[0]
          .trim()
          .toLowerCase();
        const cacheControl = String(res.headers["cache-control"] ?? "").trim().toLowerCase();

        const chunks = [];
        res.on("data", (chunk) => chunks.push(Buffer.from(chunk)));
        res.on("end", () => {
          const body = Buffer.concat(chunks);
          if (status !== 200) {
            reject(new Error(`unexpected /dns-query status: ${status} bytes: ${body.length}`));
            return;
          }
          if (contentType !== "application/dns-message") {
            reject(new Error(`unexpected /dns-query content-type: ${contentType}`));
            return;
          }
          if (cacheControl !== "no-store") {
            reject(new Error(`unexpected /dns-query cache-control: ${cacheControl}`));
            return;
          }
          if (body.length < 12) {
            reject(new Error(`unexpected /dns-query body length: ${body.length}`));
            return;
          }
          const gotId = body.readUInt16BE(0);
          if (gotId !== id) {
            reject(new Error(`unexpected /dns-query ID: ${gotId} (expected ${id})`));
            return;
          }
          const flags = body.readUInt16BE(2);
          const rcode = flags & 0x000f;
          if (rcode !== 5) {
            reject(new Error(`unexpected /dns-query rcode: ${rcode} (expected 5/REFUSED)`));
            return;
          }
          resolve();
        });
      },
    );
    req.on("error", reject);
    req.end();
  });
}

function checkUdpRelayToken(cookiePair) {
  return new Promise((resolve, reject) => {
    const req = https.request(
      {
        host,
        port,
        method: "POST",
        path: "/udp-relay/token",
        rejectUnauthorized: false,
        headers: {
          origin: `https://${host}`,
          cookie: cookiePair,
          "content-length": "0",
        },
      },
      (res) => {
        const status = res.statusCode ?? 0;
        res.setEncoding("utf8");
        let body = "";
        res.on("data", (chunk) => {
          body += chunk;
        });
        res.on("end", () => {
          if (status < 200 || status >= 300) {
            reject(new Error(`unexpected /udp-relay/token status: ${status} body: ${body}`));
            return;
          }
          let parsed;
          try {
            parsed = JSON.parse(body);
          } catch (err) {
            reject(new Error(`invalid JSON from /udp-relay/token: ${body}`));
            return;
          }
          if (parsed?.authMode !== "api_key") {
            reject(new Error(`unexpected authMode from /udp-relay/token: ${JSON.stringify(parsed)}`));
            return;
          }
          if (typeof parsed?.token !== "string" || parsed.token.length === 0) {
            reject(new Error(`missing token from /udp-relay/token: ${JSON.stringify(parsed)}`));
            return;
          }
          resolve(parsed.token);
        });
      },
    );
    req.on("error", reject);
    req.end();
   });
 }

 async function wsConnect(path, label) {
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

   socket.setNoDelay(true);
   await once(socket, "secureConnect");
   socket.write(req);

   let buf = Buffer.alloc(0);
   const deadline = Date.now() + 5000;
   while (buf.indexOf("\r\n\r\n") === -1) {
     const remaining = deadline - Date.now();
     if (remaining <= 0) {
       socket.destroy();
       throw new Error(`timeout waiting for ${label} upgrade response`);
     }
     const chunk = await readSocketChunk(socket, remaining, `${label} upgrade response`).catch((err) => {
       socket.destroy();
       throw err;
     });
     buf = Buffer.concat([buf, chunk]);
   }

   const idx = buf.indexOf("\r\n\r\n");
   const headerBlock = buf.slice(0, idx).toString("utf8");
   const rest = buf.slice(idx + 4);
   const lines = headerBlock.split("\r\n");
   const statusLine = lines[0] ?? "";
   if (!statusLine.includes(" 101 ")) {
     socket.destroy();
     throw new Error(`unexpected status line from ${label}: ${statusLine}`);
   }
   const acceptLine = lines.find((line) => /^sec-websocket-accept:/i.test(line));
   if (!acceptLine) {
     socket.destroy();
     throw new Error(`missing Sec-WebSocket-Accept in ${label} upgrade response`);
   }
   const accept = acceptLine.split(":", 2)[1]?.trim() ?? "";
   if (accept !== expectedAccept) {
     socket.destroy();
     throw new Error(`unexpected Sec-WebSocket-Accept for ${label} (expected ${expectedAccept}, got ${accept})`);
   }

   return { socket, buffer: rest };
 }

 function wsWriteFrame(state, opcode, payload) {
   const finOpcode = 0x80 | (opcode & 0x0f);
   const len = payload.length;
   const maskKey = crypto.randomBytes(4);

   let header;
   if (len <= 125) {
     header = Buffer.alloc(2);
     header[0] = finOpcode;
     header[1] = 0x80 | len;
   } else if (len < 65536) {
     header = Buffer.alloc(4);
     header[0] = finOpcode;
     header[1] = 0x80 | 126;
     header.writeUInt16BE(len, 2);
   } else {
     header = Buffer.alloc(10);
     header[0] = finOpcode;
     header[1] = 0x80 | 127;
     // We never send payloads > 2^32 in smoke tests; keep it simple.
     header.writeUInt32BE(0, 2);
     header.writeUInt32BE(len >>> 0, 6);
   }

   const masked = Buffer.alloc(len);
   for (let i = 0; i < len; i++) {
     masked[i] = payload[i] ^ maskKey[i & 3];
   }
   state.socket.write(Buffer.concat([header, maskKey, masked]));
 }

 async function wsReadFrame(state, label, timeoutMs) {
   const deadline = Date.now() + timeoutMs;
   const needBytes = async (n) => {
     while (state.buffer.length < n) {
       const remaining = deadline - Date.now();
       if (remaining <= 0) {
         throw new Error(`timeout reading ${label} WebSocket frame`);
       }
       const chunk = await readSocketChunk(state.socket, remaining, `${label} WebSocket frame`);
       state.buffer = Buffer.concat([state.buffer, chunk]);
     }
   };

   for (;;) {
     await needBytes(2);
     const b0 = state.buffer[0];
     const b1 = state.buffer[1];
     const fin = (b0 & 0x80) !== 0;
     const opcode = b0 & 0x0f;
     const masked = (b1 & 0x80) !== 0;
     let len = b1 & 0x7f;
     let headerLen = 2;

     if (len === 126) {
       await needBytes(4);
       len = state.buffer.readUInt16BE(2);
       headerLen = 4;
     } else if (len === 127) {
       await needBytes(10);
       const high = state.buffer.readUInt32BE(2);
       const low = state.buffer.readUInt32BE(6);
       if (high !== 0) {
         throw new Error(`${label} WebSocket frame too large`);
       }
       len = low;
       headerLen = 10;
     }

     let maskKey = null;
     if (masked) {
       await needBytes(headerLen + 4);
       maskKey = state.buffer.slice(headerLen, headerLen + 4);
       headerLen += 4;
     }

     await needBytes(headerLen + len);
     let payload = state.buffer.slice(headerLen, headerLen + len);
     state.buffer = state.buffer.slice(headerLen + len);

     if (masked && maskKey) {
       const out = Buffer.alloc(payload.length);
       for (let i = 0; i < payload.length; i++) {
         out[i] = payload[i] ^ maskKey[i & 3];
       }
       payload = out;
     }

     if (!fin) {
       throw new Error(`${label} WebSocket frame fragmentation not supported in smoke test`);
     }

     if (opcode === 0x9) {
       // ping -> pong
       wsWriteFrame(state, 0xa, payload);
       continue;
     }
     if (opcode === 0xa) {
       // pong
       continue;
     }
     if (opcode === 0x8) {
       throw new Error(`${label} WebSocket closed by server`);
     }

     return { opcode, payload };
   }
 }

 function encodeV1Datagram({ guestPort, remoteIP, remotePort, payload }) {
   const ipParts = remoteIP.split(".").map((part) => Number(part));
   if (ipParts.length !== 4 || ipParts.some((p) => !Number.isFinite(p) || p < 0 || p > 255)) {
     throw new Error(`invalid IPv4 address for UDP relay: ${remoteIP}`);
   }
   const buf = Buffer.alloc(8 + payload.length);
   buf.writeUInt16BE(guestPort, 0);
   for (let i = 0; i < 4; i++) buf[2 + i] = ipParts[i];
   buf.writeUInt16BE(remotePort, 6);
   payload.copy(buf, 8);
   return buf;
 }

 function decodeDatagramFrame(frame) {
   if (frame.length >= 2 && frame[0] === 0xa2 && frame[1] === 0x02) {
     if (frame.length < 12) {
       throw new Error(`malformed v2 UDP relay frame (len=${frame.length})`);
     }
     const af = frame[2];
     const type = frame[3];
     if (type !== 0x00) {
       throw new Error(`unsupported v2 UDP relay frame type=${type}`);
     }
     const guestPort = frame.readUInt16BE(4);
     let remoteIP;
     let offset = 6;
     if (af === 0x04) {
       remoteIP = Array.from(frame.slice(offset, offset + 4)).join(".");
       offset += 4;
     } else if (af === 0x06) {
       remoteIP = frame.slice(offset, offset + 16).toString("hex");
       offset += 16;
     } else {
       throw new Error(`unsupported v2 UDP relay address family=${af}`);
     }
     const remotePort = frame.readUInt16BE(offset);
     offset += 2;
     return { version: 2, guestPort, remoteIP, remotePort, payload: frame.slice(offset) };
   }

   if (frame.length < 8) {
     throw new Error(`malformed v1 UDP relay frame (len=${frame.length})`);
   }
   const guestPort = frame.readUInt16BE(0);
   const remoteIP = Array.from(frame.slice(2, 6)).join(".");
   const remotePort = frame.readUInt16BE(6);
   return { version: 1, guestPort, remoteIP, remotePort, payload: frame.slice(8) };
 }

 async function checkUdpWebSocketRoundTrip(token) {
   if (!dockerGatewayIP) {
     throw new Error("missing AERO_SMOKE_DOCKER_GATEWAY_IP; cannot validate /udp data plane");
   }

   const echo = await startUDPEchoServer();
   try {
     const ws = await wsConnect(`/udp?token=${encodeURIComponent(token)}`, "/udp");
     try {
       const ready = await wsReadFrame(ws, "/udp", 5000);
       if (ready.opcode !== 0x1) {
         throw new Error(`expected text ready frame from /udp, got opcode=${ready.opcode}`);
       }
       let readyMsg;
       try {
         readyMsg = JSON.parse(ready.payload.toString("utf8"));
       } catch (err) {
         throw new Error(`invalid JSON ready message from /udp: ${err}`);
       }
       if (readyMsg?.type !== "ready") {
         throw new Error(`unexpected /udp control message: ${JSON.stringify(readyMsg)}`);
       }

       const payload = Buffer.from("aero-udp-smoke");
       const guestPort = 54321;
       const pkt = encodeV1Datagram({
         guestPort,
         remoteIP: dockerGatewayIP,
         remotePort: echo.port,
         payload,
       });
       wsWriteFrame(ws, 0x2, pkt);

       const reply = await wsReadFrame(ws, "/udp", 5000);
       if (reply.opcode !== 0x2) {
         throw new Error(`expected binary UDP relay reply frame, got opcode=${reply.opcode}`);
       }
       const decoded = decodeDatagramFrame(reply.payload);
       if (decoded.guestPort !== guestPort) {
         throw new Error(`unexpected guestPort in UDP reply: ${decoded.guestPort}`);
       }
       if (decoded.remotePort !== echo.port) {
         throw new Error(`unexpected remotePort in UDP reply: ${decoded.remotePort}`);
       }
       if (decoded.remoteIP !== dockerGatewayIP) {
         throw new Error(`unexpected remoteIP in UDP reply: ${decoded.remoteIP}`);
       }
       if (!decoded.payload.equals(payload)) {
         throw new Error(`unexpected UDP payload in reply: ${decoded.payload.toString("utf8")}`);
       }
     } finally {
       ws.socket.end();
     }
   } finally {
     echo.socket.close();
   }
 }

 function checkRelayWebSocketUpgrade(path, label) {
   return new Promise((resolve, reject) => {
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

    let done = false;
    let handshakeTimer;
    let holdTimer;

    const fail = (err) => {
      if (done) return;
      done = true;
      clearTimeout(handshakeTimer);
      clearTimeout(holdTimer);
      socket.destroy();
      reject(err);
    };

    const succeed = () => {
      if (done) return;
      done = true;
      clearTimeout(handshakeTimer);
      clearTimeout(holdTimer);
      socket.end();
      resolve();
    };

    handshakeTimer = setTimeout(() => {
      fail(new Error(`timeout waiting for ${label} upgrade response`));
    }, 5000);

    socket.on("secureConnect", () => {
      socket.write(req);
    });

    let buf = "";
    socket.on("data", (chunk) => {
      if (done) return;
      buf += chunk.toString("utf8");
      const idx = buf.indexOf("\r\n\r\n");
      if (idx === -1) return;

      const headerBlock = buf.slice(0, idx);
      const lines = headerBlock.split("\r\n");
      const statusLine = lines[0] ?? "";
      if (!statusLine.includes(" 101 ")) {
        fail(new Error(`unexpected status line from ${label}: ${statusLine}`));
        return;
      }

      const acceptLine = lines.find((line) => /^sec-websocket-accept:/i.test(line));
      if (!acceptLine) {
        fail(new Error(`missing Sec-WebSocket-Accept in ${label} upgrade response`));
        return;
      }
      const accept = acceptLine.split(":", 2)[1]?.trim() ?? "";
      if (accept !== expectedAccept) {
        fail(new Error(`unexpected Sec-WebSocket-Accept for ${label} (expected ${expectedAccept}, got ${accept})`));
        return;
      }

      // Give the server a moment to reject invalid auth tokens (it will close the
      // connection shortly after the handshake). Treat an early close as failure.
      clearTimeout(handshakeTimer);
      holdTimer = setTimeout(() => {
        succeed();
      }, 300);
    });

    socket.on("close", () => {
      if (done) return;
      fail(new Error(`${label} WebSocket closed immediately after handshake (auth/routing failure?)`));
    });

    socket.on("error", (err) => {
      fail(err);
    });
  });
}

(async () => {
  const session = await requestSessionInfo();
  await checkDnsQuery(session.cookiePair);
  await checkTcpUpgrade(session.cookiePair);
  let lastL2Error;
  for (let attempt = 1; attempt <= 30; attempt++) {
    try {
      await checkL2RejectsMissingCookie(session.l2Path);
      await checkL2Upgrade(session.cookiePair, session.l2Path);
      lastL2Error = undefined;
      break;
    } catch (err) {
      lastL2Error = err;
      if (attempt !== 30) {
        await sleep(1000);
      }
    }
  }
  if (lastL2Error) {
    throw lastL2Error;
  }

  await checkL2RejectsMissingOrigin(session.cookiePair, session.l2Path);
  const token = await checkUdpRelayToken(session.cookiePair);
  if (token !== session.udpRelayToken) {
    throw new Error(`mismatched udp relay token: /session=${session.udpRelayToken} /udp-relay/token=${token}`);
  }
   await checkRelayWebSocketUpgrade(`/webrtc/signal?token=${encodeURIComponent(token)}`, "/webrtc/signal");
   await checkUdpWebSocketRoundTrip(token);
 })().catch((err) => {
   console.error("deploy smoke: node networking checks failed:", err);
   process.exit(1);
 });
NODE
else
  echo "deploy smoke: node not found; skipping WebSocket validation (/tcp, /l2)" >&2
fi

echo "deploy smoke: OK" >&2
