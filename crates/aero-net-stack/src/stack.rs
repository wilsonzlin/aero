#![forbid(unsafe_code)]

use crate::packet::{
    parse_single_query, qname_to_string, ArpPacket, ArpPacketBuilder, DhcpMessage, DhcpMessageType,
    DhcpOfferAckBuilder, DnsResponseBuilder, DnsResponseCode, DnsType, EtherType, EthernetFrame,
    EthernetFrameBuilder, IcmpEchoBuilder, Icmpv4Packet, Ipv4Packet, Ipv4PacketBuilder,
    Ipv4Protocol, MacAddr, TcpFlags, TcpSegment, TcpSegmentBuilder, UdpPacket, UdpPacketBuilder,
    ARP_OP_REPLY, ARP_OP_REQUEST, HTYPE_ETHERNET, PTYPE_IPV4,
};
use crate::policy::HostPolicy;
use crate::snapshot::{
    DnsCacheEntrySnapshot, NetworkStackSnapshotState, TcpConnectionSnapshot, TcpConnectionStatus,
    TcpRestorePolicy,
};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotResult, SnapshotVersion};
use core::net::Ipv4Addr;
use std::collections::{HashMap, HashSet, VecDeque};

pub type Millis = u64;

#[derive(Debug, Clone)]
pub struct StackConfig {
    pub our_mac: MacAddr,
    pub gateway_ip: Ipv4Addr,
    pub guest_ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub dns_ip: Ipv4Addr,

    pub dhcp_lease_time_secs: u32,
    pub webrtc_udp: bool,

    pub host_policy: HostPolicy,

    /// Maximum number of concurrent TCP connections tracked by the stack.
    ///
    /// When exceeded, new SYNs are rejected with a guest-visible TCP RST and no connection state is
    /// allocated.
    pub max_tcp_connections: u32,

    /// Maximum number of bytes of TCP payload buffered per connection while the proxy-side TCP
    /// tunnel is not yet connected.
    ///
    /// When exceeded, the connection is aborted with a guest-visible TCP RST and the connection
    /// state is dropped.
    pub max_buffered_tcp_bytes_per_conn: u32,

    /// Maximum number of entries in the DNS cache.
    ///
    /// When exceeded, the cache deterministically evicts the oldest entry (FIFO by insertion
    /// order). Cache hits do not affect eviction order.
    pub max_dns_cache_entries: u32,

    /// Maximum number of in-flight DNS resolutions.
    ///
    /// When exceeded, new DNS queries are answered immediately with SERVFAIL.
    pub max_pending_dns: u32,
}

