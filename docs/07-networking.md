# 07 - Networking Stack

## Overview

Windows 7 networking requires TCP/IP stack emulation. Browser constraints mean we must use WebSockets for TCP and WebRTC for UDP.

> **Architecture note:** This document was written in the “in-browser slirp/NAT” direction.
> The current recommended direction is to **tunnel Ethernet frames (L2) to a proxy that runs the
> user-space stack**, to keep browser CPU usage low and avoid implementing TCP in WASM.
> See [`networking-architecture-rfc.md`](./networking-architecture-rfc.md).

---

## Network Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Network Stack                                 │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Windows 7                                                       │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Applications (HTTP, FTP, etc.)                          │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Winsock (ws2_32.dll)                                    │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  TCP/IP Stack (tcpip.sys)                                │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  NDIS (Network Driver Interface)                         │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  NIC Driver (e1000e.sys or virtio-net)                   │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
└───────┼─────────────────────────────────────────────────────────┘
        │  ◄── Emulation Boundary
        ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Aero Network Emulation                        │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Virtual NIC (E1000 or Virtio-net)                       │    │
│  │    - DMA Ring Buffers                                    │    │
│  │    - Packet Queues                                       │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  User-space Network Stack                                │    │
│  │    - Ethernet frame processing                           │    │
│  │    - ARP/DHCP handling                                   │    │
│  │    - NAT (Network Address Translation)                   │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌────────────────┐  ┌────────────────┐                         │
│  │   WebSocket    │  │    WebRTC      │                         │
│  │   (TCP proxy)  │  │  (UDP proxy)   │                         │
│  └────────────────┘  └────────────────┘                         │
│       │                     │                                    │
│       ▼                     ▼                                    │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │           Network Proxy Server (Required)                │    │
│  │    - TCP connection relay                                │    │
│  │    - UDP packet relay                                    │    │
│  │    - DNS resolution                                      │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

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
- eventual production deployments (configure allowlists)

To run in trusted local development mode (allows `127.0.0.1`, RFC1918, etc):

```bash
cd net-proxy
npm ci
AERO_PROXY_OPEN=1 npm run dev
```

Health check:

```bash
curl http://127.0.0.1:8081/healthz
```

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

---

## WebRTC UDP Proxy

### Security warning (server-side relay)

Running a UDP relay makes your server a **network egress point**. If it is reachable by untrusted clients and forwards UDP to arbitrary destinations, it can be abused as an **open proxy / SSRF primitive** (internal network scanning, hitting link-local services, etc.).

**Recommendation:** default to **local-only** deployment (bind on `127.0.0.1` / behind auth) and enforce an explicit destination policy (CIDR + port allowlists) in production.

The WebRTC UDP relay DataChannel framing and signaling schema are specified in
[`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md).

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
        
        // Create data channel for UDP
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
        // Create v1 UDP relay frame:
        // guest_port (u16) + remote_ipv4 (4B) + remote_port (u16) + payload
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

---

## DNS Resolution

In browser environments, DNS lookups should go through the gateway’s **first-party** DNS-over-HTTPS endpoints:

- `/dns-query` (RFC 8484 DoH, `application/dns-message`)
- `/dns-json` (optional JSON convenience endpoint)

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
        // Prefer `/dns-query` for raw DNS messages; `/dns-json` is an optional
        // convenience endpoint for A/AAAA lookups and debugging.
        let url = format!(
            "{}/dns-json?name={}&type=A",
            self.gateway_url,
            hostname
        );
        
        let response = fetch(&url, FetchOptions {
            headers: vec![("Accept".to_string(), "application/dns-json".to_string())],
        }).await?;
        
        let json: DnsJsonResponse = response.json().await?;
        
        if let Some(answer) = json.answer.first() {
            Ok(answer.data.parse()?)
        } else {
            Err(Error::DnsResolutionFailed)
        }
    }
}
```

---

## Virtio-net (Paravirtualized)

> For the exact Windows 7 driver ↔ Aero device-model interoperability contract (PCI transport, virtqueue rules, and virtio-net requirements), see:  
> [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md)

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

## Gateway diagnostics & traffic capture (development / incident response)

The Aero TCP proxy (`backend/aero-gateway`) includes **opt-in** diagnostics tooling intended for local development and controlled incident response.

### Admin stats endpoint

- `GET /admin/stats`
  - Returns current counters:
    - active TCP connections
    - bytes transferred totals (client → target and target → client)
    - DNS cache size
    - uptime + version

#### Security / access control

All `/admin/*` endpoints are locked behind an API key:

- Set `ADMIN_API_KEY` in the gateway process environment.
- Clients must provide the exact value in the `ADMIN_API_KEY` HTTP header.
- If `ADMIN_API_KEY` is not set, `/admin/*` endpoints return `404`.

**Operational guidance:** never expose the admin API to the public Internet. Bind the gateway to localhost where possible or restrict access via firewall / private network.

### Optional per-connection traffic capture (disabled by default)

Traffic capture is **off unless explicitly enabled**.

To enable:

- Set `CAPTURE_DIR=/path/to/captures`
- Optionally set:
  - `CAPTURE_MAX_BYTES` (default: 1 GiB)
  - `CAPTURE_MAX_FILES` (default: 512)

When enabled, the gateway writes one capture file per TCP connection in a simple JSONL format:

- A metadata line (connection timestamp, client IP, session *hash*, target)
- Followed by direction-tagged byte chunks with timestamps

#### Privacy & security risks

Capture files may contain sensitive application data (credentials, cookies, personal data, etc.). Treat captures like packet captures:

- Only enable capture for short periods and in controlled environments.
- Store `CAPTURE_DIR` on encrypted storage when possible.
- Ensure the directory is not world-readable.
- Delete captures after use.

To avoid leaking gateway session secrets in the capture itself, the metadata stores **only a session hash** (SHA-256), never the raw secret.
