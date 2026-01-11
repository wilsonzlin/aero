# Aero TCP proxy server (WebSocket multiplexed)
This directory contains a **small unprivileged TCP relay** that makes it possible for the browser (WASM guest OS) to open arbitrary outbound TCP connections.

Browsers cannot open raw TCP sockets. The only browser‑legal option is a WebSocket to a relay, which then opens TCP sockets on behalf of the browser.

## Features
- Single **multiplexed** WebSocket session carries many logical TCP streams
- Binary framing protocol: `OPEN`, `DATA`, `CLOSE`, `ERROR`
- **Backpressure**: pauses TCP reads when the WebSocket send buffer grows too large
- Security:
  - Auth token required
  - Deny private IPv4 ranges by default (SSRF mitigation)
  - Simple rate limits per WebSocket session
- Structured JSON logs + in‑memory counters (basic metrics)

## Usage
```bash
# From the repo root (npm workspaces)
npm ci

# Required
export AERO_PROXY_AUTH_TOKEN="dev-token"

# Optional
export AERO_PROXY_PORT=8080
export AERO_PROXY_HOST=127.0.0.1
export AERO_PROXY_ALLOW_PRIVATE_IPS=0

npm -w tools/net-proxy-server start
```

Connect from the browser to:
```
ws://127.0.0.1:8080/tcp-mux?token=dev-token
```

## Security model
This server is **powerful**: it can connect to any TCP endpoint that the host machine can reach.

Default protections:
- Requires an auth token (`AERO_PROXY_AUTH_TOKEN`)
- Blocks private / link-local / loopback IPv4 ranges unless explicitly enabled (`AERO_PROXY_ALLOW_PRIVATE_IPS=1`)

If you run this on a shared machine or expose it to the internet, treat it like a credentialed proxy and deploy accordingly.

## Protocol (v1)
Each WebSocket message is a single binary frame:

### Common header
| Field | Type | Notes |
|------|------|------|
| `type` | `u8` | `1=OPEN`, `2=DATA`, `3=CLOSE`, `4=ERROR` |
| `connection_id` | `u32be` | `0` is reserved for session-level errors |

### `OPEN` (client → server)
Payload:
| Field | Type |
|------|------|
| `ip_version` | `u8` (only `4` supported) |
| `dst_ip` | `u8[4]` |
| `dst_port` | `u16be` |

### `OPEN` (server → client)
Ack payload is empty.

### `DATA`
Payload is raw bytes.

### `CLOSE`
Payload is empty.

### `ERROR`
Payload:
| Field | Type |
|------|------|
| `code` | `u16be` |
| `msg_len` | `u16be` |
| `message` | `utf8[msg_len]` |

## Testing
```bash
npm -w tools/net-proxy-server test
```

## Docker
```bash
docker build -t aero-net-proxy-server .
docker run --rm -p 8080:8080 -e AERO_PROXY_AUTH_TOKEN=dev-token aero-net-proxy-server
```
