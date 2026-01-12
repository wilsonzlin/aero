# Aero L2 Tunnel Protocol (`aero-l2-tunnel-v1`)

This document defines the **wire protocol** for tunneling **raw Ethernet frames (L2)** between:

- a browser client (emulated NIC), and
- an Aero proxy (user-space network stack / NAT / policy enforcement).

**Final decision:** [ADR 0013: Networking via L2 tunnel (Option C) to an unprivileged proxy](./adr/0013-networking-l2-tunnel.md).  
For background and tradeoffs, see [`networking-architecture-rfc.md`](./networking-architecture-rfc.md).

The protocol is designed to be used over:

- **WebSocket** (browser ↔ proxy)
- **WebRTC DataChannel** (browser ↔ proxy)

The goals are:

- unambiguous, **versioned** framing (for forwards/backwards compatibility),
- a minimal **control plane** (PING/PONG + ERROR),
- bounded message sizes (DoS resistance),
- transport-independence (same framing over WS and WebRTC).

Canonical cross-implementation protocol vectors live in
[`crates/conformance/test-vectors/aero-vectors-v1.json`](../crates/conformance/test-vectors/aero-vectors-v1.json)
under the `aero-l2-tunnel-v1` key.

---

## Endpoint + negotiation

### WebSocket

- **Endpoint:** `GET /l2` (implementations MAY also expose an alias like `GET /eth`).
- The server MUST upgrade the request to a WebSocket connection.
- The client MUST request, and the server MUST negotiate, the WebSocket subprotocol:
  - `aero-l2-tunnel-v1`

If the subprotocol is not negotiated, the connection MUST be rejected/closed (do not silently
fall back to a different framing).

### WebRTC

- **DataChannel label:** `l2`
- Each DataChannel message MUST be treated as exactly one protocol message (no additional
  streaming framing / reassembly beyond what SCTP provides for a single message).

Reliability/ordering are transport-level policy decisions; the protocol itself is message-oriented.

For Aero production deployments (ADR 0013):

- WebRTC DataChannels that carry the L2 tunnel MUST be **reliable** (no partial reliability).
  - `maxRetransmits` MUST be unset
  - `maxPacketLifeTime` MUST be unset
- WebRTC DataChannels that carry the L2 tunnel MUST be **ordered**.
  - `ordered = true`

Rationale: when the proxy terminates TCP on behalf of the guest (slirp-style), it can acknowledge
upstream TCP data before the guest has received it. If the L2 tunnel can drop messages (partial
reliability), TCP correctness breaks. Additionally, the current proxy-side TCP termination code
(`crates/aero-net-stack`) assumes in-order delivery of guest TCP segments; a reliable-but-unordered
DataChannel can deliver segments out of order under loss (including FIN before earlier payload),
which breaks correctness. For that reason, Aero requires an **ordered** DataChannel for the L2
tunnel.

---

## Framing (data plane)

Each WebSocket message or DataChannel message contains a single **L2 tunnel protocol message**
with a fixed 4-byte header:

```
0      1      2      3
┌──────┬──────┬──────┬──────┐
│magic │ ver  │ type │flags │
│0xA2  │0x03  │ u8   │ u8   │
└──────┴──────┴──────┴──────┘
│ payload (0..N bytes)       │
└────────────────────────────┘
```

- `magic` is always `0xA2` (shared with Aero UDP relay framing).
- `version` is always `0x03` for this protocol.
  - `0x02` is reserved for the existing UDP relay v2 prefix.
- `type` is the message type (u8).
- `flags` is reserved for future use (u8).
  - Senders MUST set `flags = 0` in v1.
  - Receivers MUST ignore unknown flag bits (for forward compatibility).

All fields are single bytes; endianness is not applicable.

---

## Message types

### `0x00 FRAME`

Carries one **raw Ethernet frame**.

- Payload: bytes of the Ethernet frame as emitted by the guest NIC (no additional length prefix).
- Receivers MUST treat the payload as opaque bytes (do not assume IPv4-only, no-VLAN, etc.).

Recommended maximum payload size: **2048 bytes**.

### `0x01 PING`

Keepalive and RTT measurement.

- Payload: optional opaque bytes.
  - Recommendation: an 8-byte **u64 big-endian timestamp** (e.g. `Date.now()` milliseconds),
    but any format is allowed as long as the peer echoes it.

On receipt, the peer SHOULD respond with a `PONG` (`0x02`) whose payload is an exact echo of the
PING payload.

