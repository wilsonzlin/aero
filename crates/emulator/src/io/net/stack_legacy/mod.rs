//! In-emulator user-space network stack.
//!
//! The stack is designed to sit behind an emulated NIC (E1000/virtio-net). It receives Ethernet
//! frames from the guest and emits Ethernet frames back to the guest plus "proxy actions" that the
//! host can fulfill (WebSocket TCP proxy / WebRTC UDP proxy / DoH).

use std::collections::VecDeque;
use std::net::Ipv4Addr;

use super::NetworkBackend;

pub mod arp;
pub mod backend;
pub mod dhcp;
pub mod dns;
pub mod ethernet;
pub mod ipv4;
pub mod tcp_nat;
pub mod udp_nat;

use arp::ArpResponder;
use dhcp::DhcpServer;
use dns::{DnsServer, DnsUpstream};
use ethernet::{EtherType, EthernetFrame, MacAddr};
use ipv4::{IcmpPacket, Ipv4Packet};
use tcp_nat::TcpNat;
use udp_nat::{UdpDatagram, UdpNat};

#[allow(unused_macros)]
#[cfg(feature = "net_log")]
macro_rules! net_log {
    ($($t:tt)*) => {
        eprintln!($($t)*);
    };
}

#[allow(unused_macros)]
#[cfg(not(feature = "net_log"))]
macro_rules! net_log {
    ($($t:tt)*) => {};
}

#[derive(Debug, Clone)]
pub struct NetConfig {
    pub gateway_mac: MacAddr,
    pub gateway_ip: Ipv4Addr,
    pub guest_ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub dns_ip: Ipv4Addr,
    pub dhcp_lease_seconds: u32,
    pub promiscuous: bool,
}

