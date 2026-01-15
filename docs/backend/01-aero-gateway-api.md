# Aero Gateway API (Networking Backend Contract)
*Status: draft, v1*

This document specifies the **public backend contract** for Aero networking features that require server assistance in the browser:

- A **TCP proxy** exposed as a WebSocket endpoint (`/tcp`)
- Optional **TCP multiplexing** over WebSocket (`/tcp-mux`) for scaling high connection counts
- An **L2 tunnel** exposed as a WebSocket endpoint (`/l2`; legacy alias: `/eth`) for Option C networking (see `docs/l2-tunnel-protocol.md`)
- A **DNS-over-HTTPS** endpoint (`/dns-query`) used by the guest network stack
- Optional **DNS JSON** convenience endpoint (`/dns-json`) for debugging/simple lookups
- A lightweight **session bootstrap** endpoint (`POST /session`) that issues cookies used for rate-limiting and authorization

HTTP request/response schemas are specified in [`docs/backend/openapi.yaml`](./openapi.yaml). The `/tcp` WebSocket upgrade is documented in this document (OpenAPI intentionally does not model WebSockets).

The intent is that a frontend engineer can build a compatible client **without reading the gateway/server source code**.

> Non-goal: Documenting emulator internals. This doc only covers the backend contract.

---

## 1) Base URL / origin assumptions

All endpoints are rooted at the gateway’s **configured public base URL** (`PUBLIC_BASE_URL`), which may include a **path prefix** when the gateway is served behind a reverse proxy at a subpath (e.g. `https://example.com/aero`).

Examples:

- `PUBLIC_BASE_URL=https://gateway.example.com`
  - HTTP: `https://gateway.example.com/session`, `https://gateway.example.com/dns-query`
  - WebSocket: `wss://gateway.example.com/tcp`, `wss://gateway.example.com/l2` (legacy alias: `/eth`)

- `PUBLIC_BASE_URL=https://gateway.example.com/aero`
  - HTTP: `https://gateway.example.com/aero/session`, `https://gateway.example.com/aero/dns-query`
  - WebSocket: `wss://gateway.example.com/aero/tcp`, `wss://gateway.example.com/aero/l2` (legacy alias: `/eth`)

### Same-site vs cross-site deployments (cookies)

The gateway uses **cookies** for session state. For this to work reliably in browsers:

- **Recommended:** Deploy the frontend and gateway on the same *site* (same eTLD+1), e.g.
  - Frontend: `https://app.example.com`
  - Gateway: `https://gateway.example.com`
- Avoid deploying the gateway on a completely different site (e.g. `https://gateway.example.net`), because third‑party cookie restrictions can prevent the browser from sending the session cookie.

### Local development examples

Generic form:

- `POST http://localhost:PORT/session`
- `ws://localhost:PORT/tcp?v=1&host=example.com&port=443`
- `ws://localhost:PORT/l2` (subprotocol: `aero-l2-tunnel-v1`; legacy alias: `/eth`)
- `ws://localhost:PORT/tcp-mux` (subprotocol: `aero-tcp-mux-v1`)
- `http://localhost:PORT/dns-query?...`

Concrete example (assuming the gateway is running on port `8080`):

- `POST http://localhost:8080/session`
- `ws://localhost:8080/tcp?v=1&host=example.com&port=443`
- `ws://localhost:8080/l2` (subprotocol: `aero-l2-tunnel-v1`; legacy alias: `/eth`)
- `ws://localhost:8080/tcp-mux` (subprotocol: `aero-tcp-mux-v1`)
- `http://localhost:8080/dns-query?...`

---

## 2) Session model (`POST /session`, cookies)

### Why sessions exist

The gateway is a powerful primitive (TCP proxy + DNS). A session cookie allows the gateway to:

- Apply rate limits and connection limits per browser user
- Enforce an **Origin allowlist** (see Security Model)
- Reduce open-proxy abuse by requiring a prior session bootstrap

### `POST /session`

Create or refresh a session. The gateway responds with a `Set-Cookie` header.

**Request**

- Method: `POST`
- Path: `/session`
- Body: optional JSON (may be empty)
- CORS: if the gateway is on a different origin than the frontend, the gateway must allow CORS credentials for allowed origins.

**Response**

