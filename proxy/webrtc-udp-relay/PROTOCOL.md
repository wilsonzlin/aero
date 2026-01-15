# Aero WebRTC UDP Relay Protocol

This document defines the wire protocol used by Aero to proxy UDP between:

- the browser (guest networking stack running inside the emulator), and
- a server-side UDP relay reachable from the browser.

The protocol has two parts:

- **Signaling** (JSON): used to establish the WebRTC connection (SDP/ICE exchange).
- **Data plane** (binary): datagrams sent over a WebRTC DataChannel.

---

## WebRTC DataChannels

The relay supports multiple DataChannels. Each DataChannel message is treated as
one independent datagram/message (no streaming).

### UDP relay DataChannel (`udp`)

- **Label:** `udp`
- **Reliability:** best-effort UDP semantics.
  - `ordered = false`
  - `maxRetransmits = 0`

The relay MUST treat each DataChannel message as a single UDP datagram frame (no
streaming / no message reassembly beyond what SCTP provides for a single
message).

The binary framing described in this document applies **only** to the `udp`
DataChannel.

#### Inbound filtering / NAT behavior

By default, the relay applies **inbound UDP filtering** and only forwards inbound UDP packets from
remote address+port tuples that the guest has previously sent to:

- `UDP_INBOUND_FILTER_MODE=address_and_port` (default; recommended)

This behaves like a typical symmetric NAT and is safer for public deployments (it reduces exposure to
unsolicited inbound UDP).

If you need full-cone behavior (accept inbound UDP from any remote endpoint), set:

- `UDP_INBOUND_FILTER_MODE=any` (**less safe**)

Additional knobs:

- `UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT` (default: `UDP_BINDING_IDLE_TIMEOUT`) — expire inactive allowlist entries
- `MAX_ALLOWED_REMOTES_PER_BINDING` — cap tracked remotes per guest port binding (DoS hardening)

Observability (via `GET /metrics`, exposed as `aero_webrtc_udp_relay_events_total{event="<name>"}`):

- `udp_remote_allowlist_evictions_total`: allowlist entries evicted due to `MAX_ALLOWED_REMOTES_PER_BINDING` (cap-based evictions only; TTL expiry via `UDP_REMOTE_ALLOWLIST_IDLE_TIMEOUT` does **not** count as an eviction).
- `udp_remote_allowlist_overflow_drops_total`: inbound UDP packets dropped due to inbound filtering (remote not currently on the allowlist, e.g. due to eviction or TTL expiry). This is tracked separately and is **not** included in the generic `webrtc_udp_dropped*` / `udp_ws_dropped*` counters.

### L2 tunnel DataChannel (`l2`)

- **Label:** `l2`
- **Reliability:** **MUST be reliable.**
  - `maxRetransmits` MUST be unset
  - `maxPacketLifeTime` MUST be unset
  - The relay will close `l2` DataChannels that request partial reliability.
- **Ordering:** **MUST be ordered.**
  - `ordered = true`
  - The relay will close `l2` DataChannels that request unordered delivery (`ordered = false`).

Unlike `udp`, the relay does **not** parse or frame messages on `l2`. Instead,
it acts as a transport bridge:

```
browser DataChannel "l2"  <->  webrtc-udp-relay  <->  backend WebSocket /l2 (legacy alias: /eth)
```

- Each binary DataChannel message is forwarded as a single WebSocket **binary**
  message to the configured backend.
- Each WebSocket binary message received from the backend is forwarded as a
  single DataChannel message.
- The relay negotiates WebSocket subprotocol **`aero-l2-tunnel-v1`**.
- Message format and semantics are defined by `aero-l2-tunnel-v1` (see
  `docs/l2-tunnel-protocol.md`).
- The Go implementation keeps protocol constants + codec helpers in
  `internal/l2tunnel/` (validated against `crates/conformance/test-vectors/aero-vectors-v1.json`).
- The relay enforces a per-message size limit (`L2_MAX_MESSAGE_BYTES`, default
  4096 bytes). Messages larger than this limit may cause the relay to tear down
  the `l2` bridge.

#### Backend dial options

The relay dials the backend with a configurable set of headers/subprotocols:

- `L2_BACKEND_ORIGIN` (optional):
  - When set, the relay dials the backend with `Origin: <value>`.
  - When unset, the relay forwards the browser's `Origin` header from the
    signaling request (`GET /webrtc/signal` or `POST /webrtc/offer`) if present.