impl Default for NetConfig {
    fn default() -> Self {
        // Match common "slirp" defaults, but keep the MAC stable for repeatability in tests.
        let gateway_ip = Ipv4Addr::new(10, 0, 2, 2);
        Self {
            gateway_mac: MacAddr::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
            gateway_ip,
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            dns_ip: gateway_ip,
            dhcp_lease_seconds: 86_400,
            promiscuous: false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NetCounters {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tcp_connections_opened: u64,
    pub tcp_connections_closed: u64,
    pub dns_cache_hits: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyAction {
    TcpConnect {
        conn_id: u64,
        dst_ip: Ipv4Addr,
        dst_port: u16,
    },
    TcpSend {
        conn_id: u64,
        data: Vec<u8>,
    },
    TcpClose {
        conn_id: u64,
    },
    UdpSend {
        flow_id: u64,
        dst_ip: Ipv4Addr,
        dst_port: u16,
        data: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyEvent {
    TcpConnected { conn_id: u64 },
    TcpConnectFailed { conn_id: u64 },
    TcpData { conn_id: u64, data: Vec<u8> },
    TcpClosed { conn_id: u64 },

    UdpData { flow_id: u64, data: Vec<u8> },
}

#[derive(Debug, Default)]
pub struct StackOutput {
    pub frames_to_guest: Vec<Vec<u8>>,
    pub proxy_actions: Vec<ProxyAction>,
}

impl StackOutput {
    fn push_frame(&mut self, frame: Vec<u8>, counters: &mut NetCounters) {
        counters.tx_packets += 1;
        counters.tx_bytes += frame.len() as u64;
        self.frames_to_guest.push(frame);
    }
}

pub struct NetworkStack<U: DnsUpstream> {
    cfg: NetConfig,
    counters: NetCounters,

    guest_mac: Option<MacAddr>,
    arp: ArpResponder,
    dhcp: DhcpServer,
    dns: DnsServer<U>,
    tcp: TcpNat,
    udp: UdpNat,

    // Simple queue so that per-frame handlers can enqueue additional frames/actions without
    // needing mutable borrows of `StackOutput`.
    pending_frames: VecDeque<Vec<u8>>,
    pending_actions: VecDeque<ProxyAction>,
}

impl<U: DnsUpstream> NetworkStack<U> {
    pub fn new(cfg: NetConfig, dns_upstream: U) -> Self {
        let arp = ArpResponder::new(cfg.gateway_mac, cfg.gateway_ip);
        let dhcp = DhcpServer::new(cfg.clone());
        let dns = DnsServer::new(cfg.gateway_ip, dns_upstream);
        Self {
            cfg,
            counters: NetCounters::default(),
            guest_mac: None,
            arp,
            dhcp,
            dns,
            tcp: TcpNat::new(),
            udp: UdpNat::new(),
            pending_frames: VecDeque::new(),
            pending_actions: VecDeque::new(),
        }
    }

    pub fn config(&self) -> &NetConfig {
        &self.cfg
    }

    pub fn counters(&self) -> NetCounters {
        self.counters
    }

    pub fn process_frame_from_guest(&mut self, frame: &[u8]) -> StackOutput {
        self.counters.rx_packets += 1;
        self.counters.rx_bytes += frame.len() as u64;

        let eth = match EthernetFrame::parse(frame) {
            Some(f) => f,
            None => return StackOutput::default(),
        };

        self.guest_mac = Some(eth.src);

        if !self.cfg.promiscuous && eth.dst != self.cfg.gateway_mac && !eth.dst.is_broadcast() {
            return StackOutput::default();
        }

        match eth.ethertype {
            EtherType::Arp => self.handle_arp(eth),
            EtherType::Ipv4 => self.handle_ipv4(eth),
            _ => StackOutput::default(),
        }
    }

    pub fn process_proxy_event(&mut self, event: ProxyEvent) -> StackOutput {
        match event {
            ProxyEvent::TcpConnected { conn_id } => {
                self.tcp.on_proxy_connected(conn_id);
            }
            ProxyEvent::TcpConnectFailed { conn_id } => {
                let (frame, closed) =
                    self.tcp
                        .on_proxy_connect_failed(conn_id, self.cfg.gateway_mac, self.guest_mac);
                if let Some(frame) = frame {
                    self.pending_frames.push_back(frame);
                }
                self.counters.tcp_connections_closed += closed;
            }
            ProxyEvent::TcpData { conn_id, data } => {
                if let Some(guest_mac) = self.guest_mac {
                    for frame in
                        self.tcp
                            .on_proxy_data(conn_id, &data, guest_mac, self.cfg.gateway_mac)
                    {
                        self.pending_frames.push_back(frame);
                    }
                }
            }
            ProxyEvent::TcpClosed { conn_id } => {
                if let Some(guest_mac) = self.guest_mac {
                    let (frames, closed) =
                        self.tcp
                            .on_proxy_closed(conn_id, guest_mac, self.cfg.gateway_mac);
                    for frame in frames {
                        self.pending_frames.push_back(frame);
                    }
                    self.counters.tcp_connections_closed += closed;
                }
            }
            ProxyEvent::UdpData { flow_id, data } => {
                if let Some(guest_mac) = self.guest_mac {
                    if let Some(frame) = self.udp.on_proxy_data(
                        flow_id,
                        &data,
                        guest_mac,
                        self.cfg.gateway_mac,
                        self.cfg.guest_ip,
                    ) {
                        self.pending_frames.push_back(frame);
                    }
                }
            }
        }

        self.drain_pending()
    }

    fn handle_arp(&mut self, eth: EthernetFrame<'_>) -> StackOutput {
        if let Some(reply) = self.arp.handle(&eth) {
            self.pending_frames.push_back(reply);
        }
        self.drain_pending()
    }

    fn handle_ipv4(&mut self, eth: EthernetFrame<'_>) -> StackOutput {
        let ip = match Ipv4Packet::parse(eth.payload) {
            Some(p) => p,
            None => return StackOutput::default(),
        };

        // Drop fragments for now.
        if ip.is_fragmented() {
            return StackOutput::default();
        }

        // Packets destined to our virtual gateway.
        if ip.dst == self.cfg.gateway_ip || ip.dst == Ipv4Addr::new(255, 255, 255, 255) {
            match ip.protocol {
                ipv4::IpProtocol::Udp => self.handle_udp_to_gateway(eth.src, ip),
                ipv4::IpProtocol::Icmp => self.handle_icmp_to_gateway(ip),
                _ => StackOutput::default(),
            }
        } else {
            // NAT to host.
            match ip.protocol {
                ipv4::IpProtocol::Tcp => self.handle_tcp_nat(eth.src, ip),
                ipv4::IpProtocol::Udp => self.handle_udp_nat(eth.src, ip),
                _ => StackOutput::default(),
            }
        }
    }

    fn handle_icmp_to_gateway(&mut self, ip: Ipv4Packet<'_>) -> StackOutput {
        let icmp = match IcmpPacket::parse(ip.payload) {
            Some(p) => p,
            None => return StackOutput::default(),
        };
        if icmp.is_echo_request() && ip.dst == self.cfg.gateway_ip {
            if let Some(guest_mac) = self.guest_mac {
                let reply = icmp.build_echo_reply();
                let ip_reply = ipv4::Ipv4PacketBuilder::new()
                    .src(self.cfg.gateway_ip)
                    .dst(ip.src)
                    .protocol(ipv4::IpProtocol::Icmp)
                    .payload(reply)
                    .build();
                let frame = ethernet::build_ethernet_frame(
                    guest_mac,
                    self.cfg.gateway_mac,
                    EtherType::Ipv4,
                    &ip_reply,
                );
                self.pending_frames.push_back(frame);
            }
        }
        self.drain_pending()
    }

    fn handle_udp_to_gateway(&mut self, src_mac: MacAddr, ip: Ipv4Packet<'_>) -> StackOutput {
        let udp = match UdpDatagram::parse(ip.payload) {
            Some(u) => u,
            None => return StackOutput::default(),
        };

        // DHCP: client 68 -> server 67 (usually broadcast).
        if udp.dst_port == 67 && udp.src_port == 68 {
            if let Some(dhcp_reply) = self.dhcp.handle_message(udp.payload, src_mac) {
                if let Some(guest_mac) = self.guest_mac {
                    let udp_reply = UdpDatagram::build(
                        67,
                        68,
                        &dhcp_reply,
                        self.cfg.gateway_ip,
                        Ipv4Addr::new(255, 255, 255, 255),
                    );
                    let ip_reply = ipv4::Ipv4PacketBuilder::new()
                        .src(self.cfg.gateway_ip)
                        .dst(Ipv4Addr::new(255, 255, 255, 255))
                        .protocol(ipv4::IpProtocol::Udp)
                        .payload(udp_reply)
                        .build();
                    let frame = ethernet::build_ethernet_frame(
                        MacAddr::BROADCAST,
                        self.cfg.gateway_mac,
                        EtherType::Ipv4,
                        &ip_reply,
                    );
                    // Also send unicast to the guest if we know it; some stacks accept only unicast.
                    self.pending_frames.push_back(frame);
                    if guest_mac != MacAddr::BROADCAST {
                        let frame2 = ethernet::build_ethernet_frame(
                            guest_mac,
                            self.cfg.gateway_mac,
                            EtherType::Ipv4,
                            &ip_reply,
                        );
                        self.pending_frames.push_back(frame2);
                    }
                }
            }
            return self.drain_pending();
        }

        // DNS to gateway.
        if udp.dst_port == 53 && ip.dst == self.cfg.gateway_ip {
            let (resp, cache_hit) = self.dns.handle_query(udp.payload);
            if cache_hit {
                self.counters.dns_cache_hits += 1;
            }
            if let Some(resp) = resp {
                if let Some(guest_mac) = self.guest_mac {
                    let udp_reply =
                        UdpDatagram::build(53, udp.src_port, &resp, self.cfg.gateway_ip, ip.src);
                    let ip_reply = ipv4::Ipv4PacketBuilder::new()
                        .src(self.cfg.gateway_ip)
                        .dst(ip.src)
                        .protocol(ipv4::IpProtocol::Udp)
                        .payload(udp_reply)
                        .build();
                    let frame = ethernet::build_ethernet_frame(
                        guest_mac,
                        self.cfg.gateway_mac,
                        EtherType::Ipv4,
                        &ip_reply,
                    );
                    self.pending_frames.push_back(frame);
                }
            }
            return self.drain_pending();
        }

        StackOutput::default()
    }

    fn handle_tcp_nat(&mut self, src_mac: MacAddr, ip: Ipv4Packet<'_>) -> StackOutput {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => src_mac,
        };

        let (frames, actions, opened, closed) =
            self.tcp
                .handle_outbound(ip, guest_mac, self.cfg.gateway_mac, self.cfg.guest_ip);
        self.counters.tcp_connections_opened += opened;
        self.counters.tcp_connections_closed += closed;
        for frame in frames {
            self.pending_frames.push_back(frame);
        }
        for action in actions {
            self.pending_actions.push_back(action);
        }
        self.drain_pending()
    }

    fn handle_udp_nat(&mut self, src_mac: MacAddr, ip: Ipv4Packet<'_>) -> StackOutput {
        let guest_mac = match self.guest_mac {
            Some(m) => m,
            None => src_mac,
        };
        let (frames, actions) =
            self.udp
                .handle_outbound(ip, guest_mac, self.cfg.gateway_mac, self.cfg.guest_ip);
        for frame in frames {
            self.pending_frames.push_back(frame);
        }
        for action in actions {
            self.pending_actions.push_back(action);
        }
        self.drain_pending()
    }

    fn drain_pending(&mut self) -> StackOutput {
        let mut out = StackOutput::default();
        while let Some(frame) = self.pending_frames.pop_front() {
            out.push_frame(frame, &mut self.counters);
        }
        while let Some(action) = self.pending_actions.pop_front() {
            out.proxy_actions.push(action);
        }
        out
    }
}

/// Adapter that lets the in-emulator [`NetworkStack`] act as a NIC [`NetworkBackend`].
///
/// The backend collects `frames_to_guest` and `proxy_actions` produced by the stack so that the
/// emulator can forward them to the emulated NIC RX path and host-side proxy implementation.
pub struct StackBackend<U: DnsUpstream> {
    stack: NetworkStack<U>,
    frames_to_guest: VecDeque<Vec<u8>>,
    proxy_actions: VecDeque<ProxyAction>,
}

impl<U: DnsUpstream> StackBackend<U> {
    pub fn new(stack: NetworkStack<U>) -> Self {
        Self {
            stack,
            frames_to_guest: VecDeque::new(),
            proxy_actions: VecDeque::new(),
        }
    }

    pub fn stack(&self) -> &NetworkStack<U> {
        &self.stack
    }

    pub fn stack_mut(&mut self) -> &mut NetworkStack<U> {
        &mut self.stack
    }

    pub fn process_proxy_event(&mut self, event: ProxyEvent) {
        let out = self.stack.process_proxy_event(event);
        self.push_output(out);
    }

    pub fn drain_frames_to_guest(&mut self) -> Vec<Vec<u8>> {
        self.frames_to_guest.drain(..).collect()
    }

    pub fn drain_proxy_actions(&mut self) -> Vec<ProxyAction> {
        self.proxy_actions.drain(..).collect()
    }

    fn push_output(&mut self, out: StackOutput) {
        self.frames_to_guest.extend(out.frames_to_guest);
        self.proxy_actions.extend(out.proxy_actions);
    }
}

impl<U: DnsUpstream> NetworkBackend for StackBackend<U> {
    fn transmit(&mut self, frame: Vec<u8>) {
        let out = self.stack.process_frame_from_guest(&frame);
        self.push_output(out);
    }
}

#[cfg(test)]
mod backend_tests {
    use super::arp::{ArpOp, ArpPacket};
    use super::dns::{DnsAnswer, DnsUpstream};
    use super::ethernet::{build_ethernet_frame, EtherType, EthernetFrame, MacAddr};
    use super::*;

    struct NoDns;

    impl DnsUpstream for NoDns {
        fn resolve_a(&mut self, _name: &str) -> Option<DnsAnswer> {
            None
        }
    }

    #[test]
    fn stack_backend_transmit_queues_frames_to_guest() {
        let cfg = NetConfig::default();
        let guest_mac = MacAddr::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);

        let stack = NetworkStack::new(cfg.clone(), NoDns);
        let mut backend = StackBackend::new(stack);

        let arp = ArpPacket {
            op: ArpOp::Request,
            sender_mac: guest_mac,
            sender_ip: cfg.guest_ip,
            target_mac: MacAddr::new([0; 6]),
            target_ip: cfg.gateway_ip,
        };
        let frame = build_ethernet_frame(
            MacAddr::BROADCAST,
            guest_mac,
            EtherType::Arp,
            &arp.serialize(),
        );

        backend.transmit(frame);

        let frames = backend.drain_frames_to_guest();
        assert_eq!(frames.len(), 1);
        assert!(backend.drain_proxy_actions().is_empty());

        let eth = EthernetFrame::parse(&frames[0]).unwrap();
        assert_eq!(eth.dst, guest_mac);
        assert_eq!(eth.src, cfg.gateway_mac);
        assert_eq!(eth.ethertype, EtherType::Arp);
    }
}