- Status: `201 Created`
- Sets cookie:
  - Name: `aero_session`
  - Value: opaque (do not parse)
  - Attributes (recommended):
    - `HttpOnly`
    - `Secure` (for HTTPS deployments)
    - `SameSite=Lax` (works for same-site subdomains) or `SameSite=None; Secure` if truly cross-origin is required
    - `Path=/`
- Body: JSON with session metadata and limits (see `docs/backend/openapi.yaml`).

Canonical `aero_session` token test vectors (HMAC signing + expiry semantics) live in
[`crates/conformance/test-vectors/aero-vectors-v1.json`](../../crates/conformance/test-vectors/aero-vectors-v1.json)
under the `aero_session` key. These vectors are consumed by the Node gateway, Rust L2 proxy, and
other implementations to prevent cross-language drift.

### Session cookie token format (for implementers)

Clients MUST treat `aero_session` as an opaque value. This section is for anyone implementing a
gateway-compatible verifier/minting path in another language.

Token format:

- `<payload_b64url>.<sig_b64url>`
- `payload_b64url` is **base64url without padding** (URL-safe alphabet `A-Z a-z 0-9 - _`)
- `sig_b64url` is base64url-no-pad encoding of a 32-byte HMAC-SHA256, so it MUST be **exactly 43
  characters**

Signature:

- `sig = HMAC_SHA256(secret, payload_b64url_ascii_bytes)`
- Important: the signature is computed over the **base64url string**, not over the decoded JSON
  bytes.

Payload JSON (decoded from `payload_b64url`) MUST be an object with:

- `v`: JSON number equal to `1` (implementations SHOULD accept both `1` and `1.0`, matching JS
  `number` semantics)
- `sid`: non-empty string
- `exp`: JSON number (seconds since unix epoch; finite)

Expiry semantics:

- Token is expired if \(exp * 1000 \le nowMs\).
- I.e. an `exp` exactly equal to `floor(nowMs/1000)` is considered expired.

Defensive parsing requirements (to match canonical Rust verifier behavior and avoid DoS):

- MUST reject any token where:
  - the overall token length exceeds **16KiB + 1 + 43**
  - `payload_b64url` is not canonical base64url-no-pad (reject `len % 4 == 1`, reject invalid
    characters, reject non-canonical encodings where unused bits are non-zero)
  - `sig_b64url` is not canonical base64url-no-pad or is not exactly 43 chars
- SHOULD verify HMAC signature before decoding/parsing JSON.

Cookie extraction semantics:

- Multiple `Cookie` headers MAY be present; the implementation MUST use the **first** `aero_session`
  value encountered (“first cookie wins”).
- An empty `aero_session=` value is treated as missing, but it still “wins” over later values (so a
  later valid cookie MUST NOT bypass an earlier empty/invalid one).

#### Endpoint discovery (`endpoints`)

The JSON response includes an `endpoints` object with **relative paths** to the gateway’s networking surfaces.

These paths are rooted at the gateway’s configured base path (the `.pathname` of `PUBLIC_BASE_URL`). For example, if the gateway is served behind `/aero` and `PUBLIC_BASE_URL=https://gateway.example.com/aero`, then `endpoints.l2` is `/aero/l2`.

```json
{
  "endpoints": {
    "tcp": "/tcp",
    "tcpMux": "/tcp-mux",
    "dnsQuery": "/dns-query",
    "dnsJson": "/dns-json",
    "l2": "/l2",
    "udpRelayToken": "/udp-relay/token"
  }
}
```

Example when the gateway is served behind `/aero` (e.g. `PUBLIC_BASE_URL=https://gateway.example.com/aero`):

```json
{
  "endpoints": {
    "tcp": "/aero/tcp",
    "tcpMux": "/aero/tcp-mux",
    "dnsQuery": "/aero/dns-query",
    "dnsJson": "/aero/dns-json",
    "l2": "/aero/l2",
    "udpRelayToken": "/aero/udp-relay/token"
  }
}
```

Notes:

- `endpoints.l2` is routed by the **edge reverse proxy** to `aero-l2-proxy` (the Rust L2 tunnel proxy). It is not served by the `backend/aero-gateway` Node.js process.
- `endpoints.udpRelayToken` may return `404` when the gateway is not configured with `UDP_RELAY_BASE_URL`.
- `limits.l2` describes protocol-level payload limits for the L2 tunnel (`FRAME` vs control messages).
  - These values are **deployment dependent** and should match the configured `aero-l2-proxy` limits.
  - Recommended defaults are `2048` bytes for `FRAME` payloads and `256` bytes for control payloads (see [`docs/l2-tunnel-protocol.md`](../l2-tunnel-protocol.md)).