- `L2_BACKEND_TOKEN` (optional):
  - When set, the relay includes an additional WebSocket subprotocol entry
    `aero-l2-token.<token>` alongside `aero-l2-tunnel-v1` (used by
    `aero-l2-proxy` token auth).
  - The negotiated subprotocol MUST still be `aero-l2-tunnel-v1`.
- `L2_BACKEND_FORWARD_AERO_SESSION` (optional, default `false`):
  - When enabled, the relay extracts the `aero_session` cookie from the
    signaling request and forwards **only** that cookie to the backend as:
    `Cookie: aero_session=<value>`.
  - This allows `aero-l2-proxy` to run in session-cookie auth mode while the
    browser uses the WebRTC transport.
  - When disabled, the relay does not send any `Cookie` header.

Rationale: the L2 tunnel carries guest Ethernet frames to a proxy that may run a
user-space NAT/TCP stack (slirp-style). That stack can acknowledge upstream TCP
data before the guest has received it, so allowing tunnel message loss (partial
reliability) can break TCP correctness.

Ordering note: the current proxy-side TCP termination stack assumes in-order
delivery of guest TCP segments and does not implement full TCP reassembly. A
reliable-but-unordered DataChannel can deliver frames out of order under loss
(including FIN before earlier payload), which breaks correctness.

#### Backend dialing configuration

These settings configure the relay's **server → server** WebSocket dial to the
backend. They are separate from browser signaling auth (`AUTH_MODE=none|api_key|jwt`),
and are never sent by the browser.

- `L2_BACKEND_WS_URL` (optional): Backend WebSocket URL (must be `ws://` or
  `wss://`). When unset/empty, `l2` DataChannels are rejected.

Backend `Origin` handling (relevant for `crates/aero-l2-proxy` Origin allowlists):

- `L2_BACKEND_FORWARD_ORIGIN` (optional, default: `true` when `L2_BACKEND_WS_URL`
  is set): When enabled, the relay forwards a normalized `Origin` value from the
  client signaling request to the backend WebSocket upgrade request.
  - If the client request has no `Origin` header, the relay derives an origin
    from the request host and scheme (e.g. `https://example.com`).
- `L2_BACKEND_ORIGIN` (optional): Override the backend `Origin` header value.
  This is the recommended knob when the backend enforces an Origin allowlist and
  you want a fixed `Origin` regardless of the client signaling request.
  - The value MUST be an allowed origin on the backend (e.g. included in
    `AERO_L2_ALLOWED_ORIGINS` (or the shared `ALLOWED_ORIGINS` fallback) for
    `crates/aero-l2-proxy`).
  - Alias: `L2_BACKEND_ORIGIN_OVERRIDE`.
  - `L2_BACKEND_WS_ORIGIN` is a legacy knob that sets the backend Origin header
    unless overridden by `L2_BACKEND_ORIGIN`/`L2_BACKEND_ORIGIN_OVERRIDE`.

Backend token authentication (relevant for `crates/aero-l2-proxy`
`AERO_L2_AUTH_MODE=token|jwt|cookie_or_jwt|session_or_token|session_and_token`):

- `L2_BACKEND_TOKEN` (optional): If set, the relay offers an additional WebSocket
  subprotocol `aero-l2-token.<token>` alongside the required `aero-l2-tunnel-v1`
  subprotocol.
  - This is delivered as:
    `Sec-WebSocket-Protocol: aero-l2-tunnel-v1, aero-l2-token.<token>`
  - The negotiated subprotocol is still required to be `aero-l2-tunnel-v1`.
  - Alias: `L2_BACKEND_WS_TOKEN`.

Credential forwarding (optional):

- `L2_BACKEND_AUTH_FORWARD_MODE` (optional, default: `query`):
  `none|query|subprotocol`.
  - `query`: append `token=<credential>` (and `apiKey=<credential>` for
    compatibility) query parameters when dialing the backend.
  - `subprotocol`: offer an additional WebSocket subprotocol
    `aero-l2-token.<credential>` alongside the required `aero-l2-tunnel-v1`
    subprotocol. The negotiated subprotocol is still required to be
    `aero-l2-tunnel-v1`.
    - The `<credential>` must be valid for `Sec-WebSocket-Protocol` (HTTP token /
      RFC 7230 `tchar`); use `query` mode if your credential contains characters
      that can't be represented as a subprotocol token.
  - The forwarded `<credential>` is the same JWT/token that authenticated the
    relay's signaling endpoints (`AUTH_MODE`). When `AUTH_MODE=none`, no
    credential is forwarded.

