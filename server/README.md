# Aero backend server (TCP proxy + COOP/COEP static hosting)
This package provides a small backend service required by Aero:
1. **Static file hosting** with **COOP/COEP** headers (Cross-Origin Isolation for `SharedArrayBuffer`).
2. A **secure WebSocket TCP proxy** (`/ws/tcp`) so the browser can open outbound TCP connections.
3. A small **DNS lookup HTTP API** (`/api/dns/lookup`) for cases where the client is not using DoH directly.

## Quick start (local dev)
```bash
cd server
npm install

# Required. Pick any random string in real deployments.
export AERO_PROXY_TOKEN="dev-token"

# For local testing you must explicitly allow targets.
# This example allows connecting to any *public* host on 80/443.
export AERO_PROXY_ALLOW_HOSTS="*"
export AERO_PROXY_ALLOW_PORTS="80,443"

npm run dev
```

Open: `http://localhost:8080/`

## Security model
This server is designed to **not** be an open proxy by default:
- **Authentication token is required** for WebSocket proxying and DNS API calls.
- **Outbound targets are deny-by-default** via `AERO_PROXY_ALLOW_HOSTS` and `AERO_PROXY_ALLOW_PORTS`.
- **Private address ranges are blocked** unless `AERO_PROXY_ALLOW_PRIVATE_RANGES=1`.
- Per-client **connection limits** and basic **bandwidth caps** are enforced.

If you loosen the allowlist (e.g. `AERO_PROXY_ALLOW_HOSTS="*"`), the token becomes the primary line of defense.
Treat it like a password.

## COOP/COEP
For cross-origin isolation, all HTTP responses include:
- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp`
- `Origin-Agent-Cluster: ?1`

If you serve the frontend behind a reverse proxy (nginx, Caddy, etc), ensure those headers are preserved.

### nginx snippet
```nginx
add_header Cross-Origin-Opener-Policy "same-origin" always;
add_header Cross-Origin-Embedder-Policy "require-corp" always;
add_header Origin-Agent-Cluster "?1" always;
```

## Endpoints
### Static hosting
- `GET /` serves `server/public/index.html` by default.

Set a custom directory with:
`AERO_PROXY_STATIC_DIR=/path/to/frontend/dist`.

### WebSocket TCP proxy
- `WS /ws/tcp?token=...`

Binary protocol (big-endian):

Client → server:
- `0x01 CONNECT`
  - `u8  type = 0x01`
  - `u32 connId`
  - `u8  addrType` (`0x01=hostname`, `0x02=ipv4`, `0x03=ipv6`)
  - `u8  addrLen`
  - `u8[addrLen] addr`
  - `u16 port`
- `0x02 DATA`
  - `u8 type = 0x02`
  - `u32 connId`
  - `bytes payload`
- `0x03 END`
  - `u8 type = 0x03`
  - `u32 connId`
- `0x04 CLOSE`
  - `u8 type = 0x04`
  - `u32 connId`

Server → client:
- `0x10 OPENED`
  - `u8  type = 0x10`
  - `u32 connId`
  - `u8  status` (`0=ok`, non-zero=failed)
  - `u16 msgLen`
  - `u8[msgLen] msg (utf8)`
- `0x11 DATA`
  - `u8 type = 0x11`
  - `u32 connId`
  - `bytes payload`
- `0x12 END`
  - `u8 type = 0x12`
  - `u32 connId`
- `0x13 CLOSE`
  - `u8  type = 0x13`
  - `u32 connId`
  - `u8  reason`
  - `u16 msgLen`
  - `u8[msgLen] msg (utf8)`

### DNS lookup
- `GET /api/dns/lookup?name=example.com&token=...`

Response:
```json
{ "name": "example.com", "addresses": [ { "address": "93.184.216.34", "family": 4 } ] }
```

## Configuration
| Env var | Description | Default |
| --- | --- | --- |
| `AERO_PROXY_HOST` | Bind address | `0.0.0.0` |
| `AERO_PROXY_PORT` | HTTP port | `8080` |
| `AERO_PROXY_TOKEN` | Required auth token | (required) |
| `AERO_PROXY_ALLOW_HOSTS` | Comma-separated allowlist (`*`, `example.com`, `*.example.com`, `1.2.3.4`, `10.0.0.0/8`) | *(deny all)* |
| `AERO_PROXY_ALLOW_PORTS` | Comma-separated ports/ranges (`80,443,10000-10100` or `*`) | *(deny all)* |
| `AERO_PROXY_ALLOW_PRIVATE_RANGES` | Allow private/loopback/link-local ranges (`1`/`0`) | `0` |
| `AERO_PROXY_MAX_TCP_PER_WS` | TCP conns per WebSocket | `8` |
| `AERO_PROXY_MAX_TCP_TOTAL` | TCP conns across all clients | `512` |
| `AERO_PROXY_MAX_WS_PER_IP` | WebSocket conns per remote IP | `4` |
| `AERO_PROXY_BANDWIDTH_BPS` | Per-direction bytes/sec cap per WebSocket | `5000000` |
| `AERO_PROXY_CONNECTS_PER_MINUTE` | TCP CONNECT frames per minute per WebSocket | `60` |
| `AERO_PROXY_MAX_WS_MESSAGE_BYTES` | Max WebSocket message size | `1048576` |
| `AERO_PROXY_ALLOWED_ORIGINS` | Optional comma-separated Origin allowlist for WS/DNS | *(disabled)* |

---

## Static file server with Range + CORS (for streaming disk images)

For local development/testing of the streaming disk backend, this repo also
includes a tiny standalone server script:

```bash
node server/range_server.js --dir /path/to/images --port 8081
```

It serves files with:

- HTTP `Range` support (`206 Partial Content`)
- CORS headers suitable for browser Range reads
- Optional COOP/COEP headers (`--coop-coep`)

This is intended for development only; it is not hardened.
