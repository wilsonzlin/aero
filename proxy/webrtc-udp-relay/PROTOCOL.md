# Aero WebRTC UDP Relay Protocol

This document defines the wire protocol used by Aero to proxy UDP between:

- the browser (guest networking stack running inside the emulator), and
- a server-side UDP relay reachable from the browser.

The protocol has two parts:

- **Signaling** (JSON): used to establish the WebRTC connection (SDP/ICE exchange).
- **Data plane** (binary): datagrams sent over a WebRTC DataChannel.

---

## WebRTC DataChannel

- **Label:** `udp`
- **Reliability:** best-effort UDP semantics.
  - `ordered = false`
  - `maxRetransmits = 0`

The relay MUST treat each DataChannel message as a single UDP datagram frame (no
streaming / no message reassembly beyond what SCTP provides for a single
message).

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

Minimum frame length is **12 bytes** for IPv4 and **24 bytes** for IPv6.

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

This endpoint waits for ICE gathering to complete so that candidates are embedded in the SDP.

### WebSocket signaling (trickle ICE)

**Endpoint:** `GET /webrtc/signal` (upgrades to WebSocket)

All signaling messages are JSON objects with a required `type` field.

#### Client → Server messages

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
- `unauthorized`
- `too_many_sessions`
- `internal_error`

#### WebSocket flow

1. Client connects to `/webrtc/signal`
2. Client sends `offer`
3. Server responds immediately with `answer` (does **not** wait for ICE gathering)
4. Both sides exchange `candidate` messages until connected
5. Client opens a DataChannel labeled `udp`

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

Limitations:

- Because this endpoint does not support trickle ICE, clients should also wait for ICE gathering to complete before sending the offer, otherwise the offer may not contain usable candidates.