Important interoperability note (for `crates/aero-l2-proxy`): query-string
credentials are checked before `aero-l2-token.*` subprotocol tokens. If you want
the backend to use `L2_BACKEND_TOKEN`, set `L2_BACKEND_AUTH_FORWARD_MODE=none`
(or `subprotocol`) to avoid sending `?token=`/`?apiKey=` from the client
credential.

Security note: If your backend only supports query-string tokens (or your token
cannot be represented as a WebSocket subprotocol token), you can instead embed
`?token=...` directly into `L2_BACKEND_WS_URL`. This is less preferred because
query strings are more likely to leak into logs/metrics.

---

## WebSocket UDP relay fallback (`GET /udp`)

Some environments (or debugging workflows) cannot use WebRTC. The relay also
supports proxying UDP over a WebSocket connection:

- **Endpoint:** `GET /udp` (upgrades to WebSocket)
- **Data plane:** binary framing is identical to the WebRTC `udp` DataChannel
  framing below.

### Message semantics

- Each **binary** WebSocket message is treated as exactly one UDP relay datagram
  frame (v1 or v2) as defined in this document.
- The relay does not stream/fragment/reassemble datagrams beyond WebSocket
  message boundaries.

### Authentication

Because browsers cannot attach arbitrary headers to WebSocket upgrade requests,
`/udp` supports multiple credential delivery patterns:

1. **Upgrade request headers** (best for non-browser clients; avoids query-string leakage):
   - Same header formats as the HTTP signaling endpoints (see "Authentication" above).
2. **Query string** (less preferred; may leak into logs/history):
   - `AUTH_MODE=api_key` → `?apiKey=...` (or `?token=...` for compatibility)
   - `AUTH_MODE=jwt` → `?token=...` (or `?apiKey=...` for compatibility)
3. **First WebSocket message** (preferred for browser clients): a JSON **text** message:

```json
{"type":"auth","apiKey":"..."}
```

or:

```json
{"type":"auth","token":"..."}
```

Note: some clients send both `apiKey` and `token` for compatibility. If both are
provided, they must match.

If `AUTH_MODE != none`, the client MUST authenticate within `SIGNALING_AUTH_TIMEOUT`. If the timeout is hit, credentials are invalid, or if the client sends a datagram frame before authenticating, the relay sends a `{"type":"error","code":"unauthorized"}` message and closes the WebSocket with close code **1008** (policy violation).

### Keepalive / idle timeouts (after authentication)

After authentication completes, the relay applies an idle timeout and sends keepalive pings:

- Config:
  - `UDP_WS_IDLE_TIMEOUT` / `--udp-ws-idle-timeout` (default `60s`)
  - `UDP_WS_PING_INTERVAL` / `--udp-ws-ping-interval` (default `20s`; must be **<** `UDP_WS_IDLE_TIMEOUT`)
- The relay sends WebSocket **ping control frames** at `UDP_WS_PING_INTERVAL`.
- Any received frame (including WebSocket **pong** control frames) extends the idle deadline.
- If the connection is idle for `UDP_WS_IDLE_TIMEOUT`, the relay closes the WebSocket with close code **1000** (normal closure) and reason `"idle timeout"`.
- If the relay fails to write a ping (e.g. the peer is gone), it closes the WebSocket with close code **1001** (going away).

### Control messages (WebSocket text frames)

Text messages are reserved for control-plane signaling:

- Relay → client ready (sent after authentication completes, or immediately when
  `AUTH_MODE=none`):

  ```json
  {"type":"ready","sessionId":"..."}
  ```

  Clients SHOULD wait for this before sending datagrams when auth is enabled.
  `sessionId` is an opaque, server-generated identifier intended for
  observability/debugging. It is not the JWT `sid` claim (even when
  `AUTH_MODE=jwt`).

- Relay → client error:

  ```json
  {"type":"error","code":"unauthorized","message":"..."}
  ```

  The relay then closes the WebSocket connection.
  Common `code` values:
  - `unauthorized`: authentication failed or credentials missing.
  - `too_many_sessions`: server-wide session quota reached.
  - `session_already_active`: an active session already exists for this JWT `sid` (when `AUTH_MODE=jwt`).