### `0x02 PONG`

Response to a `PING`.

- Payload: exact echo of the received `PING` payload.

### `0x7F ERROR`

Reports a protocol- or policy-level error.

The ERROR message is intended to be actionable for clients. Implementations SHOULD close the tunnel
after sending an ERROR for protocol/policy violations (see [Error handling](#error-handling)).
The production Rust proxy (`crates/aero-l2-proxy`) sends a structured ERROR payload and then closes
the WebSocket with close code **1008** (policy violation).

Payload formats:

1) **UTF-8 string**: a human-readable message.
2) **Structured binary** (optional): for programmatic error codes.

If using the structured binary form, the payload is:

```
code (u16 BE) | msg_len (u16 BE) | msg (msg_len bytes, UTF-8)
```

Receivers MAY attempt to parse the structured form first (only if the length matches) and fall back
to treating the entire payload as a UTF-8 string.

#### Error codes (structured payload)

When using the structured binary form, `code` is a stable `u16` error code.

| Code | Name | Notes |
|---:|---|---|
| 1 | `protocol_error` | Malformed/invalid tunnel framing. |
| 2 | `auth_required` | Authentication required (mostly relevant for upgrade-time errors). |
| 3 | `auth_invalid` | Invalid credentials. |
| 4 | `origin_missing` | Missing Origin header (upgrade-time). |
| 5 | `origin_denied` | Origin not allowed (upgrade-time). |
| 6 | `quota_bytes` | Total byte quota exceeded. |
| 7 | `quota_fps` | Frame rate quota exceeded. |
| 8 | `quota_connections` | Connection/session concurrency quota exceeded. |
| 9 | `backpressure` | Proxy cannot enqueue outbound messages (client not reading / internal buffer full). |

Implementation references:

- Rust (canonical protocol crate): `crates/aero-l2-protocol/src/lib.rs`
  - Error code constants: `L2_TUNNEL_ERROR_CODE_*`
  - Structured payload codec: `encode_structured_error_payload` / `decode_structured_error_payload`
- TypeScript (browser/client codec): `web/src/shared/l2TunnelProtocol.ts`
  - Error code constants: `L2_TUNNEL_ERROR_CODE_*`
  - Structured payload codec: `encodeStructuredErrorPayload` / `decodeStructuredErrorPayload`

---

## Size limits

Implementations MUST enforce a maximum payload size to:

- avoid excessive WebRTC fragmentation,
- bound memory usage for malicious peers,
- prevent pathological broadcast storms from saturating the tunnel.

The maximum MUST be **configurable**.

Recommended defaults:

- `FRAME` max payload: **2048 bytes**
- Control messages (`PING`, `PONG`, `ERROR`) max payload: **256 bytes**

Messages whose payload exceeds the configured limit MUST be rejected/dropped.

---

## Error handling

Implementations MUST be robust to malformed/malicious peers.

- Messages with `len < 4` (missing header) MUST be dropped.
- Messages with an invalid `magic` or unsupported `version` MUST be dropped.
- Messages exceeding the configured maximum MUST be dropped.

For repeated protocol violations (configurable threshold), the implementation SHOULD close the
connection:

- **WebSocket:** close with an appropriate close code (e.g. `1002` for protocol error).
- **WebRTC:** close the DataChannel / PeerConnection (implementation-specific).

Unknown `type` values SHOULD be ignored/dropped (to allow forward-compatible extensions).

---

## Keepalive

Recommended behavior:

- Client sends `PING` periodically (e.g. every **5–15 seconds**; implementations may randomize
  within a range to avoid synchronized thundering herds).
- Server responds with `PONG` immediately.
- If no `PONG` is received within a timeout (e.g. 2× interval), the client SHOULD reconnect.

---

## Security model (normative)

This tunnel provides an **egress path** from the guest VM to the public Internet via the proxy.
It MUST be treated as a high-risk surface (SSRF / open proxy).

- For internet-exposed deployments, the `/l2` endpoint MUST enforce the same authentication and origin checks as `/tcp`.
  - Browser clients SHOULD authenticate using the gateway session cookie (`aero_session`), matching `/tcp`.
  - Non-browser clients / internal bridges SHOULD authenticate using a token (see below).
  - If the deployment uses cookies, it MUST apply the same CSRF protections as `/tcp`.
- The proxy MUST enforce egress policy:
  - block private/reserved IP ranges by default (RFC1918, link-local, ULA, etc.),
  - apply port allowlists/denylists,
  - apply per-session rate limits / quotas as appropriate.