Example `limits.l2`:

```json
{
  "limits": {
    "l2": {
      "maxFramePayloadBytes": 2048,
      "maxControlPayloadBytes": 256
    }
  }
}
```

#### Optional: UDP relay configuration (`udpRelay`)

If the gateway is configured with a UDP relay base URL (`UDP_RELAY_BASE_URL`), the session response includes an additional top-level field:

```json
{
  "udpRelay": {
    "baseUrl": "https://relay.example.com",
    "authMode": "none | api_key | jwt",
    "endpoints": {
      "webrtcSignal": "/webrtc/signal",
      "webrtcOffer": "/webrtc/offer",
      "webrtcIce": "/webrtc/ice",
      "udp": "/udp"
    },
    "token": "…",
    "expiresAt": "2026-01-10T00:05:00Z"
  }
}
```

Notes:

- `udpRelay.baseUrl` may be configured as either an **HTTP(S)** or **WebSocket (WS/S)** URL
  (the gateway accepts `http:`, `https:`, `ws:`, `wss:`).
- Clients must normalize schemes based on the endpoint transport:
  - For HTTP endpoints (`/webrtc/ice`, `/webrtc/offer`): use `http(s)://` (`ws://` → `http://`, `wss://` → `https://`).
  - For WebSocket signaling (`/webrtc/signal`) and `/udp` WebSocket fallback: use `ws(s)://` (`http://` → `ws://`, `https://` → `wss://`).
  - In particular, browser `fetch()` **does not support** `ws://`/`wss://` URLs.

Token rules:

- `authMode=none`: `token` and `expiresAt` are omitted.
- `authMode=api_key`: `token` is the configured API key (intended for local/dev only).
- `authMode=jwt`: `token` is a short-lived JWT minted by the gateway.
  - Claims include `iat`, `exp`, and a stable session identifier (`sid`) derived from `aero_session`.
  - Gateways may additionally bind the token to the browser origin (`origin` claim) to reduce replay.

### UDP relay HS256 JWT format (for implementers)

Clients MUST treat the relay JWT as an opaque secret. This section is for independent
implementations (Rust/Go/TS) that need to mint/verify compatible tokens.

Token format:

- `<header_b64url>.<payload_b64url>.<sig_b64url>`
- Each segment is **base64url without padding** (URL-safe alphabet `A-Z a-z 0-9 - _`)
- `sig_b64url` MUST be exactly **43 characters** (HMAC-SHA256 signature, 32 bytes)

Signature:

- `sig = HMAC_SHA256(secret, "<header_b64url>.<payload_b64url>"_ascii_bytes)`

Header JSON (decoded from `header_b64url`):

- MUST be an object with `alg: "HS256"`
- `typ` MAY be present; if present it MUST be a string (canonical minting uses `"JWT"`)

Payload JSON (decoded from `payload_b64url`):

- Required:
  - `sid`: non-empty string
  - `iat`: integer (seconds since unix epoch)
  - `exp`: integer (seconds since unix epoch)
- Optional:
  - `origin`: string (bind token to browser origin)
  - `aud`: string (audience)
  - `iss`: string (issuer)
  - `nbf`: integer (not-before; if present and \(nowSec < nbf\), token is not yet valid)

Validity semantics:

- Token is expired if \(nowSec \ge exp\).
- If `nbf` is present, token is invalid if \(nowSec < nbf\).

Defensive parsing requirements (to match canonical Rust verifier behavior and avoid DoS):

- MUST reject any token where:
  - the overall token length exceeds **4KiB + 1 + 16KiB + 1 + 43**
  - any segment is not canonical base64url-no-pad (reject `len % 4 == 1`, reject invalid
    characters, reject non-canonical encodings where unused bits are non-zero)
  - `header_b64url` exceeds 4KiB, or `payload_b64url` exceeds 16KiB
  - `sig_b64url` is not exactly 43 chars
- SHOULD verify HMAC signature before decoding/parsing JSON.

The exhaustive negative-case vectors for both token types live in
[`protocol-vectors/auth-tokens.json`](../../protocol-vectors/auth-tokens.json).

See also: [`docs/auth-tokens.md`](../auth-tokens.md) (formats + strict verification contract).

Clients must treat `udpRelay.token` as a secret and must not log it.

Using the token:

- For HTTP endpoints (`/webrtc/ice`, `/webrtc/offer`), clients should prefer sending the credential via headers (`Authorization: Bearer ...` or `X-API-Key: ...`).
- For WebSocket endpoints (`/webrtc/signal`, `/udp`), clients can authenticate via:
  - the first WebSocket message `{ "type":"auth", "token":"..." }` / `{ "type":"auth", "apiKey":"..." }` (**recommended for browser clients**; avoids leaking secrets into URLs), or
  - the URL query string `?token=...` / `?apiKey=...` (**fallback**; can leak into browser history and proxy logs).
  - Non-browser clients may also authenticate via upgrade request headers (same carriers as the HTTP endpoints).

Relay contract:

- The relay service is implemented by [`proxy/webrtc-udp-relay`](../../proxy/webrtc-udp-relay/).
- WebRTC signaling schema and v1/v2 datagram framing (used by both the WebRTC DataChannel and the `GET /udp` WebSocket fallback) are specified in [`proxy/webrtc-udp-relay/PROTOCOL.md`](../../proxy/webrtc-udp-relay/PROTOCOL.md).
- Inbound UDP filtering: by default the relay only forwards inbound UDP from remote address+port tuples that the guest previously sent to (`UDP_INBOUND_FILTER_MODE=address_and_port`). This is safer for public deployments. If you need full-cone behavior (accept inbound UDP from any remote), set `UDP_INBOUND_FILTER_MODE=any` (**less safe**; see the relay README).
- WebRTC DataChannel DoS hardening: the relay configures pion/SCTP message-size caps to prevent malicious peers from sending extremely large WebRTC DataChannel messages that would otherwise be buffered/allocated before `DataChannel.OnMessage` runs. Relevant knobs:
  - `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` (SDP `a=max-message-size` hint; 0 = auto)
  - `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES` (hard receive-side cap; 0 = auto; must be ≥ `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` and ≥ `1500`)
- Session leak hardening: the relay closes server-side PeerConnections that never reach a connected state within `WEBRTC_SESSION_CONNECT_TIMEOUT` (default `30s`).
- `GET /webrtc/ice` responses are explicitly **non-cacheable** (`Cache-Control: no-store`, `Pragma: no-cache`, `Expires: 0`) because they may include sensitive TURN credentials (especially TURN REST ephemeral creds). Clients should not cache ICE responses beyond the lifetime of the returned credentials.

#### Optional: refresh relay token (`POST /udp-relay/token`)

Some gateway deployments expose `POST /udp-relay/token`, which returns a fresh short-lived relay credential without requiring a full session bootstrap.

- Requires the `aero_session` cookie.
- Requires a valid `Origin` header (and is subject to per-session rate limits).

### Browser usage pattern

Call `/session` once during app startup **before** opening any TCP WebSockets or sending DoH requests:

```ts
const url = new URL(gatewayBaseUrl);
url.pathname = `${url.pathname.replace(/\/$/, "")}/session`;
url.search = "";
url.hash = "";

await fetch(url.toString(), {
  method: 'POST',
  credentials: 'include', // critical: persist aero_session cookie
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({}), // optional; may be omitted
});
```

> Note: WebSockets do not have a `credentials` option; cookies are attached automatically if the browser considers them in-scope for the WebSocket URL.

---

## 3) `/tcp` WebSocket TCP proxy protocol

### Endpoint

`GET /tcp` **upgraded to WebSocket**.

Each WebSocket connection proxies **one** TCP connection.

### Query parameters

#### `v` (optional)

Protocol version for the `/tcp` WebSocket connection.

- Supported: `v=1`
- If omitted, the gateway must default to **v1**.

#### `host` (required)

`host` identifies the remote TCP endpoint host:

- A DNS name (e.g. `example.com`)
- An IPv4 address (e.g. `93.184.216.34`)
- An IPv6 address (e.g. `2606:4700:4700::1111`). Bracket form (`[::1]`) is also accepted for compatibility.

#### `port` (required)

`port` is an integer `1..65535`.

Examples (canonical form):

 - `ws://localhost:8080/tcp?v=1&host=example.com&port=443`
 - `wss://gateway.example.com/tcp?v=1&host=93.184.216.34&port=80`
 - `wss://gateway.example.com/tcp?v=1&host=2606:4700:4700::1111&port=443`

#### Compatibility alias: `target` (optional)

Some clients may use a single `target` query parameter instead of `host` + `port`. Gateways **must** support this form for compatibility.