After the connection is ready, the relay expects only **binary** datagram
frames.

### Limits / backpressure

- Datagram `payload` length is capped by `MAX_DATAGRAM_PAYLOAD_BYTES` (default:
  1200 bytes).
- The relay maintains a byte-bounded outbound send queue. When the queue is
  full, outbound frames are dropped to avoid unbounded memory growth.

### Semantics note (reliability)

WebSockets run over TCP and are **reliable and ordered**. The relay still treats
each message as a logical "datagram", but delivery semantics differ from the
WebRTC DataChannel configuration above (no best-effort/unordered behavior).

---

## Data plane: Binary datagram frames

Two frame versions exist:

- **v1**: legacy, IPv4-only, 8-byte header.
- **v2**: versioned framing with explicit address family, supports IPv4 and IPv6.

### Decoding rules (v1 vs v2)

Given a received DataChannel message `b`:

- If `len(b) >= 2` and `b[0] == 0xA2` and `b[1] == 0x02`, parse as **v2**.
- Otherwise, parse as **v1**.

---

## Data plane: Binary datagram frame v1 (IPv4-only)

Each DataChannel message is a single binary frame:

```
0                   1                   2                   3
0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-------------------------------+-------------------------------+
|         guest_port (u16)      |        remote_ipv4 (4B)       |
+-------------------------------+-------------------------------+
|      remote_ipv4 (cont.)      |        remote_port (u16)      |
+-------------------------------+-------------------------------+
|                         payload (0..N)                        |
+---------------------------------------------------------------+
```

### Fields (v1)

All integers are **big-endian**.

| Offset | Size | Name          | Type  | Description |
|--------|------|---------------|-------|-------------|
| 0      | 2    | `guest_port`  | u16   | Guest-side UDP port. **Outbound:** guest source port. **Inbound:** guest destination port. |
| 2      | 4    | `remote_ipv4` | 4B    | Remote IPv4 address. **Outbound:** destination IP. **Inbound:** source IP. |
| 6      | 2    | `remote_port` | u16   | Remote UDP port. **Outbound:** destination port. **Inbound:** source port. |
| 8      | N    | `payload`     | bytes | UDP payload bytes. |

Header length is always **8 bytes**.

### Example (golden vector)

For the canonical machine-readable golden vectors (used by conformance tests
across Go/TypeScript implementations), see:
[`protocol-vectors/udp-relay.json`](../../protocol-vectors/udp-relay.json).

Datagram:

- `guest_port = 10000` (`0x2710`)
- `remote_ipv4 = 192.0.2.1` (`c0 00 02 01`)
- `remote_port = 53` (`0x0035`)
- `payload = "abc"` (`61 62 63`)

Frame bytes (hex):

```
27 10 c0 00 02 01 00 35 61 62 63
```

---

## Data plane: Binary datagram frame v2 (IPv4 + IPv6)

v2 introduces an unambiguous prefix and supports both IPv4 and IPv6 endpoints.

### Header format

All integers are **big-endian**.

```
0      1      2      3      5              (var)        (var+1)
┌──────┬──────┬──────┬──────┬──────────────┬────────────┬───────────────┐
│magic │ ver  │ af   │ type │ guest_port   │ remote_ip  │ remote_port    │
│0xA2  │0x02  │0x04/ │0x00  │ (u16 BE)     │ 4/16 bytes │ (u16 BE)       │
│      │      │0x06  │      │              │            │               │
└──────┴──────┴──────┴──────┴──────────────┴────────────┴───────────────┘
│ payload...                                                            │
└───────────────────────────────────────────────────────────────────────┘
```

### Fields (v2)

| Offset | Size     | Name             | Type  | Description |
|--------|----------|------------------|-------|-------------|
| 0      | 1        | `magic`          | u8    | Always `0xA2`. |
| 1      | 1        | `version`        | u8    | Always `0x02`. |
| 2      | 1        | `address_family` | u8    | `0x04` for IPv4, `0x06` for IPv6. |
| 3      | 1        | `type`           | u8    | Message type. **v2 defines only `0x00` (datagram)**; all other values are reserved and MUST be rejected. |
| 4      | 2        | `guest_port`     | u16   | Guest-side UDP port (same semantics as v1). |
| 6      | 4 or 16  | `remote_ip`      | bytes | Remote IP address (length depends on `address_family`). |
| var    | 2        | `remote_port`    | u16   | Remote UDP port. |
| var+2  | N        | `payload`        | bytes | UDP payload bytes. |

