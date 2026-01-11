# Aero TCP proxy server (WebSocket multiplexed)
This directory contains a **small unprivileged TCP relay** that makes it possible for the browser (WASM guest OS) to open arbitrary outbound TCP connections.

Browsers cannot open raw TCP sockets. The only browser‑legal option is a WebSocket to a relay, which then opens TCP sockets on behalf of the browser.

## Features
- Single **multiplexed** WebSocket session carries many logical TCP streams
- Speaks the canonical **`aero-tcp-mux-v1`** framing used by `backend/aero-gateway`
  - `OPEN`, `DATA`, `CLOSE`, `ERROR`, `PING`, `PONG`
- **Backpressure**: pauses TCP reads when the WebSocket send buffer grows too large
- Security:
  - Auth token required (dev-only, see below)
  - Deny private/special IP ranges by default (SSRF mitigation)
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
 # Optional IPv4 CIDR allowlist (evaluated even when AERO_PROXY_ALLOW_PRIVATE_IPS=0)
 # export AERO_PROXY_ALLOW_CIDRS="127.0.0.1/32,192.168.0.0/16"

npm -w tools/net-proxy-server start
```

Connect from the browser to:
```
ws://127.0.0.1:8080/tcp-mux?token=dev-token
```

Clients MUST negotiate the WebSocket subprotocol:

```ts
const ws = new WebSocket("ws://127.0.0.1:8080/tcp-mux?token=dev-token", "aero-tcp-mux-v1");
ws.binaryType = "arraybuffer";
```

For a full browser-side implementation (stream parsing + backpressure + PING/PONG handling), see:

- `web/src/net/tcpMuxProxy.ts` (`WebSocketTcpMuxProxyClient`)

## Security model
This server is **powerful**: it can connect to any TCP endpoint that the host machine can reach.

Default protections:
- Requires an auth token (`AERO_PROXY_AUTH_TOKEN`)
- Blocks private / special-purpose IP ranges unless explicitly enabled (`AERO_PROXY_ALLOW_PRIVATE_IPS=1`)
- Optional IPv4 CIDR allowlist for local dev (`AERO_PROXY_ALLOW_CIDRS`).

> Note: This tool intentionally does **NOT** implement the Aero Gateway's cookie-backed
> sessions (`POST /session`, `aero_session` cookie). The `?token=` parameter is a
> dev-only mechanism for local testing and should not be confused with production
> gateway auth.

If you run this on a shared machine or expose it to the internet, treat it like a credentialed proxy and deploy accordingly.

## Protocol: `aero-tcp-mux-v1`

This tool implements the same `/tcp-mux` framing as the production gateway.

For the authoritative contract, see:

- [`docs/backend/01-aero-gateway-api.md`](../../docs/backend/01-aero-gateway-api.md)
- [`backend/aero-gateway/src/protocol/tcpMux.ts`](../../backend/aero-gateway/src/protocol/tcpMux.ts)

### Transport model

All WebSocket **binary** messages are treated as a byte stream carrying one or more mux frames.
Frames may be split across WebSocket messages or concatenated within a message.

### Frame header (fixed 9 bytes)

| Field | Type | Description |
|---|---:|---|
| `msg_type` | `u8` | Message type (`OPEN=1`, `DATA=2`, `CLOSE=3`, `ERROR=4`, `PING=5`, `PONG=6`) |
| `stream_id` | `u32be` | Client-assigned stream identifier (`0` is reserved for PING/PONG) |
| `length` | `u32be` | Payload length |
| `payload` | `bytes[length]` | Payload bytes |

### `OPEN` payload (client → server)

| Field | Type |
|---|---:|
| `host_len` | `u16be` |
| `host` | `bytes[host_len]` (UTF-8 hostname or IP literal) |
| `port` | `u16be` |
| `metadata_len` | `u16be` |
| `metadata` | `bytes[metadata_len]` (optional) |

### `DATA` payload

Raw TCP bytes for the stream.

### `CLOSE` payload

| Field | Type |
|---|---:|
| `flags` | `u8` (`0x01=FIN`, `0x02=RST`) |

### `ERROR` payload

| Field | Type |
|---|---:|
| `code` | `u16be` |
| `message_len` | `u16be` |
| `message` | `utf8[message_len]` |

## Testing
```bash
npm -w tools/net-proxy-server test
```

## Docker
```bash
docker build -t aero-net-proxy-server .
docker run --rm -p 8080:8080 -e AERO_PROXY_AUTH_TOKEN=dev-token aero-net-proxy-server
```