`target` format: `<host>:<port>` (IPv6 must use RFC3986 bracket form, e.g. `target=[2606:4700:4700::1111]:443`).

When both forms are provided, the gateway must prefer `target`.

### Authentication

The WebSocket upgrade request must include the `aero_session` cookie issued by `POST /session`.

If the cookie is missing/invalid, the gateway must reject the upgrade with `401 Unauthorized` (no WebSocket).

Some deployments may additionally support a non-cookie authentication mode (token auth). Because browsers cannot set arbitrary headers for `new WebSocket(...)`, any token must be passed via a WebSocket-compatible mechanism (commonly `Sec-WebSocket-Protocol`). This is deployment-specific and not required for v1 cookie-based sessions.

### WebSocket handshake requirements (`/tcp`, `/tcp-mux`)

The gateway only accepts a standard RFC6455 WebSocket handshake for `/tcp` and `/tcp-mux`:

- `Upgrade: websocket`
- `Connection: Upgrade`
- `Sec-WebSocket-Version: 13`
- `Sec-WebSocket-Key: <non-empty>`

Malformed/non-WebSocket upgrade requests are rejected with `400 Bad Request` (no WebSocket). In browser APIs this typically surfaces as a generic WebSocket connection failure.

### Connection lifecycle

1. Client creates a WebSocket to `/tcp?v=1&host=...&port=...` (or the legacy `target=...` form).
2. Gateway validates (rough order):
   - WebSocket handshake headers (RFC6455)
   - Session cookie
   - Origin allowlist
   - Target parsing
   - Port allowlist
   - Destination hostname policy (allow/deny list, optional “DNS-name-only” mode)
   - Destination IP policy (blocked ranges)
3. Gateway completes the WebSocket upgrade.
4. Gateway attempts to connect to the target TCP endpoint and begins relaying bytes.
5. If the TCP connection fails (or the relay encounters an error), the gateway closes the WebSocket.

### WebSocket message types (v1)

The `/tcp` WebSocket is a **raw byte tunnel**:

- **Binary messages**: raw TCP payload bytes (data in either direction).
- **Text messages**: treated as UTF‑8 bytes and forwarded to the TCP socket (supported for compatibility/debug; prefer binary).

Clients must set:

```ts
ws.binaryType = 'arraybuffer';
```

Any WebSocket message payload is treated as a chunk of the TCP byte stream:

- Client → gateway: bytes are written to the TCP socket.
- Gateway → client: bytes read from the TCP socket (sent as WebSocket binary messages).

Message boundaries are **not preserved** end-to-end (TCP is a byte stream). Clients must treat each message as an arbitrary-length chunk and reassemble as needed.

### Close / errors

- Closing the WebSocket closes the corresponding TCP socket.
- If the WebSocket upgrade is rejected (origin/auth/policy), the browser will surface it as a generic WebSocket connection failure.
- If the TCP connect fails or the relay encounters an error, the gateway closes the WebSocket. Clients should treat `close`/`error` as a generic failure and retry (with backoff) when appropriate.
- Protocol violations may close with a standard close code (e.g. `1002` protocol error).

### Limits (what clients should assume)

Specific limit values are deployment-dependent, but clients should assume at least:

- The gateway may reject overly long request targets (URL + query) for `/tcp` and `/tcp-mux` with `414 URI Too Long`.
- The gateway may enforce **max concurrent TCP connections** per session.
- The gateway may enforce a **max WebSocket message size**; clients should chunk large writes (e.g. ≤ 16–64 KiB).
- The gateway may enforce **connect timeouts** and **idle timeouts**.

The recommended way to obtain concrete limits is via the JSON response of `POST /session`.

### Optional: `/tcp-mux` (multiplexed TCP over one WebSocket)

Browsers limit concurrent WebSockets per origin and each socket has overhead. For workloads that need many concurrent TCP connections (e.g. many short-lived HTTP connections), some gateway deployments may expose:

- `GET /tcp-mux` (WebSocket upgrade)

`/tcp-mux` carries multiple logical TCP streams over a single WebSocket using a gateway-defined framing protocol. This document treats `/tcp-mux` as an optional scaling path; clients should implement `/tcp` first.

---

## 3.5) `/tcp-mux` WebSocket multiplexed TCP proxy protocol

`/tcp-mux` is an additive optimization over `/tcp` that avoids the overhead of one-WebSocket-per-guest-TCP-connection. A single WebSocket carries **many concurrent TCP streams** (similar to SSH channel multiplexing).