Minimum frame length is **12 bytes** for IPv4 and **24 bytes** for IPv6 (`24` is the v2 IPv6 header size; see `internal/udpproto.MaxFrameOverheadBytes`).

### Example (v2, IPv6 golden vector)

Datagram:

- `guest_port = 48879` (`0xBEEF`)
- `remote_ip = 2001:db8::1`
- `remote_port = 51966` (`0xCAFE`)
- `payload = 0x01 0x02 0x03`

Frame bytes (hex):

```
a2 02 06 00 be ef 20 01 0d b8 00 00 00 00 00 00 00 00 00 00 00 01 ca fe 01 02 03
```

---

## Negotiation / compatibility

- **IPv6 requires v2 framing.** v1 cannot represent IPv6 addresses.
- For IPv4 traffic, servers may choose to send either v1 or v2 back to the
  client.
- Implementations typically use a preference knob (e.g. `PREFER_V2`) and a
  compatibility check (only emit v2 once the client has demonstrated v2
  support) to avoid breaking older v1-only clients.

---

## Maximum payload size

Implementations MUST enforce a maximum `payload` length (for both v1 and v2) to:

- avoid excessive DataChannel fragmentation,
- reduce the likelihood of UDP/IP fragmentation on the public Internet,
- cap memory usage for malicious peers.

The maximum is **configurable**. Recommended defaults are in the **1200–1472
byte** range:

- **Default: 1200 bytes.** This is a conservative "safe Internet" size
  (commonly used by QUIC to avoid PMTU issues).
- **1472 bytes** is the theoretical maximum UDP payload for IPv4 MTU 1500
  (1500 - 20 byte IP header - 8 byte UDP header), but WebRTC adds additional
  overhead (DTLS/SCTP), so real-world safe sizes are often smaller.

### Configuration knob

On the Go relay, the max payload is configured via:

- `MAX_DATAGRAM_PAYLOAD_BYTES` / `--max-datagram-payload-bytes` (default `1200`)

### WebRTC DataChannel / SCTP message size caps (DoS hardening)

The relay uses WebRTC DataChannels (SCTP user messages) as the transport. In
addition to `MAX_DATAGRAM_PAYLOAD_BYTES` and `L2_MAX_MESSAGE_BYTES`, the relay
configures **pion/webrtc SettingEngine** caps to prevent malicious peers from
sending extremely large SCTP messages that could otherwise be buffered/allocated
before `DataChannel.OnMessage` runs.

The relay advertises an SDP `a=max-message-size` via:

- `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` / `--webrtc-datachannel-max-message-bytes` (0 = auto)

This should be at least:

- `MAX_DATAGRAM_PAYLOAD_BYTES + 24` (worst case UDP relay frame overhead: v2 header with an IPv6 address; `24` is `internal/udpproto.MaxFrameOverheadBytes`), and
- `L2_MAX_MESSAGE_BYTES` (for `l2` tunnel messages).

The relay also enforces a hard receive-side SCTP buffer cap via:

- `WEBRTC_SCTP_MAX_RECEIVE_BUFFER_BYTES` / `--webrtc-sctp-max-receive-buffer-bytes` (0 = auto)

If a peer sends messages larger than these limits, the relay may close the
DataChannel or the entire session.

Observability (via `GET /metrics`, exposed as `aero_webrtc_udp_relay_events_total{event="<name>"}`):

- `webrtc_datachannel_udp_message_too_large` / `webrtc_datachannel_l2_message_too_large`: a peer sent a WebRTC
  DataChannel message larger than `WEBRTC_DATACHANNEL_MAX_MESSAGE_BYTES` (likely ignoring SDP); the relay closes the
  entire WebRTC session.
- `webrtc_udp_dropped_oversized`: a peer sent an oversized `udp` DataChannel frame larger than the UDP relay framing
  maximum (`MAX_DATAGRAM_PAYLOAD_BYTES` + protocol overhead); the relay drops it and closes the `udp` DataChannel.

---

## Error handling

