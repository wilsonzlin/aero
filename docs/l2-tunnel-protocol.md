# Aero L2 Tunnel Protocol (`aero-l2-tunnel-v1`)

This document defines the **wire protocol** for tunneling **raw Ethernet frames (L2)** between:

- a browser client (emulated NIC), and
- an Aero proxy (user-space network stack / NAT / policy enforcement).

The protocol is designed to be used over:

- **WebSocket** (browser ↔ proxy)
- **WebRTC DataChannel** (browser ↔ proxy)

The goals are:

- unambiguous, **versioned** framing (for forwards/backwards compatibility),
- a minimal **control plane** (PING/PONG + ERROR),
- bounded message sizes (DoS resistance),
- transport-independence (same framing over WS and WebRTC).

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
If in doubt, use reliable/ordered (WebSocket-like) semantics.

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

The ERROR message is **advisory**; implementations may choose to keep the tunnel open or close it
after sending/receiving ERROR (see [Error handling](#error-handling)).

Payload formats:

1) **UTF-8 string**: a human-readable message.
2) **Structured binary** (optional): for programmatic error codes.

If using the structured binary form, the payload is:

```
code (u16 BE) | msg_len (u16 BE) | msg (msg_len bytes, UTF-8)
```

Receivers MAY attempt to parse the structured form first (only if the length matches) and fall back
to treating the entire payload as a UTF-8 string.

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

- Client sends `PING` every **10–30 seconds**.
- Server responds with `PONG` immediately.
- If no `PONG` is received within a timeout (e.g. 2× interval), the client SHOULD reconnect.

---

## Security model (normative)

This tunnel provides an **egress path** from the guest VM to the public Internet via the proxy.
It MUST be treated as a high-risk surface (SSRF / open proxy).

- The `/l2` endpoint MUST enforce the same authentication and origin checks as `/tcp`.
  - If the deployment uses cookies/session tokens, it MUST apply the same CSRF protections.
- The proxy MUST enforce egress policy:
  - block private/reserved IP ranges by default (RFC1918, link-local, ULA, etc.),
  - apply port allowlists/denylists,
  - apply per-session rate limits / quotas as appropriate.
- The proxy MUST NOT bridge the tunnel to the host LAN.
  - All frames MUST terminate in a user-space network stack (server-side slirp/NAT or equivalent)
    that provides a **synthetic** L2 segment for the VM session.