impl Default for StackConfig {
    fn default() -> Self {
        Self {
            our_mac: MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]),
            gateway_ip: Ipv4Addr::new(10, 0, 2, 2),
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            dns_ip: Ipv4Addr::new(10, 0, 2, 2),
            dhcp_lease_time_secs: 86400,
            webrtc_udp: true,
            host_policy: HostPolicy::default(),
            max_tcp_connections: 1024,
            max_buffered_tcp_bytes_per_conn: 256 * 1024,
            max_dns_cache_entries: 10_000,
            max_pending_dns: 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UdpTransport {
    WebRtc,
    Proxy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// An Ethernet frame that should be delivered to the guest NIC (i.e. inbound to the guest).
    EmitFrame(Vec<u8>),

    /// Open a new TCP tunnel to the proxy (typically a WebSocket) for the given remote endpoint.
    TcpProxyConnect {
        connection_id: u32,
        remote_ip: Ipv4Addr,
        remote_port: u16,
    },
    /// Send payload bytes on an established TCP tunnel.
    TcpProxySend { connection_id: u32, data: Vec<u8> },
    /// Close the TCP tunnel.
    TcpProxyClose { connection_id: u32 },

    /// Send a UDP datagram to the proxy.
    UdpProxySend {
        transport: UdpTransport,
        src_port: u16,
        dst_ip: Ipv4Addr,
        dst_port: u16,
        data: Vec<u8>,
    },

    /// Resolve a hostname via DoH or proxy-side DNS.
    DnsResolve { request_id: u32, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TcpProxyEvent {
    Connected { connection_id: u32 },
    Data { connection_id: u32, data: Vec<u8> },
    Closed { connection_id: u32 },
    Error { connection_id: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpProxyEvent {
    pub src_ip: Ipv4Addr,
    pub src_port: u16,
    pub dst_port: u16,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsResolved {
    pub request_id: u32,
    pub name: String,
    pub addr: Option<Ipv4Addr>,
    pub ttl_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TcpKey {
    guest_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
}

#[derive(Debug, Clone)]
struct TcpConn {
    id: u32,
    guest_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,

    guest_isn: u32,
    guest_next_seq: u32,

    our_isn: u32,
    our_next_seq: u32,

    syn_acked: bool,
    fin_sent: bool,
    fin_seq: u32,
    fin_acked: bool,
    guest_fin_received: bool,

    proxy_connected: bool,
    proxy_reconnecting: bool,
    /// Whether sequence numbers are safe to use for emitting guest-visible TCP packets.
    ///
    /// When restoring with [`TcpRestorePolicy::Reconnect`], the stack does **not** attempt to
    /// snapshot full TCP stream state. Instead, it restores only connection bookkeeping (IDs +
    /// endpoints) and treats all connections as proxy-disconnected. We then resynchronize the
    /// guest-facing sequence state opportunistically from the first post-restore ACK-bearing packet
    /// from the guest.
    seq_synced: bool,
    buffered_to_proxy: Vec<Vec<u8>>,
    buffered_to_proxy_bytes: usize,
}

impl TcpConn {
    fn on_guest_ack(&mut self, ack: u32) {
        if !self.syn_acked && ack.wrapping_sub(self.our_isn) >= 1 {
            self.syn_acked = true;
        }
        if self.fin_sent && !self.fin_acked && ack.wrapping_sub(self.fin_seq) >= 1 {
            self.fin_acked = true;
        }
    }

    fn should_remove(&self) -> bool {
        // Remove when both sides have exchanged FINs and the guest ACKed our FIN.
        self.guest_fin_received && self.fin_sent && self.fin_acked
    }
}

#[derive(Debug, Clone)]
struct PendingDns {
    txid: u16,
    src_port: u16,
    name: String,
    qname: Vec<u8>,
    qtype: u16,
    qclass: u16,
    rd: bool,
}

#[derive(Debug, Clone)]
struct DnsCacheEntry {
    addr: Ipv4Addr,
    expires_at_ms: Millis,
}

#[derive(Debug)]
pub struct NetworkStack {
    cfg: StackConfig,
    guest_mac: Option<MacAddr>,
    ip_assigned: bool,
    next_tcp_id: u32,
    next_dns_id: u32,
    ipv4_ident: u16,
    /// Offset applied to caller-provided `now_ms` to produce a stable internal time base that
    /// survives snapshot/restore.
    ///
    /// Internal time is computed as:
    /// `internal_now_ms = now_ms + time_offset_ms` (with saturating bounds).
    time_offset_ms: i64,
    /// When `Some`, the next call that supplies a `now_ms` value will recompute `time_offset_ms`
    /// so that `internal_now_ms` matches this anchor.
    ///
    /// This is used during snapshot restore because the host's monotonic time base is typically
    /// reset when a backend is recreated.
    restore_time_anchor_ms: Option<Millis>,
    /// Most recently observed internal time.
    ///
    /// Snapshots store this value so DNS cache expiry timestamps can be interpreted correctly after
    /// restore, even if the host's `now_ms` starts over from 0.
    last_now_ms: Millis,

    tcp: HashMap<TcpKey, TcpConn>,
    pending_dns: HashMap<u32, PendingDns>,
    dns_cache: HashMap<String, DnsCacheEntry>,
    dns_cache_fifo: VecDeque<String>,
}

impl NetworkStack {
    pub fn new(cfg: StackConfig) -> Self {
        Self {
            cfg,
            guest_mac: None,
            ip_assigned: false,
            next_tcp_id: 1,
            next_dns_id: 1,
            ipv4_ident: 1,
            time_offset_ms: 0,
            restore_time_anchor_ms: None,
            last_now_ms: 0,
            tcp: HashMap::new(),
            pending_dns: HashMap::new(),
            dns_cache: HashMap::new(),
            dns_cache_fifo: VecDeque::new(),
        }
    }

    pub fn config(&self) -> &StackConfig {
        &self.cfg
    }

    pub fn is_ip_assigned(&self) -> bool {
        self.ip_assigned
    }

    pub fn set_network_enabled(&mut self, enabled: bool) {
        self.cfg.host_policy.enabled = enabled;
    }

    /// Convert the host-supplied `now_ms` into the stack's internal time base.
    ///
    /// The host glue (`NetStackBackend`) typically supplies `now_ms` as milliseconds since the
    /// backend was created. When restoring from a snapshot, that clock usually restarts from 0, but
    /// the stack stores DNS cache expiry timestamps as absolute-ish `expires_at_ms` values.
    ///
    /// To keep TTL behavior stable across snapshot/restore, we align the post-restore time base to
    /// the snapshotted `last_now_ms` on the first call after `load_state()`.
    fn sync_internal_now_ms(&mut self, now_ms: Millis) -> Millis {
        if let Some(anchor) = self.restore_time_anchor_ms.take() {
            let diff = anchor as i128 - now_ms as i128;
            self.time_offset_ms = diff.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        }

        let internal = {
            let sum = now_ms as i128 + self.time_offset_ms as i128;
            if sum < 0 {
                0
            } else if sum > u64::MAX as i128 {
                u64::MAX
            } else {
                sum as u64
            }
        };
        self.last_now_ms = internal;
        internal
    }

    pub fn process_outbound_ethernet(&mut self, frame: &[u8], now_ms: Millis) -> Vec<Action> {
        let now_ms = self.sync_internal_now_ms(now_ms);
        let eth = match EthernetFrame::parse(frame) {
            Ok(eth) => eth,
            Err(_) => return Vec::new(),
        };

        // Learn guest MAC (we need it for replies).
        self.guest_mac.get_or_insert(eth.src_mac());

        match eth.ethertype() {
            EtherType::ARP => self.handle_arp(eth.payload()),
            EtherType::IPV4 => self.handle_ipv4(eth.payload(), now_ms),
            _ => Vec::new(),
        }
    }

    pub fn handle_tcp_proxy_event(&mut self, event: TcpProxyEvent, now_ms: Millis) -> Vec<Action> {
        // Keep the internal time base in sync even though most TCP proxy events don't currently
        // depend on `now_ms`.
        let _ = self.sync_internal_now_ms(now_ms);
        let mut out = Vec::new();

        // Data is latency-sensitive; handle it without any extra bookkeeping first.
        if let TcpProxyEvent::Data {
            connection_id,
            data,
        } = event
        {
            self.on_tcp_proxy_data(connection_id, data, &mut out);
            return out;
        }

        let connection_id = match event {
            TcpProxyEvent::Connected { connection_id } => connection_id,
            TcpProxyEvent::Closed { connection_id } => connection_id,
            TcpProxyEvent::Error { connection_id } => connection_id,
            TcpProxyEvent::Data { .. } => unreachable!(),
        };

        let Some(key) = self
            .tcp
            .iter()
            .find_map(|(k, c)| (c.id == connection_id).then_some(*k))
        else {
            return out;
        };

        // Temporarily remove the connection so we can mutate `self` (e.g. allocate IPv4 IDs) while
        // also mutating the connection.
        let mut conn = match self.tcp.remove(&key) {
            Some(c) => c,
            None => return out,
        };

        match event {
            TcpProxyEvent::Connected { .. } => {
                conn.proxy_connected = true;
                conn.proxy_reconnecting = false;
                for chunk in conn.buffered_to_proxy.drain(..) {
                    out.push(Action::TcpProxySend {
                        connection_id: conn.id,
                        data: chunk,
                    });
                }
                conn.buffered_to_proxy_bytes = 0;
                self.tcp.insert(key, conn);
            }
            TcpProxyEvent::Closed { .. } => {
                if !conn.seq_synced {
                    // We cannot emit a guest-visible FIN/RST without sequence state. Drop the
                    // connection and let the guest time out or reconnect.
                    return out;
                }

                // If the tunnel closed before the guest completed the SYN handshake, abort with
                // an RST instead of a FIN.
                if !conn.syn_acked {
                    out.extend(self.emit_tcp_rst(&conn));
                    return out;
                }

                if !conn.fin_sent {
                    out.extend(self.emit_tcp_fin(&mut conn));
                }

                if !conn.should_remove() {
                    self.tcp.insert(key, conn);
                }
            }
            TcpProxyEvent::Error { .. } => {
                if !conn.seq_synced {
                    // Cannot emit a meaningful RST without seq/ack state; drop.
                    return out;
                }
                out.extend(self.emit_tcp_rst(&conn));
                // Drop the connection (guest will see RST or time out).
            }
            TcpProxyEvent::Data { .. } => unreachable!(),
        }

        out
    }

    pub fn handle_udp_proxy_event(&mut self, event: UdpProxyEvent, now_ms: Millis) -> Vec<Action> {
        let _ = self.sync_internal_now_ms(now_ms);
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        if !self.ip_assigned {
            return Vec::new();
        }

        let Ok(udp) = UdpPacketBuilder {
            src_port: event.src_port,
            dst_port: event.dst_port,
            payload: &event.data,
        }
        .build_vec(event.src_ip, self.cfg.guest_ip) else {
            return Vec::new();
        };

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::UDP,
            src_ip: event.src_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &udp,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    pub fn handle_dns_resolved(&mut self, resolved: DnsResolved, now_ms: Millis) -> Vec<Action> {
        let now_ms = self.sync_internal_now_ms(now_ms);
        let pending = match self.pending_dns.remove(&resolved.request_id) {
            Some(p) => p,
            None => return Vec::new(),
        };

        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };

        // Apply outbound IP policy to DNS results too, otherwise the guest can learn disallowed
        // IPs even though follow-up TCP/UDP connections would be denied.
        let allowed_addr = resolved
            .addr
            .filter(|ip| self.cfg.host_policy.allows_ip(*ip));

        if let Some(addr) = allowed_addr {
            let expires_at_ms = now_ms.saturating_add(resolved.ttl_secs as u64 * 1000);
            self.insert_dns_cache(
                pending.name.clone(),
                DnsCacheEntry {
                    addr,
                    expires_at_ms,
                },
            );
        }

        let (answer_a, rcode) = match allowed_addr {
            Some(addr) => (Some(addr), DnsResponseCode::NoError),
            None => (None, DnsResponseCode::NameError),
        };

        let Ok(dns_payload) = DnsResponseBuilder {
            id: pending.txid,
            rd: pending.rd,
            rcode,
            qname: &pending.qname,
            qtype: pending.qtype,
            qclass: pending.qclass,
            answer_a,
            ttl: resolved.ttl_secs,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(udp) = UdpPacketBuilder {
            src_port: 53,
            dst_port: pending.src_port,
            payload: &dns_payload,
        }
        .build_vec(self.cfg.dns_ip, self.cfg.guest_ip) else {
            return Vec::new();
        };

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::UDP,
            src_ip: self.cfg.dns_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &udp,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn handle_arp(&mut self, payload: &[u8]) -> Vec<Action> {
        let arp = match ArpPacket::parse(payload) {
            Ok(a) => a,
            Err(_) => return Vec::new(),
        };

        // We only implement Ethernet/IPv4 ARP.
        if arp.htype() != HTYPE_ETHERNET
            || arp.ptype() != PTYPE_IPV4
            || arp.hlen() != 6
            || arp.plen() != 4
        {
            return Vec::new();
        }

        let sender_hw = match arp.sender_mac() {
            Some(m) => m,
            None => return Vec::new(),
        };
        let sender_ip = match arp.sender_ip() {
            Some(ip) => ip,
            None => return Vec::new(),
        };
        let target_ip = match arp.target_ip() {
            Some(ip) => ip,
            None => return Vec::new(),
        };

        // Cache guest IP->MAC when known.
        if sender_ip != Ipv4Addr::UNSPECIFIED {
            // Only store if we're already sure about the guest IP (to avoid poisoning ourselves
            // from DHCP's 0.0.0.0 phase).
            if self.ip_assigned && sender_ip == self.cfg.guest_ip {
                self.guest_mac = Some(sender_hw);
            }
        }

        if arp.opcode() != ARP_OP_REQUEST {
            return Vec::new();
        }
        if target_ip != self.cfg.gateway_ip && target_ip != self.cfg.dns_ip {
            return Vec::new();
        }

        let Ok(reply) = ArpPacketBuilder {
            opcode: ARP_OP_REPLY,
            sender_mac: self.cfg.our_mac,
            sender_ip: target_ip,
            target_mac: sender_hw,
            target_ip: sender_ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: sender_hw,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::ARP,
            payload: &reply,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn handle_ipv4(&mut self, payload: &[u8], now_ms: Millis) -> Vec<Action> {
        let ip = match Ipv4Packet::parse(payload) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        match ip.protocol() {
            Ipv4Protocol::UDP => self.handle_udp(ip, now_ms),
            Ipv4Protocol::TCP => self.handle_tcp(ip),
            Ipv4Protocol::ICMP => self.handle_icmp(ip),
            _ => Vec::new(),
        }
    }

    fn handle_udp(&mut self, ip: Ipv4Packet<'_>, now_ms: Millis) -> Vec<Action> {
        let udp = match UdpPacket::parse(ip.payload()) {
            Ok(u) => u,
            Err(_) => return Vec::new(),
        };

        // DHCP (client->server)
        if udp.src_port() == 68 && udp.dst_port() == 67 {
            return self.handle_dhcp(ip, udp);
        }

        // DNS queries to our advertised DNS IP.
        if udp.dst_port() == 53 && ip.dst_ip() == self.cfg.dns_ip {
            return self.handle_dns_query(ip, udp, now_ms);
        }

        if !self.ip_assigned {
            return Vec::new();
        }

        // Forward UDP to proxy.
        if !self.cfg.host_policy.enabled || !self.cfg.host_policy.allows_ip(ip.dst_ip()) {
            return Vec::new();
        }

        vec![Action::UdpProxySend {
            transport: if self.cfg.webrtc_udp {
                UdpTransport::WebRtc
            } else {
                UdpTransport::Proxy
            },
            src_port: udp.src_port(),
            dst_ip: ip.dst_ip(),
            dst_port: udp.dst_port(),
            data: udp.payload().to_vec(),
        }]
    }

    fn handle_dns_query(
        &mut self,
        _ip: Ipv4Packet<'_>,
        udp: UdpPacket<'_>,
        now_ms: Millis,
    ) -> Vec<Action> {
        let query = match parse_single_query(udp.payload()) {
            Ok(q) => q,
            Err(_) => return Vec::new(),
        };
        let qname = query.qname;
        let mut name = match qname_to_string(qname) {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        };

        // Normalise to lower-case for DNS cache keys and host actions.
        // `make_ascii_lowercase` is in-place (no extra allocation).
        while name.ends_with('.') {
            name.pop();
        }
        name.make_ascii_lowercase();
        let rd = query.recursion_desired();
        let qtype = query.qtype;
        let qclass = query.qclass;

        // Enforce domain policy early.
        if !self.cfg.host_policy.enabled || !self.cfg.host_policy.allows_domain(&name) {
            return self.emit_dns_error(
                query.id,
                qname,
                qtype,
                qclass,
                udp.src_port(),
                rd,
                DnsResponseCode::NameError,
            );
        }

        // We only implement A/IN. For AAAA and friends, return NOTIMP so clients can fall back to
        // A queries instead of treating the name as missing.
        if qtype != DnsType::A as u16 || qclass != 1 {
            return self.emit_dns_error(
                query.id,
                qname,
                qtype,
                qclass,
                udp.src_port(),
                rd,
                DnsResponseCode::NotImplemented,
            );
        }

        if let Some(entry) = self.dns_cache.get(&name) {
            if entry.expires_at_ms > now_ms {
                // Policy can change over time; re-check before serving from cache.
                if !self.cfg.host_policy.allows_ip(entry.addr) {
                    return self.emit_dns_error(
                        query.id,
                        qname,
                        qtype,
                        qclass,
                        udp.src_port(),
                        rd,
                        DnsResponseCode::NameError,
                    );
                }
                let guest_mac = match self.guest_mac {
                    Some(m) => m,
                    None => return Vec::new(),
                };

                 let Ok(dns_payload) = DnsResponseBuilder {
                     id: query.id,
                     rd,
                     rcode: DnsResponseCode::NoError,
                     qname,
                     qtype,
                     qclass,
                     answer_a: Some(entry.addr),
                    // TTL is encoded as a u32 in DNS responses. Snapshots may contain
                    // untrusted `expires_at_ms` values, so clamp rather than truncate.
                    ttl: ((entry.expires_at_ms - now_ms) / 1000).min(u32::MAX as u64) as u32,
                 }
                 .build_vec() else {
                     return Vec::new();
                 };

                let Ok(udp_out) = UdpPacketBuilder {
                    src_port: 53,
                    dst_port: udp.src_port(),
                    payload: &dns_payload,
                }
                .build_vec(self.cfg.dns_ip, self.cfg.guest_ip) else {
                    return Vec::new();
                };

                let Ok(ip_out) = Ipv4PacketBuilder {
                    dscp_ecn: 0,
                    identification: self.next_ipv4_ident(),
                    flags_fragment: 0x4000, // DF
                    ttl: 64,
                    protocol: Ipv4Protocol::UDP,
                    src_ip: self.cfg.dns_ip,
                    dst_ip: self.cfg.guest_ip,
                    options: &[],
                    payload: &udp_out,
                }
                .build_vec() else {
                    return Vec::new();
                };

                let Ok(eth) = EthernetFrameBuilder {
                    dest_mac: guest_mac,
                    src_mac: self.cfg.our_mac,
                    ethertype: EtherType::IPV4,
                    payload: &ip_out,
                }
                .build_vec() else {
                    return Vec::new();
                };

                return vec![Action::EmitFrame(eth)];
            }
        }

        // Avoid unbounded growth if the guest sends many DNS queries while the host/proxy is slow
        // to resolve them.
        let max_pending_dns = self.cfg.max_pending_dns as usize;
        if max_pending_dns == 0 || self.pending_dns.len() >= max_pending_dns {
            return self.emit_dns_error(
                query.id,
                qname,
                qtype,
                qclass,
                udp.src_port(),
                rd,
                DnsResponseCode::ServerFailure,
            );
        }

        let request_id = self.next_dns_id;
        self.next_dns_id += 1;
        self.pending_dns.insert(
            request_id,
            PendingDns {
                txid: query.id,
                src_port: udp.src_port(),
                name: name.clone(),
                qname: qname.to_vec(),
                qtype,
                qclass,
                rd,
            },
        );
        vec![Action::DnsResolve { request_id, name }]
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_dns_error(
        &mut self,
        txid: u16,
        qname: &[u8],
        qtype: u16,
        qclass: u16,
        dst_port: u16,
        rd: bool,
        rcode: DnsResponseCode,
    ) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };

        let Ok(dns_payload) = DnsResponseBuilder {
            id: txid,
            rd,
            rcode,
            qname,
            qtype,
            qclass,
            answer_a: None,
            ttl: 0,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(udp) = UdpPacketBuilder {
            src_port: 53,
            dst_port,
            payload: &dns_payload,
        }
        .build_vec(self.cfg.dns_ip, self.cfg.guest_ip) else {
            return Vec::new();
        };

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::UDP,
            src_ip: self.cfg.dns_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &udp,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn handle_dhcp(&mut self, _ip: Ipv4Packet<'_>, udp: UdpPacket<'_>) -> Vec<Action> {
        let msg = match DhcpMessage::parse(udp.payload()) {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };

        let guest_mac = msg.client_mac;

        let (reply_type, mark_assigned) = match msg.message_type {
            DhcpMessageType::Discover => (DhcpMessageType::Offer, false),
            DhcpMessageType::Request => (DhcpMessageType::Ack, true),
            _ => return Vec::new(),
        };

        let dns_servers = [self.cfg.dns_ip];
        let Ok(dhcp) = DhcpOfferAckBuilder {
            message_type: reply_type as u8,
            transaction_id: msg.transaction_id,
            flags: msg.flags,
            client_mac: guest_mac,
            your_ip: self.cfg.guest_ip,
            server_ip: self.cfg.gateway_ip,
            subnet_mask: self.cfg.netmask,
            router: self.cfg.gateway_ip,
            dns_servers: &dns_servers,
            lease_time_secs: self.cfg.dhcp_lease_time_secs,
        }
        .build_vec() else {
            return Vec::new();
        };

        if mark_assigned {
            self.ip_assigned = true;
            self.guest_mac = Some(guest_mac);
        }

        let Ok(udp_out) = UdpPacketBuilder {
            src_port: 67,
            dst_port: 68,
            payload: &dhcp,
        }
        .build_vec(self.cfg.gateway_ip, Ipv4Addr::BROADCAST) else {
            return Vec::new();
        };

        let Ok(ip_out) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::UDP,
            src_ip: self.cfg.gateway_ip,
            dst_ip: Ipv4Addr::BROADCAST,
            options: &[],
            payload: &udp_out,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: MacAddr::BROADCAST,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip_out,
        }
        .build_vec() else {
            return Vec::new();
        };

        let mut out = vec![Action::EmitFrame(eth)];

        // Some stacks accept only unicast replies once the client MAC is known. Send a second copy
        // directly to the guest MAC/IP when possible.
        if guest_mac != MacAddr::BROADCAST {
            let Ok(udp_unicast) = UdpPacketBuilder {
                src_port: 67,
                dst_port: 68,
                payload: &dhcp,
            }
            .build_vec(self.cfg.gateway_ip, self.cfg.guest_ip) else {
                return out;
            };

            let Ok(ip_unicast) = Ipv4PacketBuilder {
                dscp_ecn: 0,
                identification: self.next_ipv4_ident(),
                flags_fragment: 0x4000, // DF
                ttl: 64,
                protocol: Ipv4Protocol::UDP,
                src_ip: self.cfg.gateway_ip,
                dst_ip: self.cfg.guest_ip,
                options: &[],
                payload: &udp_unicast,
            }
            .build_vec() else {
                return out;
            };

            let Ok(eth_unicast) = EthernetFrameBuilder {
                dest_mac: guest_mac,
                src_mac: self.cfg.our_mac,
                ethertype: EtherType::IPV4,
                payload: &ip_unicast,
            }
            .build_vec() else {
                return out;
            };

            out.push(Action::EmitFrame(eth_unicast));
        }

        out
    }

    fn handle_icmp(&mut self, ip: Ipv4Packet<'_>) -> Vec<Action> {
        if !self.ip_assigned {
            return Vec::new();
        }

        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };

        // Only answer echo requests addressed to our gateway IP.
        if ip.dst_ip() != self.cfg.gateway_ip {
            return Vec::new();
        }
        let pkt = match Icmpv4Packet::parse(ip.payload()) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        let echo = match pkt.echo() {
            Some(e) if e.icmp_type == 8 => e,
            _ => return Vec::new(),
        };

        let icmp = match IcmpEchoBuilder::echo_reply(echo.identifier, echo.sequence, echo.payload)
            .build_vec()
        {
            Ok(pkt) => pkt,
            Err(_) => return Vec::new(),
        };

        let Ok(ip_out) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::ICMP,
            src_ip: self.cfg.gateway_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &icmp,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip_out,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn handle_tcp(&mut self, ip: Ipv4Packet<'_>) -> Vec<Action> {
        let tcp = match TcpSegment::parse(ip.payload()) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        if !self.ip_assigned {
            return Vec::new();
        }

        let key = TcpKey {
            guest_port: tcp.src_port(),
            remote_ip: ip.dst_ip(),
            remote_port: tcp.dst_port(),
        };

        let flags = tcp.flags();

        if flags.contains(TcpFlags::RST) {
            if let Some(conn) = self.tcp.remove(&key) {
                return vec![Action::TcpProxyClose {
                    connection_id: conn.id,
                }];
            }
            return Vec::new();
        }

        if !self.tcp.contains_key(&key) {
            // Only start connections when we see SYN.
            if !flags.contains(TcpFlags::SYN) || flags.contains(TcpFlags::ACK) {
                return Vec::new();
            }

            // Enforce security policy *before* advertising a connection to the guest.
            if !self.cfg.host_policy.enabled || !self.cfg.host_policy.allows_ip(ip.dst_ip()) {
                return self.emit_tcp_rst_for_syn(
                    ip.src_ip(),
                    tcp.src_port(),
                    ip.dst_ip(),
                    tcp.dst_port(),
                    tcp.seq_number(),
                );
            }

            // Cap concurrent connections to avoid unbounded memory use.
            let max_tcp_connections = self.cfg.max_tcp_connections as usize;
            if max_tcp_connections == 0 || self.tcp.len() >= max_tcp_connections {
                return self.emit_tcp_rst_for_syn(
                    ip.src_ip(),
                    tcp.src_port(),
                    ip.dst_ip(),
                    tcp.dst_port(),
                    tcp.seq_number(),
                );
            }

            let guest_isn = tcp.seq_number();
            let our_isn = self.allocate_isn();
            let conn_id = self.next_tcp_id;
            self.next_tcp_id += 1;

            let conn = TcpConn {
                id: conn_id,
                guest_port: tcp.src_port(),
                remote_ip: ip.dst_ip(),
                remote_port: tcp.dst_port(),
                guest_isn,
                guest_next_seq: guest_isn.wrapping_add(1),
                our_isn,
                our_next_seq: our_isn.wrapping_add(1),
                syn_acked: false,
                fin_sent: false,
                fin_seq: 0,
                fin_acked: false,
                guest_fin_received: false,
                proxy_connected: false,
                proxy_reconnecting: false,
                seq_synced: true,
                buffered_to_proxy: Vec::new(),
                buffered_to_proxy_bytes: 0,
            };

            let mut actions = Vec::new();
            actions.push(Action::TcpProxyConnect {
                connection_id: conn_id,
                remote_ip: conn.remote_ip,
                remote_port: conn.remote_port,
            });
            actions.extend(self.emit_tcp_syn_ack(&conn));
            self.tcp.insert(key, conn);
            return actions;
        }

        // Temporarily remove the connection so we can mutate `self` (e.g. allocate IPv4 IDs) while
        // holding a mutable connection.
        let mut conn = match self.tcp.remove(&key) {
            Some(c) => c,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        let payload = tcp.payload();

        // Best-effort sequence resynchronization for connections restored under
        // [`TcpRestorePolicy::Reconnect`].
        //
        // We intentionally do not serialize full TCP stream state (seq/ack numbers). Restored
        // connections start with `seq_synced = false` and we infer the guest-facing seq/ack from
        // the first ACK-bearing segment we observe after restore.
        if !conn.seq_synced {
            // If the guest is starting a fresh connection attempt (SYN), drop the restored
            // connection and treat this as a new handshake.
            if flags.contains(TcpFlags::SYN) && !flags.contains(TcpFlags::ACK) {
                out.push(Action::TcpProxyClose {
                    connection_id: conn.id,
                });
                let mut actions = out;

                // Re-run the "new connection" code path inline (mirrors the `!contains_key` branch
                // above).
                if !self.cfg.host_policy.enabled || !self.cfg.host_policy.allows_ip(ip.dst_ip()) {
                    actions.extend(self.emit_tcp_rst_for_syn(
                        ip.src_ip(),
                        tcp.src_port(),
                        ip.dst_ip(),
                        tcp.dst_port(),
                        tcp.seq_number(),
                    ));
                    return actions;
                }

                let max_tcp_connections = self.cfg.max_tcp_connections as usize;
                if max_tcp_connections == 0 || self.tcp.len() >= max_tcp_connections {
                    actions.extend(self.emit_tcp_rst_for_syn(
                        ip.src_ip(),
                        tcp.src_port(),
                        ip.dst_ip(),
                        tcp.dst_port(),
                        tcp.seq_number(),
                    ));
                    return actions;
                }

                let guest_isn = tcp.seq_number();
                let our_isn = self.allocate_isn();
                let conn_id = self.next_tcp_id;
                self.next_tcp_id += 1;

                let conn = TcpConn {
                    id: conn_id,
                    guest_port: tcp.src_port(),
                    remote_ip: ip.dst_ip(),
                    remote_port: tcp.dst_port(),
                    guest_isn,
                    guest_next_seq: guest_isn.wrapping_add(1),
                    our_isn,
                    our_next_seq: our_isn.wrapping_add(1),
                    syn_acked: false,
                    fin_sent: false,
                    fin_seq: 0,
                    fin_acked: false,
                    guest_fin_received: false,
                    proxy_connected: false,
                    proxy_reconnecting: false,
                    seq_synced: true,
                    buffered_to_proxy: Vec::new(),
                    buffered_to_proxy_bytes: 0,
                };

                actions.push(Action::TcpProxyConnect {
                    connection_id: conn_id,
                    remote_ip: conn.remote_ip,
                    remote_port: conn.remote_port,
                });
                actions.extend(self.emit_tcp_syn_ack(&conn));
                self.tcp.insert(key, conn);
                return actions;
            }

            // Without ACK, we can't learn the expected send sequence number from the guest.
            if !flags.contains(TcpFlags::ACK) {
                self.tcp.insert(key, conn);
                return out;
            }

            conn.syn_acked = true;
            conn.guest_next_seq = tcp.seq_number();
            conn.our_next_seq = tcp.ack_number();
            conn.seq_synced = true;
        }

        // Retransmitted SYN: resend SYN-ACK for idempotence.
        if flags.contains(TcpFlags::SYN)
            && !flags.contains(TcpFlags::ACK)
            && !conn.syn_acked
            && tcp.seq_number() == conn.guest_isn
        {
            out.extend(self.emit_tcp_syn_ack(&conn));
        }

        // ACK bookkeeping (handshake + FIN).
        if flags.contains(TcpFlags::ACK) {
            conn.on_guest_ack(tcp.ack_number());
        }

        // Payload.
        if !payload.is_empty() {
            // We intentionally do not implement full TCP reassembly: accept only in-order payload.
            // Out-of-order segments are dropped and must be retransmitted by the guest. This stack
            // also assumes FIN arrives in-order; if the transport can reorder segments (e.g. a
            // reliable-but-unordered WebRTC DataChannel), it can close the upstream connection
            // prematurely. Therefore, production L2 tunneling requires an ordered transport.
            let seg_seq = tcp.seq_number();
            let seg_end = seg_seq.wrapping_add(payload.len() as u32);
            if seg_seq == conn.guest_next_seq {
                // In-order segment.
                if conn.proxy_connected {
                    out.push(Action::TcpProxySend {
                        connection_id: conn.id,
                        data: payload.to_vec(),
                    });
                } else {
                    if self.tcp_buffer_would_exceed_limit(&conn, payload.len()) {
                        out.extend(self.emit_tcp_rst(&conn));
                        out.push(Action::TcpProxyClose {
                            connection_id: conn.id,
                        });
                        return out;
                    }
                    conn.buffered_to_proxy_bytes =
                        conn.buffered_to_proxy_bytes.saturating_add(payload.len());
                    conn.buffered_to_proxy.push(payload.to_vec());
                }
                conn.guest_next_seq = conn.guest_next_seq.wrapping_add(payload.len() as u32);
                out.extend(self.emit_tcp_ack(&conn));
            } else if seg_end <= conn.guest_next_seq {
                // Fully duplicate segment; re-ACK for retransmit tolerance.
                out.extend(self.emit_tcp_ack(&conn));
            } else if seg_seq < conn.guest_next_seq {
                // Overlapping segment; forward only unseen tail.
                let offset = conn.guest_next_seq.wrapping_sub(seg_seq) as usize;
                let tail = &payload[offset..];
                if conn.proxy_connected {
                    out.push(Action::TcpProxySend {
                        connection_id: conn.id,
                        data: tail.to_vec(),
                    });
                } else {
                    if self.tcp_buffer_would_exceed_limit(&conn, tail.len()) {
                        out.extend(self.emit_tcp_rst(&conn));
                        out.push(Action::TcpProxyClose {
                            connection_id: conn.id,
                        });
                        return out;
                    }
                    conn.buffered_to_proxy_bytes =
                        conn.buffered_to_proxy_bytes.saturating_add(tail.len());
                    conn.buffered_to_proxy.push(tail.to_vec());
                }
                conn.guest_next_seq = conn.guest_next_seq.wrapping_add(tail.len() as u32);
                out.extend(self.emit_tcp_ack(&conn));
            } else {
                // Out-of-order: ACK what we have and drop.
                out.extend(self.emit_tcp_ack(&conn));
            }
        }

        // FIN.
        if flags.contains(TcpFlags::FIN) {
            let fin_seq = tcp.seq_number().wrapping_add(payload.len() as u32);
            if fin_seq == conn.guest_next_seq {
                conn.guest_next_seq = conn.guest_next_seq.wrapping_add(1);
                conn.guest_fin_received = true;
            }
            out.extend(self.emit_tcp_ack(&conn));
            out.push(Action::TcpProxyClose {
                connection_id: conn.id,
            });

            if !conn.fin_sent {
                out.extend(self.emit_tcp_fin(&mut conn));
            }
        }

        if !conn.should_remove() {
            self.tcp.insert(key, conn);
        }
        out
    }

    fn on_tcp_proxy_data(&mut self, connection_id: u32, data: Vec<u8>, out: &mut Vec<Action>) {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return,
        };
        let Some(key) = self
            .tcp
            .iter()
            .find_map(|(k, c)| (c.id == connection_id).then_some(*k))
        else {
            return;
        };

        let (remote_ip, remote_port, guest_port, seq_number, ack_number) = match self.tcp.get(&key)
        {
            Some(conn) => {
                if conn.fin_sent || !conn.syn_acked || !conn.seq_synced {
                    return;
                }
                (
                    conn.remote_ip,
                    conn.remote_port,
                    conn.guest_port,
                    conn.our_next_seq,
                    conn.guest_next_seq,
                )
            }
            None => return,
        };

        let Ok(tcp_payload) = TcpSegmentBuilder {
            src_port: remote_port,
            dst_port: guest_port,
            seq_number,
            ack_number,
            flags: TcpFlags::ACK | TcpFlags::PSH,
            window_size: 65535,
            urgent_pointer: 0,
            options: &[],
            payload: &data,
        }
        .build_vec(remote_ip, self.cfg.guest_ip) else {
            return;
        };

        // Now that the segment is built, advance the send sequence number.
        {
            let Some(conn) = self.tcp.get_mut(&key) else {
                return;
            };
            conn.our_next_seq = conn.our_next_seq.wrapping_add(data.len() as u32);
        }

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::TCP,
            src_ip: remote_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &tcp_payload,
        }
        .build_vec() else {
            return;
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return;
        };

        out.push(Action::EmitFrame(eth));
    }

    fn emit_tcp_syn_ack(&mut self, conn: &TcpConn) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        let tcp = match TcpSegmentBuilder::syn_ack(
            conn.remote_port,
            conn.guest_port,
            conn.our_isn,
            conn.guest_next_seq,
            65535,
        )
        .build_vec(conn.remote_ip, self.cfg.guest_ip)
        {
            Ok(pkt) => pkt,
            Err(_) => return Vec::new(),
        };

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::TCP,
            src_ip: conn.remote_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &tcp,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn emit_tcp_ack(&mut self, conn: &TcpConn) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        let tcp = match TcpSegmentBuilder::ack(
            conn.remote_port,
            conn.guest_port,
            conn.our_next_seq,
            conn.guest_next_seq,
            65535,
        )
        .build_vec(conn.remote_ip, self.cfg.guest_ip)
        {
            Ok(pkt) => pkt,
            Err(_) => return Vec::new(),
        };

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::TCP,
            src_ip: conn.remote_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &tcp,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn emit_tcp_fin(&mut self, conn: &mut TcpConn) -> Vec<Action> {
        if conn.fin_sent {
            return Vec::new();
        }

        conn.fin_sent = true;
        conn.fin_seq = conn.our_next_seq;
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        let tcp = match TcpSegmentBuilder::fin_ack(
            conn.remote_port,
            conn.guest_port,
            conn.fin_seq,
            conn.guest_next_seq,
            65535,
        )
        .build_vec(conn.remote_ip, self.cfg.guest_ip)
        {
            Ok(pkt) => pkt,
            Err(_) => return Vec::new(),
        };

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::TCP,
            src_ip: conn.remote_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &tcp,
        }
        .build_vec() else {
            return Vec::new();
        };
        conn.our_next_seq = conn.our_next_seq.wrapping_add(1);

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn emit_tcp_rst(&mut self, conn: &TcpConn) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        let tcp = match TcpSegmentBuilder::rst(
            conn.remote_port,
            conn.guest_port,
            conn.our_next_seq,
            conn.guest_next_seq,
            0,
        )
        .build_vec(conn.remote_ip, self.cfg.guest_ip)
        {
            Ok(pkt) => pkt,
            Err(_) => return Vec::new(),
        };

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::TCP,
            src_ip: conn.remote_ip,
            dst_ip: self.cfg.guest_ip,
            options: &[],
            payload: &tcp,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn emit_tcp_rst_for_syn(
        &mut self,
        guest_ip: Ipv4Addr,
        guest_port: u16,
        remote_ip: Ipv4Addr,
        remote_port: u16,
        guest_seq: u32,
    ) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        let tcp =
            match TcpSegmentBuilder::rst(remote_port, guest_port, 0, guest_seq.wrapping_add(1), 0)
                .build_vec(remote_ip, guest_ip)
            {
                Ok(pkt) => pkt,
                Err(_) => return Vec::new(),
            };

        let Ok(ip) = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: self.next_ipv4_ident(),
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::TCP,
            src_ip: remote_ip,
            dst_ip: guest_ip,
            options: &[],
            payload: &tcp,
        }
        .build_vec() else {
            return Vec::new();
        };

        let Ok(eth) = EthernetFrameBuilder {
            dest_mac: guest_mac,
            src_mac: self.cfg.our_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec() else {
            return Vec::new();
        };

        vec![Action::EmitFrame(eth)]
    }

    fn next_ipv4_ident(&mut self) -> u16 {
        let id = self.ipv4_ident;
        self.ipv4_ident = self.ipv4_ident.wrapping_add(1);
        id
    }

    fn allocate_isn(&mut self) -> u32 {
        // Not cryptographic; just needs to avoid obvious collisions in tests and basic operation.
        self.next_tcp_id
            .wrapping_mul(1_000_000)
            .wrapping_add(self.ipv4_ident as u32)
    }

    fn tcp_buffer_would_exceed_limit(&self, conn: &TcpConn, new_bytes: usize) -> bool {
        let max = self.cfg.max_buffered_tcp_bytes_per_conn as usize;
        if max == 0 {
            return new_bytes > 0;
        }
        conn.buffered_to_proxy_bytes
            .saturating_add(new_bytes)
            .saturating_sub(max)
            > 0
    }

    /// Export the stack's dynamic runtime state for inclusion in a VM snapshot.
    ///
    /// This state intentionally excludes static [`StackConfig`] (which should be provided by the
    /// caller when recreating the stack).
    pub fn export_snapshot_state(&self) -> NetworkStackSnapshotState {
        // DNS cache: preserve FIFO order and keep output bounded even if internal bookkeeping is
        // somehow inconsistent.
        let now_ms = self.last_now_ms;
        let max_dns = (self.cfg.max_dns_cache_entries as usize).min(crate::snapshot::MAX_DNS_CACHE_ENTRIES);
        let mut dns_cache = Vec::new();
        if max_dns > 0 {
            let mut seen: HashSet<String> = HashSet::new();
            for name in &self.dns_cache_fifo {
                if dns_cache.len() >= max_dns {
                    break;
                }
                if name.len() > crate::snapshot::MAX_DNS_NAME_BYTES {
                    continue;
                }
                if !seen.insert(name.clone()) {
                    continue;
                }
                let Some(entry) = self.dns_cache.get(name) else {
                    continue;
                };
                if entry.expires_at_ms <= now_ms {
                    continue;
                }
                dns_cache.push(DnsCacheEntrySnapshot {
                    name: name.clone(),
                    addr: entry.addr,
                    expires_at_ms: entry.expires_at_ms,
                });
            }

            // Include any entries that are present in the map but missing from the FIFO list in a
            // deterministic order.
            if dns_cache.len() < max_dns {
                let mut missing: Vec<String> = self
                    .dns_cache
                    .keys()
                    .filter(|k| !seen.contains(*k))
                    .cloned()
                    .collect();
                missing.sort();
                for name in missing {
                    if dns_cache.len() >= max_dns {
                        break;
                    }
                    if name.len() > crate::snapshot::MAX_DNS_NAME_BYTES {
                        continue;
                    }
                    let Some(entry) = self.dns_cache.get(&name) else {
                        continue;
                    };
                    if entry.expires_at_ms <= now_ms {
                        continue;
                    }
                    dns_cache.push(DnsCacheEntrySnapshot {
                        name,
                        addr: entry.addr,
                        expires_at_ms: entry.expires_at_ms,
                    });
                }
            }
        }

        let mut tcp_connections: Vec<TcpConnectionSnapshot> = self
            .tcp
            .values()
            .map(|c| TcpConnectionSnapshot {
                id: c.id,
                guest_port: c.guest_port,
                remote_ip: c.remote_ip,
                remote_port: c.remote_port,
                status: if c.proxy_connected {
                    TcpConnectionStatus::Connected
                } else if c.proxy_reconnecting {
                    TcpConnectionStatus::Reconnecting
                } else {
                    TcpConnectionStatus::Disconnected
                },
            })
            .collect();
        tcp_connections.sort_by_key(|c| c.id);
        if tcp_connections.len() > crate::snapshot::MAX_TCP_CONNECTIONS {
            tcp_connections.truncate(crate::snapshot::MAX_TCP_CONNECTIONS);
        }

        NetworkStackSnapshotState {
            guest_mac: self.guest_mac,
            ip_assigned: self.ip_assigned,
            next_tcp_id: self.next_tcp_id,
            next_dns_id: self.next_dns_id,
            ipv4_ident: self.ipv4_ident,
            last_now_ms: self.last_now_ms,
            dns_cache,
            tcp_connections,
        }
    }

    /// Import stack dynamic runtime state from a previously-exported snapshot.
    ///
    /// Returns any host actions that should be performed immediately after restore (e.g.
    /// best-effort proxy reconnects).
    pub fn import_snapshot_state(
        &mut self,
        state: NetworkStackSnapshotState,
        policy: TcpRestorePolicy,
    ) -> Vec<Action> {
        self.guest_mac = state.guest_mac;
        self.ip_assigned = state.ip_assigned;
        self.next_tcp_id = state.next_tcp_id.max(1);
        self.next_dns_id = state.next_dns_id.max(1);
        self.ipv4_ident = state.ipv4_ident;
        if self.ipv4_ident == 0 {
            self.ipv4_ident = 1;
        }
        self.last_now_ms = state.last_now_ms;
        // Align the post-restore time base on the first call that supplies `now_ms`.
        self.time_offset_ms = 0;
        self.restore_time_anchor_ms = Some(state.last_now_ms);

        // Pending DNS cannot be restored: the host-side DoH/proxy requests are not part of the VM
        // snapshot. The guest will retry.
        self.pending_dns.clear();

        self.dns_cache.clear();
        self.dns_cache_fifo.clear();
        for entry in state.dns_cache {
            self.insert_dns_cache(
                entry.name,
                DnsCacheEntry {
                    addr: entry.addr,
                    expires_at_ms: entry.expires_at_ms,
                },
            );
        }

        // TCP state is policy-controlled.
        self.tcp.clear();
        let mut actions = Vec::new();
        match policy {
            TcpRestorePolicy::Drop => {
                // Drop all active TCP connections.
            }
            TcpRestorePolicy::Reconnect => {
                // Restore only connection bookkeeping and mark as reconnecting.
                let max_tcp = self.cfg.max_tcp_connections as usize;
                if max_tcp == 0 {
                    return actions;
                }
                if !self.cfg.host_policy.enabled {
                    return actions;
                }

                let mut conns = state.tcp_connections;
                conns.sort_by_key(|c| c.id);
                for c in conns {
                    if self.tcp.len() >= max_tcp {
                        break;
                    }
                    if c.id == 0 {
                        continue;
                    }
                    // Respect current host policy (which may differ from the policy at the time the
                    // snapshot was taken).
                    if !self.cfg.host_policy.allows_ip(c.remote_ip) {
                        continue;
                    }
                    let key = TcpKey {
                        guest_port: c.guest_port,
                        remote_ip: c.remote_ip,
                        remote_port: c.remote_port,
                    };
                    if self.tcp.contains_key(&key) {
                        continue;
                    }

                    self.tcp.insert(
                        key,
                        TcpConn {
                            id: c.id,
                            guest_port: c.guest_port,
                            remote_ip: c.remote_ip,
                            remote_port: c.remote_port,
                            // Sequence numbers are resynchronized lazily on the first guest packet.
                            guest_isn: 0,
                            guest_next_seq: 0,
                            our_isn: 0,
                            our_next_seq: 0,
                            syn_acked: true,
                            fin_sent: false,
                            fin_seq: 0,
                            fin_acked: false,
                            guest_fin_received: false,
                            proxy_connected: false,
                            proxy_reconnecting: true,
                            seq_synced: false,
                            buffered_to_proxy: Vec::new(),
                            buffered_to_proxy_bytes: 0,
                        },
                    );

                    actions.push(Action::TcpProxyConnect {
                        connection_id: c.id,
                        remote_ip: c.remote_ip,
                        remote_port: c.remote_port,
                    });

                    // Keep the ID allocator monotonic even if the snapshot is corrupted.
                    self.next_tcp_id = self.next_tcp_id.max(c.id.saturating_add(1));
                }
            }
        }

        actions
    }

    fn insert_dns_cache(&mut self, name: String, entry: DnsCacheEntry) {
        let max = self.cfg.max_dns_cache_entries as usize;
        if max == 0 {
            return;
        }

        let is_new = !self.dns_cache.contains_key(&name);
        self.dns_cache.insert(name.clone(), entry);
        if is_new {
            self.dns_cache_fifo.push_back(name);
        }

        while self.dns_cache.len() > max {
            let Some(evict) = self.dns_cache_fifo.pop_front() else {
                break;
            };
            self.dns_cache.remove(&evict);
        }

        // Keep auxiliary bookkeeping bounded even if it somehow gets out of sync.
        while self.dns_cache_fifo.len() > max {
            self.dns_cache_fifo.pop_front();
        }
    }
}

// Snapshot: `aero-io-snapshot` TLV blob (DEVICE_ID = "NETS", version 1.0).
//
// `NetworkStackSnapshotState` is the serializable dynamic state. We also implement `IoSnapshot` for
// `NetworkStack` as a convenience wrapper that defaults to `TcpRestorePolicy::Drop` (deterministic,
// avoids timing-dependent reconnect behavior).
impl IoSnapshot for NetworkStack {
    const DEVICE_ID: [u8; 4] = <NetworkStackSnapshotState as IoSnapshot>::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = <NetworkStackSnapshotState as IoSnapshot>::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.export_snapshot_state().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let mut state = NetworkStackSnapshotState::default();
        state.load_state(bytes)?;

        // Reset to a deterministic baseline while preserving host configuration.
        let cfg = self.cfg.clone();
        *self = Self::new(cfg);

        let _ = self.import_snapshot_state(state, TcpRestorePolicy::Drop);
        Ok(())
    }
}