- v1 frames with `len(frame) < 8` are **malformed** and MUST be dropped.
- v2 frames with `len(frame) < 12` are **malformed** and MUST be dropped.
- Frames with `payload` longer than the configured maximum MUST be dropped.
- Implementations MAY increment a counter/metric for dropped/malformed frames.

---

## Extensibility / future versions

- v1 has no explicit version field in the binary frame header.
- v2 uses a magic byte (`0xA2`) followed by an explicit version (`0x02`) to make
  the framing unambiguous.

Implementations MUST treat unknown future versions as unsupported.

---

## Signaling API (JSON)

The relay supports multiple signaling surfaces:

- `POST /offer`: versioned JSON, non-trickle ICE (primarily for Go integration tests / backwards compatibility).
- `GET /webrtc/signal`: WebSocket signaling with trickle ICE (recommended; fastest connect).
- `POST /webrtc/offer`: HTTP offer → answer (non-trickle ICE fallback; simplest clients/tests).

### WebRTC session establishment timeout

To avoid leaking server-side resources due to half-open WebRTC sessions (misbehaving clients / DoS), the relay enforces a connect timeout and closes PeerConnections that never reach a connected state:

- Config: `WEBRTC_SESSION_CONNECT_TIMEOUT` / `--webrtc-session-connect-timeout` (default `30s`).
- Applies regardless of signaling surface (HTTP offer endpoints and WebSocket signaling).
- A session counts as "connected" once ICE is connected/completed (`ICEConnectionStateConnected` / `ICEConnectionStateCompleted`) or the PeerConnection state is connected (`PeerConnectionStateConnected`).
- Observability: timeouts increment the `/metrics` event counter `webrtc_session_connect_timeout`.
- Setting this too low can break slow networks / delayed ICE or TURN negotiation.

### Authentication

When `AUTH_MODE != none`, `GET /webrtc/ice` and **all** signaling endpoints (`POST /offer`, `POST /webrtc/offer`, `POST /session`, `GET /webrtc/signal`) require credentials.

Responses from `GET /webrtc/ice` are explicitly **non-cacheable** (`Cache-Control: no-store`, `Pragma: no-cache`, `Expires: 0`) because they may contain sensitive TURN credentials (especially TURN REST ephemeral creds), and caching can also lead to stale credentials / ICE failures.

HTTP endpoints accept credentials via:

- `AUTH_MODE=api_key`:
   - Preferred: `X-API-Key: ...`
   - Alternative: `Authorization: ApiKey ...`
   - Compatibility: `Authorization: Bearer ...`
   - Fallback: `?apiKey=...` (or `?token=...` for compatibility)
- `AUTH_MODE=jwt`:
   - Preferred: `Authorization: Bearer ...`
   - Compatibility: `X-API-Key: ...` or `Authorization: ApiKey ...`
   - Fallback: `?token=...` (or `?apiKey=...` for compatibility)

For WebSocket authentication, see "WebSocket signaling (trickle ICE)" below.

#### Concurrent sessions (JWT `sid`)

When `AUTH_MODE=jwt`, the relay uses the JWT `sid` claim as the per-session quota
key and currently enforces **at most one active relay session per `sid` at a
time**.

This is keyed by the `sid` claim (not by the raw JWT string), so minting multiple
different JWTs with the same `sid` does not allow concurrent sessions.

If another session already exists for the same `sid`, attempts to create a new
session are rejected with `session_already_active`:

- **WebSocket signaling (`GET /webrtc/signal`)**: `{ "type":"error", "code":"session_already_active", "message":"..." }`, then the relay closes the WebSocket.
- **HTTP signaling endpoints (`POST /webrtc/offer`, `POST /offer`, `POST /session`)**: `409 Conflict` with `{ "code":"session_already_active", "message":"..." }`.

This is a behavior/compatibility contract of the current implementation and may
change in the future.

### POST /offer (v1, versioned JSON, non-trickle ICE)

Client → relay:

```json
{
  "version": 1,
  "offer": {
    "type": "offer",
    "sdp": "v=0..."
  }
}
```

Relay → client:

```json
{
  "version": 1,
  "answer": {
    "type": "answer",
    "sdp": "v=0..."
  }
}
```

The server waits up to a small timeout (configurable; default ~2s) for ICE gathering to complete so that candidates are embedded in the returned SDP. If gathering does not complete in time, the server returns an answer anyway; the returned SDP may be missing candidates and connectivity may fail.