- The proxy MUST NOT bridge the tunnel to the host LAN.
  - All frames MUST terminate in a user-space network stack (server-side slirp/NAT or equivalent)
    that provides a **synthetic** L2 segment for the VM session.

---

## `aero-l2-proxy` security hardening (deployment)

The L2 tunnel is an egress-capable primitive; deploy it like you would deploy `/tcp`.

These environment variables are enforced by the **production Rust implementation**
(`crates/aero-l2-proxy`).

### Origin allowlist

By default, `aero-l2-proxy` requires an `Origin` header on the WebSocket upgrade request and validates it against an allowlist:

- `AERO_L2_ALLOWED_ORIGINS`: comma-separated list of allowed origins.
  - If unset/empty, falls back to `ALLOWED_ORIGINS` (shared with the gateway + WebRTC relay).
  - `AERO_L2_ALLOWED_ORIGINS_EXTRA` (optional) is appended (comma-prefixed convention used by `deploy/docker-compose.yml`).
  - `*` allows any **valid** Origin header value (still requires the header to be present unless `AERO_L2_OPEN=1`).
    - Malformed Origin values are rejected even when `*` is configured.

Origins are normalized and compared as:

```
<lowercase-scheme>://<lowercase-host>[:port]
```

Default ports are normalized away:

- `http://...:80` is treated as `http://...`
- `https://...:443` is treated as `https://...`

Configured origins (and the request `Origin` header) must:

- be a full origin URL (e.g. `https://example.com`),
- use `http` or `https`,
- NOT include credentials, query, fragment, or a path other than `/`.

Special cases:

- `null` is allowed only if explicitly configured (or if `*` is configured).

Dev escape hatch:

- `AERO_L2_OPEN=1` disables Origin enforcement (trusted local development only).

### Authentication (`AERO_L2_AUTH_MODE`)

Origin enforcement is not sufficient to protect an internet-exposed L2 endpoint:

- Non-browser WebSocket clients can omit `Origin`.
- Non-browser clients can trivially forge an `Origin` header.

`aero-l2-proxy` supports multiple auth modes via `AERO_L2_AUTH_MODE`:

- `none`: no auth (dev only; do not expose publicly).
- `session` (recommended for same-origin browser clients): requires the `aero_session` cookie issued
  by the gateway `POST /session` (legacy alias: `cookie`).
  - Configure the cookie signing secret via `AERO_GATEWAY_SESSION_SECRET` (preferred), or
    `SESSION_SECRET`, or `AERO_L2_SESSION_SECRET` (legacy).
- `token`: requires `AERO_L2_API_KEY` (or legacy `AERO_L2_TOKEN`) (legacy alias: `api_key`).
  - Clients can provide credentials via `?token=<value>` (or `?apiKey=<value>` for compatibility), or
    an additional `Sec-WebSocket-Protocol` entry `aero-l2-token.<value>` (offered alongside
    `aero-l2-tunnel-v1`).
- `jwt`: requires `AERO_L2_JWT_SECRET` and a JWT provided via:
  - `Authorization: Bearer <token>`, and/or
  - `?token=<value>` query param (or `?apiKey=<value>`), and/or
  - an additional `Sec-WebSocket-Protocol` entry `aero-l2-token.<value>` (offered alongside
    `aero-l2-tunnel-v1`).
  - Optional validation: `AERO_L2_JWT_AUDIENCE` and/or `AERO_L2_JWT_ISSUER` (when set, claims must match).
- `cookie_or_jwt`: accepts either a valid gateway session cookie or a valid JWT.
  - Requires both the cookie signing secret (`AERO_GATEWAY_SESSION_SECRET` or `SESSION_SECRET` /
    `AERO_L2_SESSION_SECRET`) and `AERO_L2_JWT_SECRET`.
- `session_or_token`: accepts either a valid gateway session cookie or a token.
  - Requires the cookie signing secret (`AERO_GATEWAY_SESSION_SECRET` or `SESSION_SECRET` /
    `AERO_L2_SESSION_SECRET`). If `AERO_L2_API_KEY` (or legacy `AERO_L2_TOKEN`) is configured,
    the proxy also accepts token credentials.
  - Legacy alias: `cookie_or_api_key`.
- `session_and_token`: requires both a valid gateway session cookie and a token.
  - Requires the cookie signing secret (`AERO_GATEWAY_SESSION_SECRET` or `SESSION_SECRET` /
    `AERO_L2_SESSION_SECRET`) and `AERO_L2_API_KEY` (or legacy `AERO_L2_TOKEN`).
  - Legacy alias: `cookie_and_api_key`.