### Endpoint

`GET /tcp-mux` **upgraded to WebSocket**.

The client MUST negotiate the WebSocket subprotocol:

- `Sec-WebSocket-Protocol: aero-tcp-mux-v1`

If the subprotocol is missing/invalid, the gateway must reject the upgrade with `400 Bad Request` (no WebSocket).

### Transport model

All `/tcp-mux` WebSocket **binary** messages are treated as a byte stream carrying one or more `aero-tcp-mux-v1` protocol frames. Frames MAY be:

- sent one-per-WebSocket-message, OR
- concatenated into a single WebSocket message, OR
- split across multiple WebSocket messages.

Receivers MUST buffer and reassemble.

All multi-byte integer fields are **big-endian** (network byte order).

### Frame header (fixed 9 bytes)

| Field | Type | Description |
|---|---:|---|
| `msg_type` | `u8` | Message type (see below) |
| `stream_id` | `u32` | Client-assigned stream identifier. `0` is reserved for connection-level messages (PING/PONG). |
| `length` | `u32` | Payload length in bytes |
| `payload` | `bytes[length]` | Message payload |

### Message types

| Name | `msg_type` | Direction | Description |
|---|---:|---|---|
| `OPEN` | `1` | C→S | Open a new TCP stream (dial target) |
| `DATA` | `2` | C↔S | Raw TCP bytes for a stream |
| `CLOSE` | `3` | C↔S | Half/full close a stream |
| `ERROR` | `4` | C↔S | Stream-level error (dial failure, policy denial, protocol error, etc.) |
| `PING` | `5` | C↔S | Keepalive / RTT probe; peer replies with `PONG` (same payload) |
| `PONG` | `6` | C↔S | Reply to `PING` |

### `OPEN` payload

Dial target encoded as:

| Field | Type | Description |
|---|---:|---|
| `host_len` | `u16` | Number of bytes in `host` |
| `host` | `bytes[host_len]` | UTF-8 hostname or IP literal |
| `port` | `u16` | TCP port |
| `metadata_len` | `u16` | Number of bytes in `metadata` (may be `0`) |
| `metadata` | `bytes[metadata_len]` | Optional UTF-8 metadata blob (typically JSON) |

`stream_id` MUST be non-zero and MUST be unique for the lifetime of the WebSocket connection.

There is no explicit “OPEN-OK” response; success is implicit. On failure, the gateway sends `ERROR` for the `stream_id`.

### `DATA` payload

Payload is raw TCP bytes for the stream.

### `CLOSE` payload

| Field | Type | Description |
|---|---:|---|
| `flags` | `u8` | Close flags |

`flags` bit meanings:

- `0x01` (`FIN`): sender will not send more data on this stream (half-close).
- `0x02` (`RST`): abort the stream immediately (full close).

### `ERROR` payload

| Field | Type | Description |
|---|---:|---|
| `code` | `u16` | Error code |
| `message_len` | `u16` | Number of bytes in `message` |
| `message` | `bytes[message_len]` | UTF-8 error message |

Recommended error codes:

| Code | Name | Meaning |
|---:|---|---|
| `1` | `POLICY_DENIED` | Target denied by policy (host/port allowlist, etc.) |
| `2` | `DIAL_FAILED` | TCP connection could not be established |
| `3` | `PROTOCOL_ERROR` | Malformed frame/payload |
| `4` | `UNKNOWN_STREAM` | `stream_id` does not exist |
| `5` | `STREAM_LIMIT_EXCEEDED` | Connection hit max multiplexed streams |
| `6` | `STREAM_BUFFER_OVERFLOW` | Stream buffered too much pending data |

### Policy failures

Policy denials MUST be returned as `ERROR` on the affected `stream_id` without closing the entire mux WebSocket connection.

### Reference implementations
 
If you want a concrete implementation to compare against:

- Canonical shared golden vectors + conformance tests:
  - [`protocol-vectors/tcp-mux-v1.json`](../../protocol-vectors/tcp-mux-v1.json)
- Gateway codec + handler:
  - `backend/aero-gateway/src/protocol/tcpMux.ts`
  - `backend/aero-gateway/src/routes/tcpMux.ts`
- Browser client (TypeScript):
  - `web/src/net/tcpMuxProxy.ts`