Observability: if the ICE gathering timeout is hit, the relay increments the `/metrics` event counter `ice_gathering_timeout`.

Because this endpoint does not support trickle ICE, clients should also wait for ICE gathering to complete before sending the offer, otherwise the offer may not contain usable candidates.

When `AUTH_MODE=jwt`, the relay enforces at most one active session per JWT `sid`. If another session is already active for the same `sid`, this endpoint returns **409 Conflict** with a JSON error body like:

```json
{"code":"session_already_active","message":"session already active"}
```

### POST /session (session pre-allocation)

This endpoint reserves a server-side session ID ahead of time (primarily for
quota enforcement / future flows).

- **Request:** `POST /session` (no request body)
- **Response:** `201 Created` with the raw session ID as the response body

When `AUTH_MODE=jwt` and another active relay session already exists for the same JWT `sid`, this endpoint returns **`409 Conflict`** with `{ "code":"session_already_active", "message":"..." }`.

#### Expiry / TTL

Session IDs allocated via `POST /session` are **short-lived**. If the session is
not used within a TTL window, it is automatically released/closed so it does not
permanently consume `MAX_SESSIONS` quota.

- Config: `SESSION_PREALLOC_TTL` / `--session-prealloc-ttl` (default `60s`)

Note: today there is not yet a corresponding endpoint/protocol message that
"consumes" a preallocated session ID.

### WebSocket signaling (trickle ICE)

**Endpoint:** `GET /webrtc/signal` (upgrades to WebSocket)

#### Signaling authentication

When `AUTH_MODE != none`, signaling endpoints require authentication.

Supported auth modes:

- `AUTH_MODE=none`: no authentication.
- `AUTH_MODE=api_key`: API key authentication.
- `AUTH_MODE=jwt`: JWT (HS256) authentication.

Canonical JWT (HS256) test vectors live in
[`crates/conformance/test-vectors/aero-vectors-v1.json`](../../crates/conformance/test-vectors/aero-vectors-v1.json)
under the `aero-udp-relay-jwt-hs256` key and are consumed by both the gateway token minting logic
and the relay verifier to prevent cross-language drift.

##### JWT encoding and limits

Tokens are standard JWTs:

- **Format**: `<header_b64url>.<payload_b64url>.<sig_b64url>`
- **Encoding**: base64url **without padding** (RFC 7515 / RFC 7519)
- **Algorithm**: `HS256`
- **Hardening**: implementations SHOULD apply conservative size caps for header/payload to avoid
  attacker-controlled allocations and parsing work, and SHOULD reject malformed tokens early (e.g.
  empty segments, extra dots, non-string `typ` when present).

##### JWT claims

The relay currently accepts only **HS256** JWTs and enforces the following claims:

- Required:
  - `sid` (string): Aero session ID. Must be non-empty.
  - `iat` (number): issued-at timestamp in **Unix seconds**.
  - `exp` (number): expiry timestamp in **Unix seconds**. The relay rejects tokens where `now >= exp`.
- Optional:
  - `nbf` (number): not-before timestamp in **Unix seconds**. If present, the relay rejects tokens where `now < nbf`.
  - `origin` (string): browser Origin that minted the token (used by some deployments for policy/debugging).
  - `aud` (string): audience.
  - `iss` (string): issuer.

WebSocket credentials can be provided via either:

1. **Upgrade request headers** (best for non-browser clients; avoids query-string leakage):
   - Same header formats as the HTTP signaling endpoints (see "Authentication" above).
2. **URL query string** (works for all clients):
   - `AUTH_MODE=api_key` → `?apiKey=...` (or `?token=...` for compatibility)
   - `AUTH_MODE=jwt` → `?token=...` (or `?apiKey=...` for compatibility)
3. **First WebSocket message** (recommended for browser clients when possible):

   ```json
   {"type":"auth","apiKey":"..."}
   ```

   or:

   ```json
   {"type":"auth","token":"..."}
   ```

If `AUTH_MODE != none`, the client MUST authenticate within `SIGNALING_AUTH_TIMEOUT`. If the timeout is hit, credentials are invalid, or if the client sends `offer`/`candidate` before authenticating, the server closes the WebSocket with close code **1008** (policy violation). The server may send a `{type:"error", code:"unauthorized"}` message immediately before closing.

#### Keepalive / idle timeouts (after authentication)

