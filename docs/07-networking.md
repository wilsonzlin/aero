# 07 - Networking Stack

## Overview

Aero needs to expose a “real” NIC to the Windows 7 guest while running inside a browser that cannot
open arbitrary TCP/UDP sockets. That means *some* proxy service is always in the data path; the
primary question is where the “host-side” networking stack lives.

## Current recommended architecture (Option C: L2 tunnel to proxy)

**Final decision:** [ADR 0013: Networking via L2 tunnel (Option C) to an unprivileged proxy](./adr/0013-networking-l2-tunnel.md).

**Option C (L2 tunnel) is the recommended production architecture.** It keeps browser CPU usage low
and avoids implementing a TCP/IP stack in WASM.

**Summary:**

- **Browser:** pure Ethernet frame forwarder (an L2 pipe).
  - The browser does not parse ARP/DHCP/IP/TCP/UDP; it forwards raw frames.
- **Proxy:** unprivileged user-space NAT stack (“slirp on the server”).
  - The proxy terminates Ethernet, provides DHCP/DNS on a synthetic LAN, and opens host sockets for
    outbound TCP/UDP.
- **Transport:** **WebSocket first** (single reliable tunnel), **WebRTC optional** (DataChannel-based
  tunnel). For WebRTC, the `l2` DataChannel MUST be reliable and ordered (`ordered = true`).

### End-to-end data path

```
Windows 7 guest
  TCP/IP stack + DHCP client
        │
        ▼
Virtual NIC (e1000 / virtio-net)
        │   (Ethernet frames)
        ▼
WASM emulator (Rust)
  L2 tunnel backend (frame pipe)
    - `L2TunnelRingBackend` (browser runtime): `NET_TX`/`NET_RX` AIPC rings
        │   (Ethernet frames)
        ▼
Browser transport
  WebSocket (default) / WebRTC DataChannel (optional)
        │
        ▼
Proxy: aero-l2-proxy
  user-space Ethernet+IP stack + NAT + policy
        │   (host TCP/UDP sockets)
        ▼
Internet
```

Browser runtime note: in the worker-based web runtime, the emulator exchanges raw Ethernet frames
with the JS tunnel client via `SharedArrayBuffer` AIPC rings (`NET_TX`/`NET_RX`) inside `ioIpcSab`.
See `docs/ipc-protocol.md` (queue kinds) and `web/src/runtime/shared_layout.ts` (`IO_IPC_NET_*`).

### Browser-side observability (L2 forwarder telemetry)

The network worker periodically emits low-rate runtime `log` events summarizing the L2 tunnel
forwarder state and drop counters. These surface in the dev console with the coordinator's role
prefix, e.g.:

```
[net] l2: open tx=10f/100B rx=20f/200B drop+{rx_full=0, pending=1, tx_bp=0} pending=0f/0B
```

- Logs are emitted at ~1Hz when `proxyUrl` is configured.
- Transition logs (`l2: connecting/open/closed/error`) are emitted immediately.
- When drop deltas are non-zero, the periodic stats log is emitted at `WARN` so it also appears in
  the coordinator's nonfatal event stream.
- Tunnel transport errors are emitted at `ERROR` (`l2: error: ...`).

See:
- `web/src/net/l2TunnelForwarder.ts` (counters + log formatting helper)
- `web/src/workers/net.worker.ts` (periodic + transition log emission)

### Key docs and repo components

- Background/tradeoffs: [`networking-architecture-rfc.md`](./networking-architecture-rfc.md)
- Wire protocol: [`l2-tunnel-protocol.md`](./l2-tunnel-protocol.md)
- Runbook (local + production): [`l2-tunnel-runbook.md`](./l2-tunnel-runbook.md)

Authoritative endpoint/protocol docs (browser-facing contracts):

- **Aero Gateway HTTP API:** [`docs/backend/openapi.yaml`](./backend/openapi.yaml)
  - `/session`, `/dns-query`, `/dns-json`, `/udp-relay/token`, etc.
- **Aero Gateway WebSocket API:** [`docs/backend/01-aero-gateway-api.md`](./backend/01-aero-gateway-api.md)
  - `/tcp` and `/tcp-mux` (`aero-tcp-mux-v1`)
