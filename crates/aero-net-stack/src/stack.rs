#![forbid(unsafe_code)]

use crate::packet::{
    ArpOperation, ArpPacket, DhcpMessage, DhcpMessageType, DnsMessage, DnsResponseCode, DnsType,
    EtherType, EthernetFrame, IcmpEchoPacket, Ipv4Packet, Ipv4Protocol, MacAddr, TcpFlags,
    TcpSegment, UdpDatagram,
};
use crate::policy::HostPolicy;
use core::net::Ipv4Addr;
use std::collections::{HashMap, VecDeque};

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

    pub fn process_outbound_ethernet(&mut self, frame: &[u8], now_ms: Millis) -> Vec<Action> {
        let eth = match EthernetFrame::parse(frame) {
            Ok(eth) => eth,
            Err(_) => return Vec::new(),
        };

        // Learn guest MAC (we need it for replies).
        self.guest_mac.get_or_insert(eth.src);

        match eth.ethertype {
            EtherType::ARP => self.handle_arp(eth.payload),
            EtherType::IPV4 => self.handle_ipv4(eth.payload, now_ms),
            _ => Vec::new(),
        }
    }

    pub fn handle_tcp_proxy_event(&mut self, event: TcpProxyEvent, _now_ms: Millis) -> Vec<Action> {
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
                out.extend(self.emit_tcp_rst(&conn));
                // Drop the connection (guest will see RST or time out).
            }
            TcpProxyEvent::Data { .. } => unreachable!(),
        }

        out
    }

    pub fn handle_udp_proxy_event(&mut self, event: UdpProxyEvent, _now_ms: Millis) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        if !self.ip_assigned {
            return Vec::new();
        }

        let udp = UdpDatagram::serialize(
            event.src_ip,
            self.cfg.guest_ip,
            event.src_port,
            event.dst_port,
            &event.data,
        );
        let ip = Ipv4Packet::serialize(
            event.src_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::UDP,
            self.next_ipv4_ident(),
            64,
            &udp,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
        vec![Action::EmitFrame(eth)]
    }

    pub fn handle_dns_resolved(&mut self, resolved: DnsResolved, now_ms: Millis) -> Vec<Action> {
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

        let (addr_opt, rcode) = match allowed_addr {
            Some(addr) => (Some(addr.octets()), DnsResponseCode::NoError),
            None => (None, DnsResponseCode::NameError),
        };
        let dns_payload = DnsMessage::build_response(
            pending.txid,
            pending.rd,
            &pending.name,
            pending.qtype,
            pending.qclass,
            addr_opt,
            resolved.ttl_secs,
            rcode,
        );

        let udp = UdpDatagram::serialize(
            self.cfg.dns_ip,
            self.cfg.guest_ip,
            53,
            pending.src_port,
            &dns_payload,
        );
        let ip = Ipv4Packet::serialize(
            self.cfg.dns_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::UDP,
            self.next_ipv4_ident(),
            64,
            &udp,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
        vec![Action::EmitFrame(eth)]
    }

    fn handle_arp(&mut self, payload: &[u8]) -> Vec<Action> {
        let arp = match ArpPacket::parse(payload) {
            Ok(a) => a,
            Err(_) => return Vec::new(),
        };

        // Cache guest IP->MAC when known.
        if arp.sender_ip != Ipv4Addr::UNSPECIFIED {
            // Only store if we're already sure about the guest IP (to avoid poisoning ourselves
            // from DHCP's 0.0.0.0 phase).
            if self.ip_assigned && arp.sender_ip == self.cfg.guest_ip {
                self.guest_mac = Some(arp.sender_hw);
            }
        }

        if arp.op != ArpOperation::Request {
            return Vec::new();
        }
        if arp.target_ip != self.cfg.gateway_ip && arp.target_ip != self.cfg.dns_ip {
            return Vec::new();
        }

        let reply = ArpPacket {
            op: ArpOperation::Reply,
            sender_hw: self.cfg.our_mac,
            sender_ip: arp.target_ip,
            target_hw: arp.sender_hw,
            target_ip: arp.sender_ip,
        };
        let eth = EthernetFrame::serialize(
            arp.sender_hw,
            self.cfg.our_mac,
            EtherType::ARP,
            &reply.serialize(),
        );
        vec![Action::EmitFrame(eth)]
    }

    fn handle_ipv4(&mut self, payload: &[u8], now_ms: Millis) -> Vec<Action> {
        let ip = match Ipv4Packet::parse(payload) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        match ip.protocol {
            Ipv4Protocol::UDP => self.handle_udp(ip, now_ms),
            Ipv4Protocol::TCP => self.handle_tcp(ip),
            Ipv4Protocol::ICMP => self.handle_icmp(ip),
            _ => Vec::new(),
        }
    }

    fn handle_udp(&mut self, ip: Ipv4Packet<'_>, now_ms: Millis) -> Vec<Action> {
        let udp = match UdpDatagram::parse(ip.payload) {
            Ok(u) => u,
            Err(_) => return Vec::new(),
        };

        // DHCP (client->server)
        if udp.src_port == 68 && udp.dst_port == 67 {
            return self.handle_dhcp(ip, udp);
        }

        // DNS queries to our advertised DNS IP.
        if udp.dst_port == 53 && ip.dst == self.cfg.dns_ip {
            return self.handle_dns_query(ip, udp, now_ms);
        }

        if !self.ip_assigned {
            return Vec::new();
        }

        // Forward UDP to proxy.
        if !self.cfg.host_policy.enabled || !self.cfg.host_policy.allows_ip(ip.dst) {
            return Vec::new();
        }

        vec![Action::UdpProxySend {
            transport: if self.cfg.webrtc_udp {
                UdpTransport::WebRtc
            } else {
                UdpTransport::Proxy
            },
            src_port: udp.src_port,
            dst_ip: ip.dst,
            dst_port: udp.dst_port,
            data: udp.payload.to_vec(),
        }]
    }

    fn handle_dns_query(
        &mut self,
        _ip: Ipv4Packet<'_>,
        udp: UdpDatagram<'_>,
        now_ms: Millis,
    ) -> Vec<Action> {
        let msg = match DnsMessage::parse_query(udp.payload) {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        let question = match msg.questions.first() {
            Some(q) => q,
            None => return Vec::new(),
        };

        let name = question.name.trim_end_matches('.').to_ascii_lowercase();
        let rd = (msg.flags & (1 << 8)) != 0;
        let qtype = question.qtype;
        let qclass = question.qclass;

        // Enforce domain policy early.
        if !self.cfg.host_policy.enabled || !self.cfg.host_policy.allows_domain(&name) {
            return self.emit_dns_error(
                msg.id,
                &name,
                qtype,
                qclass,
                udp.src_port,
                rd,
                DnsResponseCode::NameError,
            );
        }

        // We only implement A/IN. For AAAA and friends, return NOTIMP so clients can fall back to
        // A queries instead of treating the name as missing.
        if qtype != DnsType::A as u16 || qclass != 1 {
            return self.emit_dns_error(
                msg.id,
                &name,
                qtype,
                qclass,
                udp.src_port,
                rd,
                DnsResponseCode::NotImplemented,
            );
        }

        if let Some(entry) = self.dns_cache.get(&name) {
            if entry.expires_at_ms > now_ms {
                // Policy can change over time; re-check before serving from cache.
                if !self.cfg.host_policy.allows_ip(entry.addr) {
                    return self.emit_dns_error(
                        msg.id,
                        &name,
                        qtype,
                        qclass,
                        udp.src_port,
                        rd,
                        DnsResponseCode::NameError,
                    );
                }
                let guest_mac = match self.guest_mac {
                    Some(m) => m,
                    None => return Vec::new(),
                };
                let dns_payload = DnsMessage::build_response(
                    msg.id,
                    rd,
                    &name,
                    qtype,
                    qclass,
                    Some(entry.addr.octets()),
                    ((entry.expires_at_ms - now_ms) / 1000) as u32,
                    DnsResponseCode::NoError,
                );
                let udp_out = UdpDatagram::serialize(
                    self.cfg.dns_ip,
                    self.cfg.guest_ip,
                    53,
                    udp.src_port,
                    &dns_payload,
                );
                let ip_out = Ipv4Packet::serialize(
                    self.cfg.dns_ip,
                    self.cfg.guest_ip,
                    Ipv4Protocol::UDP,
                    self.next_ipv4_ident(),
                    64,
                    &udp_out,
                );
                let eth =
                    EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip_out);
                return vec![Action::EmitFrame(eth)];
            }
        }

        // Avoid unbounded growth if the guest sends many DNS queries while the host/proxy is slow
        // to resolve them.
        let max_pending_dns = self.cfg.max_pending_dns as usize;
        if max_pending_dns == 0 || self.pending_dns.len() >= max_pending_dns {
            return self.emit_dns_error(
                msg.id,
                &name,
                qtype,
                qclass,
                udp.src_port,
                rd,
                DnsResponseCode::ServerFailure,
            );
        }

        let request_id = self.next_dns_id;
        self.next_dns_id += 1;
        self.pending_dns.insert(
            request_id,
            PendingDns {
                txid: msg.id,
                src_port: udp.src_port,
                name: name.clone(),
                qtype,
                qclass,
                rd,
            },
        );
        vec![Action::DnsResolve { request_id, name }]
    }

    fn emit_dns_error(
        &mut self,
        txid: u16,
        name: &str,
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
        let dns_payload = DnsMessage::build_response(txid, rd, name, qtype, qclass, None, 0, rcode);
        let udp = UdpDatagram::serialize(
            self.cfg.dns_ip,
            self.cfg.guest_ip,
            53,
            dst_port,
            &dns_payload,
        );
        let ip = Ipv4Packet::serialize(
            self.cfg.dns_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::UDP,
            self.next_ipv4_ident(),
            64,
            &udp,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
        vec![Action::EmitFrame(eth)]
    }

    fn handle_dhcp(&mut self, _ip: Ipv4Packet<'_>, udp: UdpDatagram<'_>) -> Vec<Action> {
        let msg = match DhcpMessage::parse(udp.payload) {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        let mtype = match msg.options.message_type {
            Some(t) => t,
            None => return Vec::new(),
        };

        let guest_mac = msg.chaddr;

        let (reply_type, mark_assigned) = match mtype {
            DhcpMessageType::Discover => (DhcpMessageType::Offer, false),
            DhcpMessageType::Request => (DhcpMessageType::Ack, true),
            _ => return Vec::new(),
        };

        let dhcp = DhcpMessage::serialize_offer_or_ack(
            msg.xid,
            msg.flags,
            guest_mac,
            self.cfg.guest_ip,
            self.cfg.gateway_ip,
            self.cfg.netmask,
            self.cfg.gateway_ip,
            self.cfg.dns_ip,
            self.cfg.dhcp_lease_time_secs,
            reply_type,
        );

        if mark_assigned {
            self.ip_assigned = true;
            self.guest_mac = Some(guest_mac);
        }

        let udp_out =
            UdpDatagram::serialize(self.cfg.gateway_ip, Ipv4Addr::BROADCAST, 67, 68, &dhcp);
        let ip_out = Ipv4Packet::serialize(
            self.cfg.gateway_ip,
            Ipv4Addr::BROADCAST,
            Ipv4Protocol::UDP,
            self.next_ipv4_ident(),
            64,
            &udp_out,
        );
        let eth = EthernetFrame::serialize(
            MacAddr::BROADCAST,
            self.cfg.our_mac,
            EtherType::IPV4,
            &ip_out,
        );

        let mut out = vec![Action::EmitFrame(eth)];

        // Some stacks accept only unicast replies once the client MAC is known. Send a second copy
        // directly to the guest MAC/IP when possible.
        if guest_mac != MacAddr::BROADCAST {
            let udp_unicast =
                UdpDatagram::serialize(self.cfg.gateway_ip, self.cfg.guest_ip, 67, 68, &dhcp);
            let ip_unicast = Ipv4Packet::serialize(
                self.cfg.gateway_ip,
                self.cfg.guest_ip,
                Ipv4Protocol::UDP,
                self.next_ipv4_ident(),
                64,
                &udp_unicast,
            );
            let eth_unicast =
                EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip_unicast);
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
        if ip.dst != self.cfg.gateway_ip {
            return Vec::new();
        }
        let pkt = match IcmpEchoPacket::parse(ip.payload) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        if pkt.icmp_type != 8 || pkt.code != 0 {
            return Vec::new();
        }
        let icmp = IcmpEchoPacket::serialize_echo_reply(pkt.identifier, pkt.sequence, pkt.payload);
        let ip_out = Ipv4Packet::serialize(
            self.cfg.gateway_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::ICMP,
            self.next_ipv4_ident(),
            64,
            &icmp,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip_out);
        vec![Action::EmitFrame(eth)]
    }

    fn handle_tcp(&mut self, ip: Ipv4Packet<'_>) -> Vec<Action> {
        let tcp = match TcpSegment::parse(ip.payload) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        if !self.ip_assigned {
            return Vec::new();
        }

        let key = TcpKey {
            guest_port: tcp.src_port,
            remote_ip: ip.dst,
            remote_port: tcp.dst_port,
        };

        if tcp.flags & TcpFlags::RST != 0 {
            if let Some(conn) = self.tcp.remove(&key) {
                return vec![Action::TcpProxyClose {
                    connection_id: conn.id,
                }];
            }
            return Vec::new();
        }

        if !self.tcp.contains_key(&key) {
            // Only start connections when we see SYN.
            if tcp.flags & TcpFlags::SYN == 0 || tcp.flags & TcpFlags::ACK != 0 {
                return Vec::new();
            }

            // Enforce security policy *before* advertising a connection to the guest.
            if !self.cfg.host_policy.enabled || !self.cfg.host_policy.allows_ip(ip.dst) {
                return self.emit_tcp_rst_for_syn(
                    ip.src,
                    tcp.src_port,
                    ip.dst,
                    tcp.dst_port,
                    tcp.seq,
                );
            }

            // Cap concurrent connections to avoid unbounded memory use.
            let max_tcp_connections = self.cfg.max_tcp_connections as usize;
            if max_tcp_connections == 0 || self.tcp.len() >= max_tcp_connections {
                return self.emit_tcp_rst_for_syn(
                    ip.src,
                    tcp.src_port,
                    ip.dst,
                    tcp.dst_port,
                    tcp.seq,
                );
            }

            let guest_isn = tcp.seq;
            let our_isn = self.allocate_isn();
            let conn_id = self.next_tcp_id;
            self.next_tcp_id += 1;

            let conn = TcpConn {
                id: conn_id,
                guest_port: tcp.src_port,
                remote_ip: ip.dst,
                remote_port: tcp.dst_port,
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

        // Retransmitted SYN: resend SYN-ACK for idempotence.
        if (tcp.flags & TcpFlags::SYN != 0) && (tcp.flags & TcpFlags::ACK == 0) && !conn.syn_acked {
            if tcp.seq == conn.guest_isn {
                out.extend(self.emit_tcp_syn_ack(&conn));
            }
        }

        // ACK bookkeeping (handshake + FIN).
        if tcp.flags & TcpFlags::ACK != 0 {
            conn.on_guest_ack(tcp.ack);
        }

        // Payload.
        if !tcp.payload.is_empty() {
            // We intentionally do not implement full TCP reassembly: accept only in-order payload.
            if tcp.seq == conn.guest_next_seq {
                if conn.proxy_connected {
                    conn.guest_next_seq =
                        conn.guest_next_seq.wrapping_add(tcp.payload.len() as u32);
                    out.push(Action::TcpProxySend {
                        connection_id: conn.id,
                        data: tcp.payload.to_vec(),
                    });
                } else {
                    if self.tcp_buffer_would_exceed_limit(&conn, tcp.payload.len()) {
                        out.extend(self.emit_tcp_rst(&conn));
                        out.push(Action::TcpProxyClose {
                            connection_id: conn.id,
                        });
                        return out;
                    }
                    conn.guest_next_seq =
                        conn.guest_next_seq.wrapping_add(tcp.payload.len() as u32);
                    conn.buffered_to_proxy_bytes = conn
                        .buffered_to_proxy_bytes
                        .saturating_add(tcp.payload.len());
                    conn.buffered_to_proxy.push(tcp.payload.to_vec());
                }
                out.extend(self.emit_tcp_ack(&conn));
            } else if tcp.seq.wrapping_add(tcp.payload.len() as u32) <= conn.guest_next_seq {
                // Fully duplicate segment; re-ACK for retransmit tolerance.
                out.extend(self.emit_tcp_ack(&conn));
            } else if tcp.seq < conn.guest_next_seq {
                // Overlapping segment; forward only unseen tail.
                let offset = conn.guest_next_seq.wrapping_sub(tcp.seq) as usize;
                let tail = &tcp.payload[offset..];
                if conn.proxy_connected {
                    conn.guest_next_seq = conn.guest_next_seq.wrapping_add(tail.len() as u32);
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
                    conn.guest_next_seq = conn.guest_next_seq.wrapping_add(tail.len() as u32);
                    conn.buffered_to_proxy_bytes =
                        conn.buffered_to_proxy_bytes.saturating_add(tail.len());
                    conn.buffered_to_proxy.push(tail.to_vec());
                }
                out.extend(self.emit_tcp_ack(&conn));
            } else {
                // Out-of-order: ACK what we have and drop.
                out.extend(self.emit_tcp_ack(&conn));
            }
        }

        // FIN.
        if tcp.flags & TcpFlags::FIN != 0 {
            let fin_seq = tcp.seq.wrapping_add(tcp.payload.len() as u32);
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
        let Some((key, _)) = self
            .tcp
            .iter()
            .find(|(_, c)| c.id == connection_id)
            .map(|(k, c)| (*k, c.clone()))
        else {
            return;
        };
        let conn = self.tcp.get_mut(&key).unwrap();
        if conn.fin_sent || !conn.syn_acked {
            return;
        }

        let tcp_payload = TcpSegment::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            conn.remote_port,
            conn.guest_port,
            conn.our_next_seq,
            conn.guest_next_seq,
            TcpFlags::ACK | TcpFlags::PSH,
            65535,
            &data,
        );
        conn.our_next_seq = conn.our_next_seq.wrapping_add(data.len() as u32);
        let ip = Ipv4Packet::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::TCP,
            self.next_ipv4_ident(),
            64,
            &tcp_payload,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
        out.push(Action::EmitFrame(eth));
    }

    fn emit_tcp_syn_ack(&mut self, conn: &TcpConn) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        let tcp = TcpSegment::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            conn.remote_port,
            conn.guest_port,
            conn.our_isn,
            conn.guest_next_seq,
            TcpFlags::SYN | TcpFlags::ACK,
            65535,
            &[],
        );
        let ip = Ipv4Packet::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::TCP,
            self.next_ipv4_ident(),
            64,
            &tcp,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
        vec![Action::EmitFrame(eth)]
    }

    fn emit_tcp_ack(&mut self, conn: &TcpConn) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        let tcp = TcpSegment::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            conn.remote_port,
            conn.guest_port,
            conn.our_next_seq,
            conn.guest_next_seq,
            TcpFlags::ACK,
            65535,
            &[],
        );
        let ip = Ipv4Packet::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::TCP,
            self.next_ipv4_ident(),
            64,
            &tcp,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
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
        let tcp = TcpSegment::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            conn.remote_port,
            conn.guest_port,
            conn.fin_seq,
            conn.guest_next_seq,
            TcpFlags::FIN | TcpFlags::ACK,
            65535,
            &[],
        );
        let ip = Ipv4Packet::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::TCP,
            self.next_ipv4_ident(),
            64,
            &tcp,
        );
        conn.our_next_seq = conn.our_next_seq.wrapping_add(1);
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
        vec![Action::EmitFrame(eth)]
    }

    fn emit_tcp_rst(&mut self, conn: &TcpConn) -> Vec<Action> {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => return Vec::new(),
        };
        let tcp = TcpSegment::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            conn.remote_port,
            conn.guest_port,
            conn.our_next_seq,
            conn.guest_next_seq,
            TcpFlags::RST | TcpFlags::ACK,
            0,
            &[],
        );
        let ip = Ipv4Packet::serialize(
            conn.remote_ip,
            self.cfg.guest_ip,
            Ipv4Protocol::TCP,
            self.next_ipv4_ident(),
            64,
            &tcp,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
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
        let tcp = TcpSegment::serialize(
            remote_ip,
            guest_ip,
            remote_port,
            guest_port,
            0,
            guest_seq.wrapping_add(1),
            TcpFlags::RST | TcpFlags::ACK,
            0,
            &[],
        );
        let ip = Ipv4Packet::serialize(
            remote_ip,
            guest_ip,
            Ipv4Protocol::TCP,
            self.next_ipv4_ident(),
            64,
            &tcp,
        );
        let eth = EthernetFrame::serialize(guest_mac, self.cfg.our_mac, EtherType::IPV4, &ip);
        vec![Action::EmitFrame(eth)]
    }

    fn next_ipv4_ident(&mut self) -> u16 {
        let id = self.ipv4_ident;
        self.ipv4_ident = self.ipv4_ident.wrapping_add(1);
        id
    }

    fn allocate_isn(&mut self) -> u32 {
        // Not cryptographic; just needs to avoid obvious collisions in tests and basic operation.
        (self.next_tcp_id as u32)
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
