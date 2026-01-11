use std::collections::VecDeque;

use super::dns::DnsUpstream;
use super::{NetConfig, NetworkStack, ProxyAction, ProxyEvent, StackOutput};
use crate::io::net::NetworkBackend;

/// Adapter that turns a [`NetworkStack`] into a NIC-facing backend.
///
/// - Implements [`NetworkBackend`] for use with the emulated E1000 device model.
/// - Can also be used with the virtio-net device model, which consumes the same [`NetworkBackend`]
///   trait for transmitting frames.
///
/// The backend queues:
/// - Ethernet frames that should be injected back into the guest NIC RX path.
/// - Proxy actions (TCP connect/send/close + UDP send) that the host must fulfill.
pub struct NetStackBackend<U: DnsUpstream> {
    stack: NetworkStack<U>,
    pending_frames: VecDeque<Vec<u8>>,
    pending_actions: VecDeque<ProxyAction>,
}

impl<U: DnsUpstream> NetStackBackend<U> {
    pub fn new(cfg: NetConfig, dns_upstream: U) -> Self {
        Self {
            stack: NetworkStack::new(cfg, dns_upstream),
            pending_frames: VecDeque::new(),
            pending_actions: VecDeque::new(),
        }
    }

    pub fn stack(&self) -> &NetworkStack<U> {
        &self.stack
    }

    pub fn stack_mut(&mut self) -> &mut NetworkStack<U> {
        &mut self.stack
    }

    pub fn pop_proxy_action(&mut self) -> Option<ProxyAction> {
        self.pending_actions.pop_front()
    }

    pub fn drain_proxy_actions(&mut self) -> Vec<ProxyAction> {
        self.pending_actions.drain(..).collect()
    }

    pub fn push_proxy_event(&mut self, event: ProxyEvent) {
        let out = self.stack.process_proxy_event(event);
        self.push_output(out);
    }

    fn push_output(&mut self, out: StackOutput) {
        for frame in out.frames_to_guest {
            self.pending_frames.push_back(frame);
        }
        for action in out.proxy_actions {
            self.pending_actions.push_back(action);
        }
    }
}

impl<U: DnsUpstream> NetworkBackend for NetStackBackend<U> {
    fn transmit(&mut self, frame: Vec<u8>) {
        let out = self.stack.process_frame_from_guest(&frame);
        self.push_output(out);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.pending_frames.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::net::stack::arp::{ArpOp, ArpPacket};
    use crate::io::net::stack::dhcp::{DhcpMessage, DhcpMessageType};
    use crate::io::net::stack::dns::{DnsAnswer, DnsUpstream};
    use crate::io::net::stack::ethernet::{build_ethernet_frame, EtherType, EthernetFrame, MacAddr};
    use crate::io::net::stack::ipv4::{IpProtocol, Ipv4Packet, Ipv4PacketBuilder};
    use crate::io::net::stack::udp_nat::UdpDatagram;
    use std::net::Ipv4Addr;

    #[derive(Default)]
    struct NoDns;

    impl DnsUpstream for NoDns {
        fn resolve_a(&mut self, _name: &str) -> Option<DnsAnswer> {
            None
        }
    }

    #[test]
    fn backend_responds_to_arp_request() {
        let cfg = NetConfig::default();
        let guest_mac = MacAddr::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);

        let mut backend = NetStackBackend::new(cfg.clone(), NoDns);

        let req = ArpPacket {
            op: ArpOp::Request,
            sender_mac: guest_mac,
            sender_ip: cfg.guest_ip,
            target_mac: MacAddr::new([0, 0, 0, 0, 0, 0]),
            target_ip: cfg.gateway_ip,
        };

        let frame = build_ethernet_frame(
            MacAddr::BROADCAST,
            guest_mac,
            EtherType::Arp,
            &req.serialize(),
        );
        backend.transmit(frame);

        let reply = backend.poll_receive().expect("ARP reply");
        let eth = EthernetFrame::parse(&reply).expect("ethernet parse");
        assert_eq!(eth.dst, guest_mac);
        assert_eq!(eth.src, cfg.gateway_mac);
        assert_eq!(eth.ethertype, EtherType::Arp);
        let arp = ArpPacket::parse(eth.payload).expect("arp parse");
        assert_eq!(arp.op, ArpOp::Reply);
        assert_eq!(arp.sender_ip, cfg.gateway_ip);
    }

    fn build_dhcp_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
        const MAGIC: [u8; 4] = [99, 130, 83, 99];
        let mut out = vec![0u8; 240];
        out[0] = 1; // BOOTREQUEST
        out[1] = 1; // ethernet
        out[2] = 6; // mac len
        out[4..8].copy_from_slice(&xid.to_be_bytes());
        out[10] = 0x80; // broadcast
        out[28..34].copy_from_slice(&mac.octets());
        out[236..240].copy_from_slice(&MAGIC);
        out.extend_from_slice(&[53, 1, 1, 255]); // DHCP discover + end
        out
    }

    #[test]
    fn backend_emits_dhcp_offer() {
        let cfg = NetConfig::default();
        let guest_mac = MacAddr::new([0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]);
        let mut backend = NetStackBackend::new(cfg.clone(), NoDns);

        let xid = 0x12345678;
        let dhcp = build_dhcp_discover(xid, guest_mac);
        let udp = UdpDatagram::build(68, 67, &dhcp, Ipv4Addr::UNSPECIFIED, Ipv4Addr::new(255, 255, 255, 255));
        let ip = Ipv4PacketBuilder::new()
            .src(Ipv4Addr::UNSPECIFIED)
            .dst(Ipv4Addr::new(255, 255, 255, 255))
            .protocol(IpProtocol::Udp)
            .payload(udp)
            .build();
        let frame = build_ethernet_frame(
            MacAddr::BROADCAST,
            guest_mac,
            EtherType::Ipv4,
            &ip,
        );

        backend.transmit(frame);

        let mut offers = Vec::new();
        while let Some(pkt) = backend.poll_receive() {
            offers.push(pkt);
        }
        assert!(
            !offers.is_empty(),
            "backend should emit at least one DHCP response frame"
        );

        let found_offer = offers.iter().any(|frame| {
            let Some(eth) = EthernetFrame::parse(frame) else {
                return false;
            };
            let Some(ip) = Ipv4Packet::parse(eth.payload) else {
                return false;
            };
            if ip.protocol != IpProtocol::Udp {
                return false;
            }
            let Some(udp) = UdpDatagram::parse(ip.payload) else {
                return false;
            };
            if udp.src_port != 67 || udp.dst_port != 68 {
                return false;
            }
            let Some(msg) = DhcpMessage::parse(udp.payload) else {
                return false;
            };
            msg.xid == xid && msg.message_type == DhcpMessageType::Offer
        });

        assert!(found_offer, "expected DHCP OFFER in backend output");
    }
}