- Dev relays (Node):
  - `net-proxy/` (local development relay; `/tcp-mux` endpoint + DoH `/dns-query` + `/dns-json`)
  - `tools/net-proxy-server/` (standalone dev relay; `/tcp-mux` with `?token=` auth)

## 4) `/dns-query` DNS-over-HTTPS (DoH)

The gateway exposes a DNS-over-HTTPS endpoint compatible with **RFC 8484** at:

`/dns-query`

This is used by the guest stack to resolve hostnames without direct UDP access from the browser.

> Local development note: the repo also includes [`net-proxy/`](../../net-proxy/) which exposes `/dns-query` and
> `/dns-json` so you can run local networking without the full gateway. `net-proxy`’s DoH endpoints are intentionally
> lightweight (and are unauthenticated / not policy-filtered); see [`net-proxy/README.md`](../../net-proxy/README.md).
> If your frontend is running on a different origin than `net-proxy`, either proxy the DoH paths through your dev
> server (recommended) or enable the explicit CORS allowlist via `AERO_PROXY_DOH_CORS_ALLOW_ORIGINS`.

### Authentication

`/dns-query` requests must include the `aero_session` cookie.

### GET (RFC 8484)

**Request**

- Method: `GET`
- Query parameter: `dns=<base64url(dns_message)>`
- Header: `Accept: application/dns-message`

**Response**

- Status: `200 OK` (including DNS errors encoded in the DNS message, e.g. `NXDOMAIN`)
- Header: `Content-Type: application/dns-message`
- Body: DNS response in wire format
  - Note: for some *HTTP-level* failures (e.g. malformed query, payload too large, rate limiting, unsupported `Content-Type`), the gateway may respond with a non-`200` status (e.g. `400`, `413`, `429`, `415`) but will still return a valid `application/dns-message` payload (typically `FORMERR` or `SERVFAIL`) so clients can extract the DNS header/id.
  - Authentication / Origin failures (`401`/`403`) return a JSON error response (see OpenAPI).

Example:

```bash
curl -sS \
  -H 'accept: application/dns-message' \
  'https://example.com/dns-query?dns=AAABAAABAAAAAAAAB2V4YW1wbGUDY29tAAABAAE' \
  --output response.dns
```

### POST (RFC 8484)

**Request**

- Method: `POST`
- Header: `Content-Type: application/dns-message`
- Header: `Accept: application/dns-message`
- Body: DNS query in wire format

Example (build a query for `example.com A` and POST it):

```bash
python3 - <<'PY' > query.dns
import struct
# id=0, flags=0x0100, qdcount=1
msg = struct.pack('!HHHHHH', 0, 0x0100, 1, 0, 0, 0)
name = b''.join(len(l).to_bytes(1,'big') + l.encode() for l in 'example.com'.split('.')) + b'\\x00'
msg += name + struct.pack('!HH', 1, 1)  # QTYPE=A, QCLASS=IN
open('query.dns','wb').write(msg)
PY

curl -sS \
  -H 'content-type: application/dns-message' \
  -H 'accept: application/dns-message' \
  --data-binary @query.dns \
  'https://example.com/dns-query' \
  --output response.dns
```

### `/dns-json` (JSON DoH convenience)

The gateway also exposes a JSON endpoint intended for debugging and simple A/AAAA lookups:

- `GET /dns-json?name=example.com&type=A`
- Response `Content-Type: application/dns-json`

The response body is compatible with Cloudflare’s `application/dns-json` schema subset:

```json
{
  "Status": 0,
  "TC": false,
  "RD": true,
  "RA": true,
  "AD": false,
  "CD": false,
  "Question": [{ "name": "example.com", "type": 1 }],
  "Answer": [{ "name": "example.com", "type": 1, "TTL": 60, "data": "93.184.216.34" }]
}
```

Supported `type` values: `A`, `AAAA`, `CNAME` (or their numeric equivalents).

### Authentication

`/dns-json` requests must include the `aero_session` cookie.

`/dns-query` remains the canonical DoH interface; `/dns-json` is a convenience endpoint intended primarily for debugging/simple lookups.

---

## 5) Security model (gateway responsibilities)

The gateway must treat all requests as untrusted and enforce the following controls.

### 5.1 Origin allowlist

- The gateway must validate the `Origin` header for:
  - `POST /session` (CORS)
  - WebSocket upgrades to `/tcp`
  - `/dns-query` and `/dns-json` (CORS)
- Only configured frontend origins may use the gateway.