After authentication completes, the relay applies an idle timeout and sends keepalive pings:

- Config:
  - `SIGNALING_WS_IDLE_TIMEOUT` / `--signaling-ws-idle-timeout` (default `60s`)
  - `SIGNALING_WS_PING_INTERVAL` / `--signaling-ws-ping-interval` (default `20s`; must be **<** `SIGNALING_WS_IDLE_TIMEOUT`)
- The relay sends WebSocket **ping control frames** at `SIGNALING_WS_PING_INTERVAL`.
- Any received message (including WebSocket **pong** control frames) extends the idle deadline.
- If the connection is idle for `SIGNALING_WS_IDLE_TIMEOUT`, the relay closes the WebSocket with close code **1000** (normal closure) and reason `"idle timeout"`.
- If the relay fails to write a ping (e.g. the peer is gone), it closes the WebSocket with close code **1001** (going away).

All signaling messages are JSON objects with a required `type` field.

#### Client → Server messages

Auth (required when `AUTH_MODE != none`, unless using query-string auth):

```json
{ "type": "auth", "apiKey": "..." }
```

or:

```json
{ "type": "auth", "token": "..." }
```

Note: some clients send both `apiKey` and `token` for compatibility. If both are provided, they must match.

Offer:

```json
{ "type": "offer", "sdp": { "type": "offer", "sdp": "v=0..." } }
```

Trickle ICE candidate:

```json
{
  "type": "candidate",
  "candidate": {
    "candidate": "candidate:...",
    "sdpMid": "0",
    "sdpMLineIndex": 0
  }
}
```

Notes:

- `usernameFragment` may be included (browser-dependent) and is forwarded to pion.
- A candidate with an empty `candidate` string is treated as an end-of-candidates signal and ignored.

Close:

```json
{ "type": "close" }
```

#### Server → Client messages

Answer:

```json
{ "type": "answer", "sdp": { "type": "answer", "sdp": "v=0..." } }
```

Trickle ICE candidate:

```json
{ "type": "candidate", "candidate": { "candidate": "candidate:...", "sdpMid": "0", "sdpMLineIndex": 0 } }
```

Error:

```json
{ "type": "error", "code": "bad_message", "message": "..." }
```

Error `code` values are currently best-effort and intended for debugging:

- `bad_message` (invalid JSON / schema)
- `unexpected_message` (invalid ordering such as candidate-before-offer)
- `unauthorized` (authentication required / invalid credentials)
- `too_many_sessions`
- `session_already_active` (when `AUTH_MODE=jwt`: an active session already exists for this JWT `sid`)
- `rate_limited`
- `internal_error`

Authentication failures result in a WebSocket close (policy violation). The relay may send an `unauthorized` error message immediately before closing; see "Signaling authentication" above.

#### WebSocket flow

1. Client connects to `/webrtc/signal`
2. (If `AUTH_MODE != none`) client sends `auth`
3. Client sends `offer`
4. Server responds immediately with `answer` (does **not** wait for ICE gathering)
5. Both sides exchange `candidate` messages until connected
6. Client opens a DataChannel labeled `udp`

### HTTP offer → answer (non-trickle ICE fallback)

**Endpoint:** `POST /webrtc/offer`

Request body:

```json
{ "sdp": { "type": "offer", "sdp": "v=0..." } }
```

For convenience, the server also accepts a raw SessionDescription object:

```json
{ "type": "offer", "sdp": "v=0..." }
```

Response body:

```json
{
  "sessionId": "....",
  "sdp": { "type": "answer", "sdp": "v=0... (with ICE candidates embedded)" }
}
```

The server waits up to a small timeout (configurable; default ~2s) for ICE gathering to complete before returning the answer SDP. If gathering does not complete in time, the returned SDP may be missing candidates and connectivity may fail.

Observability: if the ICE gathering timeout is hit, the relay increments the `/metrics` event counter `ice_gathering_timeout`.

When `AUTH_MODE=jwt`, the relay enforces at most one active session per JWT `sid`. If another session is already active for the same `sid`, this endpoint returns **409 Conflict** with a JSON error body like:

```json
{"code":"session_already_active","message":"session already active"}
```

Limitations:

- Because this endpoint does not support trickle ICE, clients should also wait for ICE gathering to complete before sending the offer, otherwise the offer may not contain usable candidates.