Notes:

- When using an additional `Sec-WebSocket-Protocol` entry `aero-l2-token.<value>`, the negotiated
  subprotocol MUST still be `aero-l2-tunnel-v1`; the token entry is used only for authentication and
  MUST NOT replace the tunnel framing subprotocol.
- Prefer the `Sec-WebSocket-Protocol` mechanism when possible to avoid putting secrets in URLs/logs;
  query params remain supported for compatibility and for credentials that cannot be represented as
  WebSocket subprotocol tokens (HTTP token / RFC 7230 `tchar`).
- Aliases (accepted for backwards compatibility with older configs):
  - `cookie` → `session`
  - `api_key` → `token`
  - `cookie_or_api_key` → `session_or_token`
  - `cookie_and_api_key` → `session_and_token`
- Defaults: if `AERO_L2_AUTH_MODE` is unset/empty, `aero-l2-proxy` selects a safe default:
  - If a session secret is configured (`AERO_GATEWAY_SESSION_SECRET` / `SESSION_SECRET` / `AERO_L2_SESSION_SECRET`),
    default to `session_or_token` (so browser session-cookie auth works, and internal bridge tokens can be added without breaking it).
  - Else if a token is configured (`AERO_L2_API_KEY` / `AERO_L2_TOKEN`), default to `token`.
  - Else refuse to start unless `AERO_L2_OPEN=1` and `AERO_L2_INSECURE_ALLOW_NO_AUTH=1`.
- `AERO_L2_TOKEN` is a legacy alias for token auth when `AERO_L2_AUTH_MODE` is unset (and is also
  accepted as a fallback value for `AERO_L2_API_KEY`).

Credential sources supported by `aero-l2-proxy` (WebSocket upgrade time):

- Cookie header: `Cookie: aero_session=<token>` (cookie modes).
- Query string token: both `?token=` and `?apiKey=` are accepted (some relays/clients use either).
- Subprotocol token: include `aero-l2-token.<credential>` as an **offered** subprotocol in
  `Sec-WebSocket-Protocol` while still requesting `aero-l2-tunnel-v1` (the server negotiates
  `aero-l2-tunnel-v1`).

Deprecated compatibility:

- `AERO_L2_TOKEN` is treated as an alias for `AERO_L2_API_KEY` when `AERO_L2_AUTH_MODE` is unset.

Missing/incorrect credentials MUST reject the upgrade with **HTTP 401** (no WebSocket).

### Quotas

To bound abuse and accidental infinite loops, the proxy applies coarse, best-effort limits:

- `AERO_L2_MAX_CONNECTIONS` (default: `64`): process-wide concurrent tunnel cap (`0` disables).
  - When exceeded, upgrades are rejected with **HTTP 429**.
- `AERO_L2_MAX_CONNECTIONS_PER_SESSION` (default: `0` = disabled): concurrent tunnel cap per
  authenticated session (`aero_session` cookie `sid` or JWT `sid`).
  - Legacy alias: `AERO_L2_MAX_TUNNELS_PER_SESSION`.
  - Token-only connections rely on `AERO_L2_MAX_CONNECTIONS`.
  - When exceeded, upgrades are rejected with **HTTP 429**.
- `AERO_L2_MAX_BYTES_PER_CONNECTION` (default: `0` = unlimited): total bytes per connection (rx + tx).
- `AERO_L2_MAX_FRAMES_PER_SECOND` (default: `0` = unlimited): inbound messages per second per connection.

When a per-connection quota is exceeded, the proxy closes the WebSocket (typically close code `1008`).

### WebSocket message size caps

The Rust proxy configures **WebSocket-level** `max_message_size` / `max_frame_size` so oversized
messages are rejected **before** the full payload is buffered.

The cap is derived from the configured protocol payload limits:

```
max_ws_message_size = 4 (header) + max(AERO_L2_MAX_FRAME_PAYLOAD, AERO_L2_MAX_CONTROL_PAYLOAD)
```

### Recommended deployment behind an edge proxy

For production, deploy the L2 proxy behind an edge proxy / load balancer that provides:

- TLS termination (`wss://`)
- additional authentication (mTLS / JWT / IP allowlists) as appropriate
- request logging and rate limiting

Do not expose an unauthenticated L2 tunnel directly to the public internet.
