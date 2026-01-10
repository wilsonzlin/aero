# Aero WebRTC UDP Relay Protocol

This document defines the wire protocol used by Aero to proxy UDP between:

- the browser (guest networking stack running inside the emulator), and
- a server-side UDP relay reachable from the browser.

The protocol has two parts:

- **Signaling** (JSON): used to establish the WebRTC connection (SDP exchange).
- **Data plane** (binary): datagrams sent over a WebRTC DataChannel.

---

## WebRTC DataChannel

- **Label:** `udp`
- **Reliability:** best-effort UDP semantics.
  - `ordered = false`
  - `maxRetransmits = 0`

The relay MUST treat each DataChannel message as a single UDP datagram frame (no streaming / no message reassembly beyond what SCTP provides for a single message).

---

## Data plane: Binary datagram frame v1

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

| Offset | Size | Name          | Type | Description |
|--------|------|---------------|------|-------------|
| 0      | 2    | `guest_port`  | u16  | Guest-side UDP port. **Outbound:** guest source port. **Inbound:** guest destination port. |
| 2      | 4    | `remote_ipv4` | 4B   | Remote IPv4 address. **Outbound:** destination IP. **Inbound:** source IP. |
| 6      | 2    | `remote_port` | u16  | Remote UDP port. **Outbound:** destination port. **Inbound:** source port. |
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

## Maximum payload size

Implementations MUST enforce a maximum `payload` length to:

- avoid excessive DataChannel fragmentation,
- reduce the likelihood of UDP/IP fragmentation on the public Internet,
- cap memory usage for malicious peers.

The maximum is **configurable**. Recommended defaults are in the **1200–1472 byte** range:

- **Default: 1200 bytes.** This is a conservative "safe Internet" size (commonly used by QUIC to avoid PMTU issues).
- **1472 bytes** is the theoretical maximum UDP payload for IPv4 MTU 1500 (1500 - 20 byte IP header - 8 byte UDP header), but WebRTC adds additional overhead (DTLS/SCTP), so real-world safe sizes are often smaller.

---

## Error handling

- Frames with `len(frame) < 8` are **malformed** and MUST be dropped.
- Frames with `payload` longer than the configured maximum MUST be dropped.
- Implementations MAY increment a counter/metric for dropped/malformed frames.

---

## Extensibility / future versions

v1 has no explicit version field in the binary frame header.

Future v2 is reserved to add at least:

- explicit **address family** (IPv4/IPv6),
- a **message type** (e.g. data vs control),
- and an explicit **frame version**.

Implementations MUST treat unknown future versions as unsupported.

---

## Signaling messages (v1, JSON)

Signaling is transport-agnostic (HTTP, WebSocket, etc.). This section specifies only the JSON payloads.

### Offer request

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

### Answer response

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

For v1, it is RECOMMENDED to use **non-trickle ICE** (wait for ICE gathering to complete so that candidates are embedded in the SDP) to keep the signaling surface minimal. Future versions may add explicit ICE candidate messages.