- **WebRTC UDP relay (+ `/udp` WebSocket fallback + `l2` bridge):**
  [`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md)

Planned/active code paths:

- Browser tunnel client: `web/src/net/l2Tunnel.ts`
- L2 tunnel backends (queue-backed + ring-backed NET_TX/NET_RX bridge): `crates/aero-net-backend`
  - (Re-exported via `crates/emulator/src/io/net/{tunnel_backend.rs,l2_ring_backend.rs}` for compatibility.)
- WebSocket L2 proxy (unprivileged): `crates/aero-l2-proxy`
- WebRTC transport (optional): `proxy/webrtc-udp-relay` (DataChannel carrying the L2 tunnel)

## Migration from in-browser slirp/NAT

The migration is structured so we can ship Option C incrementally without regressing networking for
contributors.

### Phase 0 (current): in-browser slirp/NAT using `/tcp` + UDP relay

- Browser runs a slirp-like stack (ARP/DHCP + TCP/UDP NAT).
- TCP egress is implemented via the gateway’s WebSocket endpoints (`/tcp` or `/tcp-mux`,
  subprotocol `aero-tcp-mux-v1`).
- DNS resolution uses the gateway’s DoH endpoints (`/dns-query`, optional `/dns-json`).
- UDP egress is implemented via `proxy/webrtc-udp-relay` (WebRTC DataChannel `udp`, with `/udp`
  WebSocket fallback; see `proxy/webrtc-udp-relay/PROTOCOL.md`).

### Phase 1: introduce `L2TunnelBackend` (frame pipe) and keep slirp as fallback

- Add an `L2TunnelBackend` that forwards raw Ethernet frames to a proxy over WebSocket.
- Keep the in-browser slirp/NAT stack as a fallback for development and debugging.
- Ensure snapshots/restore continue to work by capturing tunnel connection bookkeeping and treating
  active connections as non-restorable (same policy as today).

### Phase 2: default to the L2 tunnel in production builds

- Production builds select the L2 tunnel path by default.
- Legacy slirp remains available behind a debug flag for bisecting and emergency fallback.

### Phase 3: retire in-browser TCP NAT (keep only as debug fallback)

- Remove the CPU-heavy in-browser TCP NAT path from the normal build.
- Keep a minimal debug-only fallback for isolating issues (ideally off by default and not shipped
  for public deployments).

For performance, **virtio-net** is the preferred paravirtualized NIC once virtio drivers are installed. For Windows 7 compatibility, expose virtio devices as **PCI transitional devices** (legacy + modern) so older virtio-win builds that rely on the legacy I/O port interface can bind.

See: [`16-virtio-pci-legacy-transitional.md`](./16-virtio-pci-legacy-transitional.md)

---

> Note: The sections below primarily describe the Phase 0 in-browser slirp/NAT stack (legacy /
> fallback). The production path is the L2 tunnel described above.

## Network Architecture

```
┌───────────────────────────────────────────────────────────────────────────────┐
│                                Windows 7 guest                                 │
│                                                                               │
│  apps → Winsock → tcpip.sys → NDIS → NIC driver (e1000e.sys / virtio-net.sys)  │
└───────────────────────────────────────────────────────────────────────────────┘
                 │  ◄── emulation boundary (guest ↔ browser/WASM)
                 ▼
┌───────────────────────────────────────────────────────────────────────────────┐
│                            Browser (Aero emulator)                             │
│                                                                               │
│  Phase 0 (fallback): in-browser slirp/NAT (`crates/aero-net-stack`)            │
│    - TCP  → WebSocket `/tcp` (or `/tcp-mux` + `aero-tcp-mux-v1`)               │
│    - DNS  → DoH `/dns-query` (optional `/dns-json`)                            │
│    - UDP  → WebRTC DataChannel `udp` (or WebSocket `/udp` fallback)            │
└───────────────────────────────────────────────────────────────────────────────┘
                 │ HTTPS / WSS (same-site recommended; cookies)     │ WebRTC (UDP)
                 │                                                    │
                 ▼                                                    ▼
┌──────────────────────────────────────────────┐     ┌──────────────────────────────────────────────┐
│ backend/aero-gateway                          │     │ proxy/webrtc-udp-relay                        │
│  - POST /session                              │     │  - WebRTC signaling (see PROTOCOL.md)         │
│  - WS  /tcp (1 TCP stream per WS)             │     │    - GET  /webrtc/signal (WS, trickle ICE)     │
│  - WS  /tcp-mux (subprotocol aero-tcp-mux-v1) │     │    - POST /webrtc/offer (HTTP offer→answer)    │
│  - DoH /dns-query (+ optional /dns-json)      │     │  - UDP relay: DataChannel `udp` + WS /udp      │
│  - POST /udp-relay/token (optional)           │     └──────────────────────────────────────────────┘
└──────────────────────────────────────────────┘
```

Production deployment commonly puts a reverse proxy (e.g. Caddy) in front so the browser only
talks to a single HTTPS origin:

- `/session`, `/tcp`, `/tcp-mux`, `/dns-query`, `/dns-json`, `/udp-relay/token` → **aero-gateway**
- `/webrtc/*`, `/offer`, `/udp` → **webrtc-udp-relay**
- `/l2` → **aero-l2-proxy** (Option C; `aero-l2-tunnel-v1`)

Auth note (Option C): `/l2` should be treated like `/tcp` — for any internet-exposed deployment you
should enable **Origin allowlisting + authentication**. The Rust L2 proxy (`crates/aero-l2-proxy`)
enforces an Origin allowlist by default and
supports multiple auth modes via `AERO_L2_AUTH_MODE`:

- `session` (recommended for same-origin browser clients): requires the `aero_session` cookie issued
  by `POST /session`. The proxy must share the gateway session signing secret (`SESSION_SECRET`) via
  `AERO_GATEWAY_SESSION_SECRET` (preferred), or `SESSION_SECRET`, or `AERO_L2_SESSION_SECRET` (legacy), so it can verify the cookie.
- `token` / `jwt` / `cookie_or_jwt` / `session_or_token` / `session_and_token`: useful for cross-origin deployments and non-browser/internal
  clients. Credentials can be delivered via:
  - query param: `?token=...` (or `?apiKey=...` for compatibility),
  - `Authorization: Bearer <token>` (JWT), or
  - an additional `Sec-WebSocket-Protocol` entry `aero-l2-token.<credential>` (offered alongside
    `aero-l2-tunnel-v1`; requires the credential be valid for the WebSocket subprotocol token grammar;
    prefer this form when possible to avoid putting secrets in URLs/logs).
  - Optional JWT validation: set `AERO_L2_JWT_AUDIENCE` and/or `AERO_L2_JWT_ISSUER` (when set, claims must match).
- Compatibility aliases: `cookie`→`session`, `api_key`→`token`, `cookie_or_api_key`→`session_or_token`,
  `cookie_and_api_key`→`session_and_token`.
- `AERO_L2_TOKEN` is a legacy alias for token auth (used when `AERO_L2_AUTH_MODE` is unset and also
  accepted as a fallback value for `AERO_L2_API_KEY` in `token` mode; legacy alias `api_key`).

When using the gateway session bootstrap, prefer `endpoints.l2` from `POST /session` instead of
hardcoding `/l2`.

Note: WebRTC’s **data plane** still requires UDP connectivity to the relay’s ICE port range (or a
TURN server). The reverse proxy only fronts the relay’s HTTP/WebSocket signaling endpoints.

---

## Canonical protocols (wire formats)

This section summarizes the canonical on-the-wire formats. The full specifications live in the
linked documents.

### `aero-tcp-mux-v1` (TCP multiplexing over WebSocket)

Used by `GET /tcp-mux` (WebSocket) on `backend/aero-gateway` (and the dev relays).

- **Spec:** [`docs/backend/01-aero-gateway-api.md`](./backend/01-aero-gateway-api.md)
- **Reference implementations:**
  - `backend/aero-gateway/src/protocol/tcpMux.ts`
  - `net-proxy/src/tcpMuxProtocol.ts`
  - `web/src/net/tcpMuxProxy.ts`

Frame format (big-endian), fixed **9-byte header**:

| Field | Type | Notes |
|---|---:|---|
| `msg_type` | `u8` | `OPEN=1`, `DATA=2`, `CLOSE=3`, `ERROR=4`, `PING=5`, `PONG=6` |
| `stream_id` | `u32` | Client-assigned stream ID (`0` reserved for connection-level messages) |
| `length` | `u32` | Payload length |
| `payload` | `bytes[length]` | Message payload |

### UDP relay framing v1/v2 (WebRTC DataChannel `udp` and WebSocket `/udp`)

Used by `proxy/webrtc-udp-relay` for UDP proxying:

- **WebRTC DataChannel label:** `udp` (`ordered=false`, `maxRetransmits=0`)
- **WebSocket fallback:** `GET /udp` (message-oriented; one datagram frame per WS binary message)

Spec and implementations:

- **Spec:** [`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md)
- **Reference implementations:**
  - Go: `proxy/webrtc-udp-relay/internal/udpproto/*`
  - Browser: `web/src/shared/udpRelayProtocol.ts`

Version detection rules (from `PROTOCOL.md`):

- If `len(frame) >= 2` and the first two bytes are `0xA2 0x02`, parse as **v2**.
- Otherwise, parse as **v1** (legacy, IPv4-only).

Negotiation summary:

- **IPv6 requires v2.**
- For IPv4, the relay may send v1 or v2 back to the client; relays typically only emit v2 after the
  client has demonstrated v2 support (to avoid breaking v1-only clients).

Semantic note: `/udp` over WebSocket uses the same framing, but WebSockets are **reliable and
ordered**, so it cannot perfectly emulate UDP loss/reordering semantics. Treat it as a fallback or
debug path.

### `aero-l2-tunnel-v1` (Option C: L2 tunnel)

Used by the production L2 tunnel:

- **WebSocket:** `GET /l2` (subprotocol `aero-l2-tunnel-v1`)
- **WebRTC:** DataChannel label `l2` (must be fully reliable and ordered; `ordered = true`)

See: [`docs/l2-tunnel-protocol.md`](./l2-tunnel-protocol.md)

## Deprecations / historical modules (avoid confusion)

- `server/` is the legacy monolith backend. New backend work should target:
  - `backend/aero-gateway` (TCP proxy + DoH + UDP relay token minting)
  - `proxy/webrtc-udp-relay` (WebRTC UDP relay + `/udp` fallback + `l2` bridge)
  - `crates/aero-l2-proxy` (Option C user-space stack / NAT)
  - `crates/aero-storage-server` (storage service; part of the backend split)
- `tools/aero-gateway-rs` is a **legacy/diagnostic** Rust gateway prototype that only implements the
  historical `/tcp?target=<host>:<port>` tunnel (plus a small admin/capture surface). It is not
  production-hardened and is intentionally excluded from the default Rust workspace build/test
  surface. The canonical gateway is `backend/aero-gateway` (Node/TypeScript).
- Guest network stack implementations exist in multiple places:
  - `crates/aero-net-stack` is the canonical Phase 0 in-browser stack
    (`crates/emulator/src/io/net/stack` is a thin wrapper around it for the native runtime).
  - Historical implementations have been retired/removed to reduce CI surface:
    - `crates/emulator/src/io/net/stack_legacy` (old emulator-native stack wrapper)
    - `crates/aero-net` (Tokio-era experiment; no longer part of the Rust workspace)
  - The recommended production path is Option C (L2 tunnel) as described above.
- Dev/prototype UDP behavior:
  - `net-proxy/` supports a legacy per-target UDP mode (`/udp?host=...&port=...`) that forwards raw
    UDP payload bytes. Prefer the multiplexed v1/v2 datagram framing (`/udp` with no target params)
    or WebRTC when building new integrations.

---

## Snapshot/Restore (Save States)

Network snapshots are split into two layers:

1. **NIC device model** (e1000/virtio-net): registers + DMA rings.
2. **User-space network stack**: DHCP/NAT state and host-proxy connection bookkeeping.

### What must be captured

- **NIC state**
  - RX/TX ring base addresses + head/tail indices
  - MAC address and relevant control/status registers
  - pending interrupts / interrupt mask state
- **Network stack state**
  - DHCP lease (assigned IP, gateway, DNS, lease timers)
  - NAT mappings (stable ordering in serialization)
  - proxy connection IDs and endpoints (remote IP/port)

### Limitation: active TCP connections

Browser-hosted networking relies on WebSocket/WebRTC transports and a proxy server. **Active TCP connections are not bit-restorable** across a snapshot because:

- browser WebSocket objects cannot be serialized
- server-side TCP sockets are independent of client snapshot state

**Policy on restore:**

- **Drop**: immediately close all active proxy connections and let the guest reconnect.
- **Reconnect**: preserve connection IDs/endpoints and attempt a best-effort reconnect; if reconnection fails, drop.

For deterministic testing, prefer **Drop** (removes timing-dependent reconnection behavior).

## E1000 NIC Emulation

### Register Interface

```rust
pub struct E1000Device {
    // Control registers
    ctrl: u32,           // Device Control
    status: u32,         // Device Status
    eecd: u32,           // EEPROM Control
    eerd: u32,           // EEPROM Read
    fla: u32,            // Flash Access
    ctrl_ext: u32,       // Extended Device Control
    mdic: u32,           // MDI Control
    
    // Interrupt registers
    icr: u32,            // Interrupt Cause Read
    itr: u32,            // Interrupt Throttling
    ics: u32,            // Interrupt Cause Set
    ims: u32,            // Interrupt Mask Set
    imc: u32,            // Interrupt Mask Clear
    
    // Receive registers
    rctl: u32,           // Receive Control
    rdbal: u32,          // RX Descriptor Base Low
    rdbah: u32,          // RX Descriptor Base High
    rdlen: u32,          // RX Descriptor Length
    rdh: u32,            // RX Descriptor Head
    rdt: u32,            // RX Descriptor Tail
    
    // Transmit registers
    tctl: u32,           // Transmit Control
    tdbal: u32,          // TX Descriptor Base Low
    tdbah: u32,          // TX Descriptor Base High
    tdlen: u32,          // TX Descriptor Length
    tdh: u32,            // TX Descriptor Head
    tdt: u32,            // TX Descriptor Tail
    
    // MAC address
    mac_addr: [u8; 6],
    
    // Packet buffers
    rx_queue: VecDeque<Vec<u8>>,
    tx_queue: VecDeque<Vec<u8>>,
    
    // EEPROM
    eeprom: [u16; 64],
}

#[repr(C)]
pub struct E1000RxDescriptor {
    buffer_addr: u64,    // Buffer address
    length: u16,         // Length
    checksum: u16,       // Packet checksum
    status: u8,          // Status
    errors: u8,          // Errors
    special: u16,        // Special (VLAN)
}

#[repr(C)]
pub struct E1000TxDescriptor {
    buffer_addr: u64,    // Buffer address
    length: u16,         // Length
    cso: u8,             // Checksum Offset
    cmd: u8,             // Command
    status: u8,          // Status
    css: u8,             // Checksum Start
    special: u16,        // Special (VLAN)
}
```

### Packet Processing

```rust
impl E1000Device {
    pub fn process_tx(&mut self, memory: &MemoryBus) {
        // Check if transmit is enabled
        if self.tctl & E1000_TCTL_EN == 0 {
            return;
        }
        
        let desc_base = ((self.tdbah as u64) << 32) | (self.tdbal as u64);
        let desc_count = self.tdlen / 16;
        
        // Process descriptors from head to tail
        while self.tdh != self.tdt {
            let desc_addr = desc_base + (self.tdh as u64) * 16;
            let mut desc: E1000TxDescriptor = memory.read_struct(desc_addr);
            
            // Read packet data
            let packet_data = memory.read_bytes(desc.buffer_addr, desc.length as usize);
            
            // Queue for transmission
            self.tx_queue.push_back(packet_data);
            
            // Update descriptor status
            desc.status |= E1000_TXD_STAT_DD;  // Descriptor Done
            memory.write_struct(desc_addr, &desc);
            
            // Advance head
            self.tdh = (self.tdh + 1) % desc_count;
        }
        
        // Raise TX interrupt if enabled
        if self.ims & E1000_ICR_TXDW != 0 {
            self.icr |= E1000_ICR_TXDW;
            self.raise_irq();
        }
    }
    
    pub fn receive_packet(&mut self, packet: &[u8], memory: &mut MemoryBus) {
        // Check if receive is enabled
        if self.rctl & E1000_RCTL_EN == 0 {
            return;
        }
        
        let desc_base = ((self.rdbah as u64) << 32) | (self.rdbal as u64);
        let desc_count = self.rdlen / 16;
        
        // Get next available descriptor
        let next_desc = (self.rdh + 1) % desc_count;
        if next_desc == self.rdt {
            // No descriptors available - drop packet
            return;
        }
        
        let desc_addr = desc_base + (self.rdh as u64) * 16;
        let mut desc: E1000RxDescriptor = memory.read_struct(desc_addr);
        
        // Copy packet to descriptor buffer
        memory.write_bytes(desc.buffer_addr, packet);
        
        // Update descriptor
        desc.length = packet.len() as u16;
        desc.status = E1000_RXD_STAT_DD | E1000_RXD_STAT_EOP;
        desc.errors = 0;
        memory.write_struct(desc_addr, &desc);
        
        // Advance head
        self.rdh = next_desc;
        
        // Raise RX interrupt if enabled
        if self.ims & E1000_ICR_RXT0 != 0 {
            self.icr |= E1000_ICR_RXT0;
            self.raise_irq();
        }
    }
}
```

---

## User-space Network Stack

### Ethernet Frame Processing

```rust
pub struct NetworkStack {
    mac_addr: [u8; 6],
    ip_addr: Ipv4Addr,
    gateway: Ipv4Addr,
    netmask: Ipv4Addr,
    dns_servers: Vec<Ipv4Addr>,
    
    // ARP table
    arp_table: HashMap<Ipv4Addr, [u8; 6]>,
    
    // TCP connections (for NAT tracking)
    tcp_connections: HashMap<(u16, Ipv4Addr, u16), TcpConnection>,
    
    // UDP bindings
    udp_bindings: HashMap<u16, UdpBinding>,
}

impl NetworkStack {
    pub fn process_outgoing(&mut self, frame: &[u8]) -> Option<NetworkAction> {
        // Parse Ethernet header
        if frame.len() < 14 {
            return None;
        }
        
        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        let payload = &frame[14..];
        
        match ethertype {
            0x0800 => self.process_ipv4(payload),  // IPv4
            0x0806 => self.process_arp(payload),   // ARP
            0x86DD => self.process_ipv6(payload),  // IPv6
            _ => None,
        }
    }
    
    fn process_ipv4(&mut self, packet: &[u8]) -> Option<NetworkAction> {
        if packet.len() < 20 {
            return None;
        }
        
        let ihl = (packet[0] & 0x0F) as usize * 4;
        let protocol = packet[9];
        let src_ip = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
        let dst_ip = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
        
        let payload = &packet[ihl..];
        
        match protocol {
            6 => self.process_tcp(src_ip, dst_ip, payload),   // TCP
            17 => self.process_udp(src_ip, dst_ip, payload),  // UDP
            1 => self.process_icmp(src_ip, dst_ip, payload),  // ICMP
            _ => None,
        }
    }
    
    fn process_tcp(&mut self, src_ip: Ipv4Addr, dst_ip: Ipv4Addr, segment: &[u8]) -> Option<NetworkAction> {
        if segment.len() < 20 {
            return None;
        }
        
        let src_port = u16::from_be_bytes([segment[0], segment[1]]);
        let dst_port = u16::from_be_bytes([segment[2], segment[3]]);
        let flags = segment[13];
        
        let conn_key = (src_port, dst_ip, dst_port);
        
        if flags & TCP_SYN != 0 && flags & TCP_ACK == 0 {
            // New connection - create WebSocket
            Some(NetworkAction::ConnectTcp {
                local_port: src_port,
                remote_ip: dst_ip,
                remote_port: dst_port,
            })
        } else if let Some(conn) = self.tcp_connections.get_mut(&conn_key) {
            // Existing connection - forward data
            let data_offset = ((segment[12] >> 4) as usize) * 4;
            let payload = &segment[data_offset..];
            
            if !payload.is_empty() {
                Some(NetworkAction::SendTcp {
                    connection_id: conn.id,
                    data: payload.to_vec(),
                })
            } else {
                None
            }
        } else {
            None
        }
    }
}
```

### DHCP Client

```rust
pub struct DhcpClient {
    state: DhcpState,
    transaction_id: u32,
    offered_ip: Option<Ipv4Addr>,
    server_ip: Option<Ipv4Addr>,
    lease_time: u32,
}

enum DhcpState {
    Init,
    Selecting,
    Requesting,
    Bound,
    Renewing,
    Rebinding,
}

impl DhcpClient {
    pub fn start_discovery(&mut self) -> Vec<u8> {
        self.state = DhcpState::Selecting;
        self.transaction_id = rand::random();
        
        self.build_dhcp_discover()
    }
    
    pub fn handle_response(&mut self, packet: &[u8]) -> Option<NetworkConfig> {
        let dhcp = self.parse_dhcp(packet)?;
        
        match self.state {
            DhcpState::Selecting if dhcp.message_type == DHCP_OFFER => {
                self.offered_ip = Some(dhcp.your_ip);
                self.server_ip = Some(dhcp.server_id);
                self.state = DhcpState::Requesting;
                // Send DHCP Request
                None
            }
            DhcpState::Requesting if dhcp.message_type == DHCP_ACK => {
                self.state = DhcpState::Bound;
                self.lease_time = dhcp.lease_time;
                
                Some(NetworkConfig {
                    ip_address: dhcp.your_ip,
                    subnet_mask: dhcp.subnet_mask,
                    gateway: dhcp.router,
                    dns_servers: dhcp.dns_servers,
                })
            }
            _ => None,
        }
    }
    
    fn build_dhcp_discover(&self) -> Vec<u8> {
        let mut packet = vec![0u8; 548];
        
        packet[0] = 1;  // BOOTREQUEST
        packet[1] = 1;  // Ethernet
        packet[2] = 6;  // Hardware address length
        packet[3] = 0;  // Hops
        
        // Transaction ID
        packet[4..8].copy_from_slice(&self.transaction_id.to_be_bytes());
        
        // Flags: broadcast
        packet[10] = 0x80;
        packet[11] = 0x00;
        
        // Client hardware address
        packet[28..34].copy_from_slice(&self.mac_addr);
        
        // Magic cookie
        packet[236..240].copy_from_slice(&[99, 130, 83, 99]);
        
        // Options
        let mut opt_offset = 240;
        
        // DHCP Message Type = Discover
        packet[opt_offset] = 53;
        packet[opt_offset + 1] = 1;
        packet[opt_offset + 2] = 1;  // Discover
        opt_offset += 3;
        
        // End
        packet[opt_offset] = 255;
        
        packet
    }
}
```

---

## WebSocket TCP Proxy

All outbound TCP connections from Aero are bridged through the **Aero Gateway** backend (see `backend/aero-gateway`). For the authoritative contract, see:

- [Aero Gateway API](./backend/01-aero-gateway-api.md)
- [Aero Gateway OpenAPI](./backend/openapi.yaml)

### Reference implementation: `net-proxy/`

This repository includes a standalone WebSocket → TCP/UDP relay service in [`net-proxy/`](../net-proxy/). It is suitable for:

- local development (run alongside `vite dev`)
- E2E testing (no public internet required)

For production deployments, prefer `backend/aero-gateway` (TCP+DNS) and
`proxy/webrtc-udp-relay` (UDP).

To run in trusted local development mode (allows `127.0.0.1`, RFC1918, etc):

```bash
npm ci
AERO_PROXY_OPEN=1 npm -w net-proxy run dev
```

Health check:

```bash
curl http://127.0.0.1:8081/healthz
```

UDP relay modes:

- `WS /udp` (no `host`/`port`/`target` query params): multiplexed UDP relay framing (v1/v2 datagrams) per [`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md).
- `WS /udp?v=1&host=<host>&port=<port>` (or `target=<host>:<port>`): legacy per-target UDP relay (raw UDP payload bytes).

The simplest approach is one WebSocket per TCP connection (`/tcp`). For high
connection counts (thousands of concurrent guest sockets), use multiplexing
many TCP streams over a single WebSocket connection (`GET /tcp-mux`,
subprotocol `aero-tcp-mux-v1`). Both the production gateway (`backend/aero-gateway`)
and the local dev relay (`net-proxy/`) expose `/tcp-mux`. See
[`docs/backend/01-aero-gateway-api.md`](./backend/01-aero-gateway-api.md) for the
wire protocol.

### Client-side (Browser)

```rust
pub struct TcpProxy {
    connections: HashMap<u32, WebSocket>,
    next_id: u32,
    proxy_url: String,
}

impl TcpProxy {
    pub async fn connect(&mut self, remote_ip: Ipv4Addr, remote_port: u16) -> Result<u32> {
        let id = self.next_id;
        self.next_id += 1;
        
        // Canonical Aero Gateway endpoint format:
        //   /tcp?v=1&host=<hostname-or-ip>&port=<port>
        //
        // For compatibility with older clients, the gateway may also accept
        // a legacy `target=<host>:<port>` query parameter, but new clients
        // should always use `host` + `port`.
        let url = format!(
            "{}/tcp?v=1&host={}&port={}",
             self.proxy_url,
             remote_ip,
             remote_port
        );
        
        let ws = WebSocket::new(&url)?;
        
        ws.set_binary_type(BinaryType::Arraybuffer);
        
        let (tx, rx) = channel();
        
        ws.set_onmessage(move |event| {
            if let Some(data) = event.data().as_array_buffer() {
                let bytes = Uint8Array::new(&data).to_vec();
                tx.send(bytes).ok();
            }
        });
        
        self.connections.insert(id, ws);
        
        Ok(id)
    }
    
    pub fn send(&self, connection_id: u32, data: &[u8]) -> Result<()> {
        if let Some(ws) = self.connections.get(&connection_id) {
            ws.send_with_u8_array(data)?;
        }
        Ok(())
    }
    
    pub fn close(&mut self, connection_id: u32) {
        if let Some(ws) = self.connections.remove(&connection_id) {
            ws.close().ok();
        }
    }
}
```

### Server-side (Aero Gateway)

The TCP relay is a security-critical component (SSRF, port scanning, abuse). Do not deploy an ad-hoc “minimal TCP proxy” in production.

Use the maintained gateway implementation in `backend/aero-gateway` and follow:

- [Aero Gateway API](./backend/01-aero-gateway-api.md)
- [Aero Gateway OpenAPI](./backend/openapi.yaml)

### Security

The gateway must enforce (at minimum):

- **Origin allowlist**: validate `Origin` (WebSockets) and apply strict CORS (HTTP endpoints).
- **Authentication**: require a cookie-backed session or an explicit token (including a WebSocket-compatible mechanism).
- **Blocked destinations**: deny private/loopback/link-local/multicast ranges, and re-check post-DNS resolution to prevent DNS rebinding.
- **Port allowlist**: only allow configured outbound ports (deny-by-default).
- **Rate limiting & quotas**: per-user/IP limits on connection attempts, concurrent sockets, and bytes transferred.

### Scaling: TCP multiplexing (`/tcp-mux`)

For workloads that need many concurrent TCP connections, the gateway can optionally expose a multiplexed WebSocket endpoint (`/tcp-mux`) that carries many logical TCP streams over a single socket. This reduces WebSocket overhead and avoids browser connection limits. See the [Aero Gateway API](./backend/01-aero-gateway-api.md) for details.

#### TCP Egress Policy (Recommended for Public Deployments)

When exposing the TCP proxy endpoints (`/tcp` and `/tcp-mux`) publicly, it's recommended to restrict outbound
connections to a safe subset of domains to reduce abuse risk.

Environment variables:

- `TCP_ALLOWED_HOSTS` (default: empty / allow-all)
  - Comma-separated hostname patterns.
  - Supports exact matches (`example.com`) and wildcard subdomain matches
    (`*.example.com`).
  - If non-empty, the target **must** match at least one pattern.
- `TCP_BLOCKED_HOSTS` (default: empty)
  - Comma-separated hostname patterns using the same syntax.
  - Always enforced; deny overrides allow.
- `TCP_REQUIRE_DNS_NAME` (default: `0`)
  - When set to `1`, disallows IP-literal targets entirely (forces DNS names).

Notes:

- Hostnames are normalized before matching (lowercased, IDNA/punycode for
  international domains). Invalid hostnames are rejected.
- Hostname allow/deny decisions are applied **before** DNS resolution.
- Private/reserved IP blocking still applies after DNS resolution. If a hostname
  resolves to multiple IPs, the proxy connects only to the **first** allowed
  public IP.

For local development/testing of the `/tcp-mux` framing protocol (`aero-tcp-mux-v1`):

- **Production (Aero Gateway):** canonical framing implemented by `backend/aero-gateway`.
- **Local development relay (recommended):** `net-proxy/` exposes `/tcp-mux` and uses `AERO_PROXY_OPEN` / `AERO_PROXY_ALLOW` for per-stream policy.
- **Dev relay (standalone):** `tools/net-proxy-server/` speaks the same framing, but uses `?token=` auth (not gateway cookie sessions).
- **Browser client:** `web/src/net/tcpMuxProxy.ts`.
- **TcpProxyEvent adapter:** `web/src/net/tcpProxy.ts` (`WebSocketTcpProxyMuxClient`) exposes the mux client behind the same event-sink interface as the legacy one-WebSocket-per-connection `WebSocketTcpProxyClient`.

---

## WebRTC UDP Proxy

### Security warning (server-side relay)

Running a UDP relay makes your server a **network egress point**. If it is reachable by untrusted clients and forwards UDP to arbitrary destinations, it can be abused as an **open proxy / SSRF primitive** (internal network scanning, hitting link-local services, etc.).

**Recommendation:** default to **local-only** deployment (bind on `127.0.0.1` / behind auth) and enforce an explicit destination policy (CIDR + port allowlists) in production.

The WebRTC UDP relay DataChannel framing and signaling schema are specified in
[`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md).

The same relay also exposes `GET /udp` as a WebSocket fallback using the same v1/v2 datagram framing.

**Semantic note:** WebSockets are reliable and ordered, so `/udp` cannot preserve UDP loss/reordering
semantics and may introduce head-of-line blocking. Treat it as a fallback/debug path, not the
real-time/low-latency path.

### Browser integration: gateway-minted relay credentials

The browser should **not** embed long-lived relay secrets. Instead, it obtains a short-lived relay token from the Aero Gateway:

1. Call `POST /session` on the gateway with `credentials: "include"`.
2. Read the optional `udpRelay` field from the JSON response (only present when `UDP_RELAY_BASE_URL` is configured).
3. Use `udpRelay.baseUrl` + `udpRelay.endpoints.*` to build relay URLs.

Endpoint meanings:

- `webrtcSignal`: WebSocket signaling (trickle ICE): `wss://…/webrtc/signal`
- `webrtcOffer`: HTTP signaling fallback (non-trickle ICE): `https://…/webrtc/offer`
- `webrtcIce`: HTTP ICE server discovery: `https://…/webrtc/ice`
- `udp`: WebSocket UDP fallback (non-WebRTC): `wss://…/udp`

When `udpRelay.authMode` is:

- `none`: connect directly.
- `api_key`: append `?apiKey=<token>` to relay URLs (dev-only mode).
- `jwt`: append `?token=<token>` to relay URLs (production mode).

Some deployments additionally expose `POST /udp-relay/token` on the gateway to refresh the short-lived relay token without re-running the full session bootstrap.

```rust
pub struct UdpProxy {
    peer_connection: RtcPeerConnection,
    data_channel: RtcDataChannel,
    bindings: HashMap<u16, mpsc::Sender<UdpPacket>>,
}

impl UdpProxy {
    pub async fn initialize(signaling_url: &str) -> Result<Self> {
        let config = RtcConfiguration {
            ice_servers: vec![
                RtcIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".to_string()],
                },
            ],
        };
        
        let pc = RtcPeerConnection::new(&config)?;
        
        // Create data channel for UDP.
        //
        // Note: this DataChannel config is for the UDP relay, where best-effort/lossy semantics are
        // acceptable. If you carry the **L2 tunnel** over WebRTC, the channel MUST be reliable and
        // ordered (do NOT set `maxRetransmits`/`maxPacketLifeTime`, and do not set `ordered: false`).
        // See ADR 0013 and `docs/l2-tunnel-protocol.md`.
        let dc = pc.create_data_channel("udp", &RtcDataChannelInit {
            ordered: false,
            max_retransmits: Some(0),
        })?;
        
        // Signaling to establish connection with proxy server
        let offer = pc.create_offer().await?;
        pc.set_local_description(&offer).await?;
        
        // Send offer to signaling server
        let answer = Self::signal(signaling_url, &offer).await?;
        pc.set_remote_description(&answer).await?;
        
        Ok(Self {
            peer_connection: pc,
            data_channel: dc,
            bindings: HashMap::new(),
        })
    }
    
    pub fn send(&self, guest_port: u16, remote_ip: Ipv4Addr, remote_port: u16, data: &[u8]) -> Result<()> {
        // Create a UDP relay frame. v1 is IPv4-only; v2 is required for IPv6.
        // See `proxy/webrtc-udp-relay/PROTOCOL.md` for details.
        let mut packet = Vec::with_capacity(8 + data.len());
        packet.extend_from_slice(&guest_port.to_be_bytes());
        packet.extend_from_slice(&remote_ip.octets());
        packet.extend_from_slice(&remote_port.to_be_bytes());
        packet.extend_from_slice(data);
        
        self.data_channel.send(&packet)?;
        Ok(())
    }
}
```

**IPv6 note:** The 8-byte header shown above is the legacy **v1** framing and is
IPv4-only. Relaying to IPv6 destinations (and receiving IPv6 datagrams) requires
the versioned **v2** framing defined in `proxy/webrtc-udp-relay/PROTOCOL.md`.
 
 ---

## DNS Resolution

In browser environments, DNS lookups should go through the gateway’s **first-party** DNS-over-HTTPS endpoints:

- `/dns-query` (RFC 8484 DoH, `application/dns-message`)
- `/dns-json` (optional JSON convenience endpoint)

For simple browser/WASM clients that do not want to parse DNS wire format, `/dns-json` is the easiest integration:

```ts
const res = await fetch(`/dns-json?name=${hostname}&type=A`, {
  headers: { accept: 'application/dns-json' },
});
const json = await res.json();
const ip = json.Answer?.[0]?.data; // e.g. "93.184.216.34"
```

```rust
pub struct DnsResolver {
    gateway_url: String,
    dns_servers: Vec<Ipv4Addr>,
    cache: HashMap<String, DnsCacheEntry>,
}

impl DnsResolver {
    pub fn resolve(&mut self, hostname: &str) -> Option<Ipv4Addr> {
        // Check cache
        if let Some(entry) = self.cache.get(hostname) {
            if entry.expires > Instant::now() {
                return Some(entry.address);
            }
        }
        
        // Build DNS query
        let query = self.build_query(hostname);
        
        // In browser context, we need to proxy DNS through the server
        // or use DNS-over-HTTPS
        None
    }
    
    pub async fn resolve_doh(&self, hostname: &str) -> Result<Ipv4Addr> {
        // Use Aero Gateway DNS-over-HTTPS (first-party).
        // Canonical DoH: `/dns-query` (RFC 8484, `application/dns-message`).
        //
        // Note: some deployments may also expose `/dns-json` for simple lookups,
        // but clients should not depend on it (it is not required for DoH).
        let query = self.build_query_message(hostname, RecordType::A)?;
        let dns = base64url_encode(&query);
        let url = format!("{}/dns-query?dns={}", self.gateway_url, dns);
        
        let response = fetch(&url, FetchOptions {
            headers: vec![("Accept".to_string(), "application/dns-message".to_string())],
        }).await?;
        
        let bytes = response.bytes().await?;
        let ip = parse_first_a_record(&bytes)?;
        Ok(ip)
    }
}
```

---

## Virtio-net (Paravirtualized)

> For the exact Windows 7 driver ↔ Aero device-model interoperability contract (PCI transport, virtqueue rules, and virtio-net requirements), see:  
> [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md)  
>
> For the split-ring virtqueue implementation algorithms used by Windows 7 KMDF virtio drivers, see:  
> [`docs/virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md)

```rust
pub struct VirtioNetDevice {
    // Virtio common
    device_features: u64,
    driver_features: u64,
    
    // Device config
    mac: [u8; 6],
    status: u16,
    max_virtqueue_pairs: u16,
    
    // Virtqueues
    rx_vq: Virtqueue,
    tx_vq: Virtqueue,
    ctrl_vq: Option<Virtqueue>,
}

impl VirtioNetDevice {
    pub fn process_tx(&mut self, memory: &MemoryBus, network: &mut NetworkStack) {
        while let Some(desc_chain) = self.tx_vq.pop_available(memory) {
            // First descriptor is virtio_net_hdr
            let header_addr = desc_chain[0].addr;
            let header: VirtioNetHeader = memory.read_struct(header_addr);
            
            // Remaining descriptors are packet data
            let mut packet = Vec::new();
            for desc in &desc_chain[1..] {
                let data = memory.read_bytes(desc.addr, desc.len as usize);
                packet.extend_from_slice(&data);
            }
            
            // Process packet
            if let Some(action) = network.process_outgoing(&packet) {
                // Handle network action
            }
            
            // Return descriptor
            self.tx_vq.push_used(desc_chain.head_id, 0, memory);
        }
        
        self.maybe_notify_guest();
    }
    
    pub fn receive_packet(&mut self, packet: &[u8], memory: &mut MemoryBus) {
        if let Some(desc_chain) = self.rx_vq.pop_available(memory) {
            // Write virtio_net_hdr to first descriptor
            let header = VirtioNetHeader::default();
            memory.write_struct(desc_chain[0].addr, &header);
            
            // Write packet to second descriptor
            memory.write_bytes(desc_chain[1].addr, packet);
            
            // Return with total length
            let total_len = std::mem::size_of::<VirtioNetHeader>() + packet.len();
            self.rx_vq.push_used(desc_chain.head_id, total_len as u32, memory);
            
            self.maybe_notify_guest();
        }
    }
}
```

---

## Network Tracing (PCAP/PCAPNG Export)

When debugging network bring-up, it is often necessary to see the exact guest Ethernet frames (TX from the guest NIC and RX to the guest NIC), as well as emulator-generated traffic (e.g. TCP proxy I/O) for correlation.

The network stack should support an *optional* tracing component that:

- Captures **raw Ethernet frames** with timestamps (TX/RX) suitable for Wireshark.
- Optionally captures **post-NAT / proxy bytes** on a separate pseudo-interface for correlation.
- Can be enabled at runtime in dev builds, but is **off by default**.

### Implementation Notes (Repo)

The Rust implementation lives in `crates/emulator/src/io/net/trace/` and provides:

- `NetTracer` + `NetTraceConfig` for capturing frames and exporting PCAPNG.
- `TracedNetworkStack` wrapper that records:
  - Guest TX/RX Ethernet frames at the stack boundary
  - TCP proxy payloads (`ProxyAction::TcpSend` / `ProxyEvent::TcpData`) on a separate pseudo-interface.

Rust capture format notes (PCAPNG):

- Ethernet frames are written on a single interface named `guest-eth0`; packet direction is encoded
  via the Enhanced Packet Block `epb_flags` option (`1` = inbound, `2` = outbound).
- If proxy payload records are present, additional pseudo-interfaces may be emitted:
  - `tcp-proxy` (`LINKTYPE_USER0` / 147) containing pseudo-packets with an `ATCP` header.
  - `udp-proxy` (`LINKTYPE_USER1` / 148) containing pseudo-packets with an `AUDP` header.

### Web runtime tracing (browser / worker runtime)

When running the **worker-based web runtime** with the **Option C L2 tunnel**, you can capture the
raw Ethernet frames being forwarded between the guest and the tunnel client and export them as a
`.pcapng` file (openable in Wireshark).

Note: this is implemented in the web runtime (TypeScript) and is separate from the Rust-side
`crates/emulator/src/io/net/trace/` `NetTracer` described above; both export PCAPNG, but they run in
different runtimes and capture at different boundaries.

#### What it captures (L2 boundary)

The web tracing capture is taken at the **L2 forwarder boundary** in the browser:

- **Guest → tunnel:** frames drained from the guest-facing `NET_TX` ring before they are sent to the
  tunnel transport.
- **Tunnel → guest:** frames received from the tunnel transport before they are pushed into the
  guest-facing `NET_RX` ring.

In other words, it captures **guest↔tunnel Ethernet frames** right where the browser runtime acts as
an Ethernet “pipe” (the same conceptual boundary described in ADR 0013 / Option C).

Capture format notes:

- The exported PCAPNG uses a single Ethernet interface named `guest-eth0`.
- Packet direction is encoded via the Enhanced Packet Block `epb_flags` option:
  - `1` = inbound (tunnel → guest)
  - `2` = outbound (guest → tunnel)
- The current web runtime capture contains only raw Ethernet frames (no proxy-payload pseudo-interfaces), but the capture format matches the Rust tracer and may include additional pseudo-interfaces in future/other runtimes (see above).
- Frames are recorded at the forwarder boundary (best-effort). In particular, the capture may
  include frames that were later dropped due to missing tunnel/backpressure, or because `NET_RX`
  was full.
- Timestamps are stored in nanoseconds (PCAPNG resolution `10^-9`), but in the current web runtime
  they are derived from `Date.now()` (millisecond wall-clock time), so do not expect
  sub-millisecond precision.

Relevant implementation files:

- UI surface: `web/src/net/trace_ui.ts`
- `window.aero.netTrace` installer (bridges worker coordinator → global API): `web/src/net/trace_backend.ts`
- Tunnel forwarder owner (where the capture boundary lives): `web/src/workers/net.worker.ts`
- Forwarder capture hook semantics: `web/src/net/l2TunnelForwarder.ts`
- Capture implementation + PCAPNG writer: `web/src/net/net_tracer.ts`, `web/src/net/pcapng.ts`
- Coordinator API + message types (worker runtime): `web/src/runtime/coordinator.ts`, `web/src/runtime/protocol.ts`
- Shared type definition for the global API: `shared/aero_api.ts` (`AeroNetTraceApi`)

#### UI workflow (“Network trace (PCAPNG)” panel)

In the repo’s browser host UI (repo-root Vite app; see `src/main.ts`), there is a panel titled
**“Network trace (PCAPNG)”** (some legacy/experimental hosts label it just “Network trace”; see
`web/src/main.ts`) that provides:

- **Enable/disable** tracing via a checkbox.
- (Optional) **Live stats** (captured bytes/records + drop counters) when the backend implements a stats API.
- **Clear capture** (when supported by the backend).
- **Download capture (PCAPNG)** to your local machine.
- **Save capture to OPFS** (Origin Private File System) at a configurable path (default
  `captures/aero-net-trace.pcapng`).

Tip: if you're debugging **early boot networking** (DHCP/DNS), enable tracing before starting the VM
so you don't miss the initial traffic.

Disabling tracing stops recording new frames but does **not** automatically clear any frames already
buffered in memory (use **Clear capture** or download/export to reset).

Captures are kept **in-memory** until you export/save them. Reloading the page (or the net worker
crashing/restarting) will discard any buffered frames.

If the tracing backend is not installed in the current build/runtime (e.g. `window.aero.netTrace`
is missing), the UI will surface an error when you try to enable/export. If the net worker isn't
running yet, captures may not record anything until it starts, and operations like download/stats
may fail (some hosts explicitly require the net worker to be running before enabling).

Most web hosts install the `window.aero.netTrace` backend by calling
`installNetTraceBackendOnAeroGlobal(...)` (see `web/src/net/trace_backend.ts`).

If you are building a custom web host and want the automation API for DevTools or Playwright-style
scripts, ensure you call this after creating the worker coordinator:

```ts
import { installNetTraceBackendOnAeroGlobal } from "./net/trace_backend";

installNetTraceBackendOnAeroGlobal(workerCoordinator);
```

OPFS notes:

- OPFS is origin-scoped browser storage (`navigator.storage.getDirectory()`); it is convenient for
  keeping large captures without triggering a download prompt.
- OPFS may not be available in all browsers/contexts; the UI will error if unsupported.
- In Chromium-based browsers, you can inspect files written to OPFS via DevTools → **Application**
  → **Storage** → **Origin Private File System**.

#### Viewing the capture (Wireshark)

The downloaded/saved file is a standard **PCAPNG**. Open it in Wireshark and use normal display
filters. Common ones when debugging guest bring-up:

- `arp` (gateway discovery)
- `bootp` (DHCP)
- `dns` (DNS queries/responses)
- `tcp` / `udp` / `icmp`

Because the capture uses a single Ethernet interface (`guest-eth0`) with direction encoded via the
Enhanced Packet Block `epb_flags` option, both guest TX and RX traffic will appear under the same
interface. Use Wireshark’s packet direction column/metadata (inbound vs outbound) as needed.

#### Automation API (`window.aero.netTrace`)

For browser automation and debugging from DevTools, the web runtime exposes an optional API at:

`window.aero.netTrace`

When present, it implements:

- `isEnabled(): boolean`
- `enable(): void`
- `disable(): void`
- `downloadPcapng(): Promise<Uint8Array>`

Some builds may additionally expose:

- `clear(): void | Promise<void>` (drop buffered frames)
- `getStats(): unknown | Promise<unknown>` (implementation-defined counters such as buffered bytes/frames and drops; some builds may expose `stats()` instead)
  - In the current web runtime, `getStats()` typically returns:
    `{ enabled, records, bytes, droppedRecords, droppedBytes }`.

Worker-runtime note: in the worker-based runtime, these operations are implemented by sending
`net.trace.*` `postMessage()` commands to the net worker and receiving the `net.trace.pcapng`
response. See `web/src/runtime/coordinator.ts` (helper methods) and `web/src/workers/net.worker.ts`
(the handler + `NetTracer`).

Example:

```js
window.aero.netTrace.enable();
// ...reproduce the issue...
const bytes = await window.aero.netTrace.downloadPcapng();
console.log("pcapng bytes:", bytes.byteLength);
```

#### Capture size limits + sensitivity

PCAP/PCAPNG captures can be **large** and may contain **sensitive data** (credentials, cookies, DNS
queries, internal IPs, etc). Treat captures like secrets.

The web tracing buffer is held in-memory and has an explicit size cap to prevent unbounded growth:

- `web/src/net/net_tracer.ts` defaults to **16 MiB** of captured Ethernet payload bytes (`maxBytes`)
  per net worker.
- Once the cap is reached, new frames are **dropped** (so captures may be incomplete for long or
  high-throughput sessions).
- If available, use the live stats (`droppedRecords` / `droppedBytes`) to detect when the capture
  hit its cap and frames were dropped.
- Exporting via the UI (or `downloadPcapng`) uses the net worker’s `takePcapng()` path, which
  **clears the buffer after exporting**. Use this (or the clear button / API) to keep captures
  bounded during long debugging sessions.

Performance note: enabling tracing copies each captured frame into an in-memory buffer (so it has
CPU + memory overhead). Keep tracing disabled unless actively debugging.

Server-side alternative: the production L2 proxy can optionally write per-session `.pcapng` files via
`AERO_L2_CAPTURE_DIR` (see **L2 proxy observability** below). Capturing on both ends can help debug
tunnel framing vs. proxy-side stack issues.

### Privacy / Security Warning

Captures may include sensitive data such as credentials, cookies, private browsing traffic, or internal network metadata. Tracing must default to off and the UI should warn users before enabling or exporting captures. A redaction hook may be provided for stripping known sensitive payloads.

---

## Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| TCP Latency | < 100ms | Additional over native |
| TCP Throughput | ≥ 10 Mbps | Typical web traffic |
| UDP Latency | < 50ms | For real-time apps |
| Connection Setup | < 500ms | Including WebSocket |

---

## Next Steps

- See [Input Devices](./08-input-devices.md) for keyboard/mouse
- See [Browser APIs](./11-browser-apis.md) for WebSocket/WebRTC details
- See [Task Breakdown](./15-agent-task-breakdown.md) for network tasks

---

## Gateway observability (health, readiness, metrics)

The gateway (`backend/aero-gateway`) exposes operational endpoints intended for monitoring and automation:

- `GET /healthz` – liveness
- `GET /readyz` – readiness (returns `503` while shutting down)
- `GET /version` – build/version info (for deploy/debug)
- `GET /metrics` – Prometheus metrics (HTTP request totals/latency; DNS metrics once DoH is enabled)

**Operational guidance:** expose these endpoints only on trusted networks (or behind auth). They are not intended as a public API surface.

For traffic-level debugging, prefer:

- client-side PCAP/trace exports (see **Network Tracing (PCAP/PCAPNG Export)** above), and/or
- infrastructure-level capture (reverse proxy logs, host packet capture in controlled environments).

## L2 proxy observability (health, readiness, metrics, capture)

The production L2 proxy (`crates/aero-l2-proxy`) also exposes basic operational endpoints:

- `GET /healthz` – liveness
- `GET /readyz` – readiness
- `GET /version` – build/version info
- `GET /metrics` – Prometheus metrics (tunnel/session counters + stack/proxy activity)

It also supports optional per-session traffic capture for debugging:

- `AERO_L2_CAPTURE_DIR=/path/to/dir` – when set, writes one `.pcapng` file per tunnel session containing:
  - guest→proxy Ethernet frames (inbound)
  - proxy→guest Ethernet frames (outbound)
- `AERO_L2_PING_INTERVAL_MS=1000` – when set, the proxy sends protocol-level PINGs; RTT is recorded as `l2_ping_rtt_ms` histogram in `/metrics`.