### 5.2 Port allowlist (TCP egress)

The gateway should enforce an allowlist of outbound TCP ports (deployment-specific).

Clients must be prepared for connections to be rejected even if they are valid TCP ports, e.g. blocking `25` to prevent SMTP abuse.

### 5.2.1 Hostname allow/deny lists (optional, recommended for public deployments)

To further reduce open-proxy abuse risk, deployments may additionally enforce an outbound **hostname policy**:

- allowlist: only permit specific domains (including wildcard subdomains like `*.example.com`)
- denylist: always block specific domains (deny overrides allow)
- optional “DNS-name-only” mode: disallow IP-literal targets entirely

This policy must be applied **before** DNS resolution, and IP-range blocking must still be enforced on the resolved destination addresses.

### 5.3 Blocked destination IP ranges

To mitigate SSRF and internal network scanning, the gateway should block connecting to private and special-purpose IP ranges. Recommended blocked ranges include (non-exhaustive):

#### IPv4

- `0.0.0.0/8` (this network)
- `10.0.0.0/8` (RFC1918)
- `100.64.0.0/10` (carrier-grade NAT)
- `127.0.0.0/8` (loopback)
- `169.254.0.0/16` (link-local)
- `172.16.0.0/12` (RFC1918)
- `192.0.0.0/24` (IETF protocol assignments)
- `192.0.2.0/24` (TEST-NET-1)
- `192.168.0.0/16` (RFC1918)
- `198.18.0.0/15` (benchmarking)
- `198.51.100.0/24` (TEST-NET-2)
- `203.0.113.0/24` (TEST-NET-3)
- `224.0.0.0/4` (multicast)
- `240.0.0.0/4` (reserved)

#### IPv6

- `::/128` (unspecified)
- `::1/128` (loopback)
- `fe80::/10` (link-local)
- `fc00::/7` (unique local)
- `ff00::/8` (multicast)

If a DNS name resolves to any blocked range, the gateway must treat the destination as blocked.

### 5.4 Rate limiting & quotas

Gateways should enforce rate limits and quotas to prevent abuse, including (deployment-specific):

- requests/minute (HTTP endpoints like `/session` and `/dns-query`)
- max concurrent `/tcp` connections per session/IP
- bytes transferred per session/IP

When limits are exceeded:

- HTTP endpoints should return `429 Too Many Requests`.
- WebSocket upgrades may be rejected (or connections closed). In browsers this typically appears as a generic WebSocket connection failure.

---

## 6) Recommended browser-side integration patterns

### 6.1 WebSocket creation & readiness

```ts
async function openTcpProxySocket(gatewayBaseUrl: string, host: string, port: number): Promise<WebSocket> {
  const url = new URL(gatewayBaseUrl);
  if (url.protocol === "http:") url.protocol = "ws:";
  if (url.protocol === "https:") url.protocol = "wss:";
  url.pathname = `${url.pathname.replace(/\/$/, "")}/tcp`;
  url.search = "";
  url.hash = "";
  url.searchParams.set('v', '1');
  url.searchParams.set('host', host);
  url.searchParams.set('port', String(port));

  const ws = new WebSocket(url.toString());
  ws.binaryType = 'arraybuffer';

  await new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('tcp proxy connect timeout')), 10_000);
    ws.addEventListener('open', () => {
      clearTimeout(timeout);
      resolve();
    });
    ws.addEventListener('error', () => {
      clearTimeout(timeout);
      reject(new Error('websocket error'));
    });
  });

  return ws;
}
```

### 6.2 Reconnect strategy (exponential backoff with jitter)

For transient failures (network drops, gateway redeploy, rate limits), reconnect with exponential backoff:

- base delay: 250ms
- multiply by 2 each attempt
- cap: 10–30s
- add jitter: random 0–20% to avoid thundering herds

Also:

- If the gateway uses short-lived sessions, recreate the session via `POST /session` then reconnect when you suspect expiry.
- Avoid tight reconnect loops if the failure is deterministic (policy denial, blocked destinations, etc.).

### 6.3 Chunk outgoing writes

When sending large payloads, chunk to avoid hitting server or intermediary limits:

```ts
function wsSendChunked(ws: WebSocket, bytes: Uint8Array, chunkSize = 16 * 1024) {
  for (let i = 0; i < bytes.length; i += chunkSize) {
    ws.send(bytes.subarray(i, i + chunkSize));
  }
}
```
