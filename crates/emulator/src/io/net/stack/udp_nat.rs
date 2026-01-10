use std::collections::HashMap;
use std::net::Ipv4Addr;

use super::ethernet::{build_ethernet_frame, EtherType, MacAddr};
use super::ipv4::{checksum_pseudo_header, finalize_checksum, IpProtocol, Ipv4Packet, Ipv4PacketBuilder};
use super::{ProxyAction, ProxyEvent};

#[derive(Debug, Clone, Copy)]
pub struct UdpDatagram<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub checksum: u16,
    pub payload: &'a [u8],
}

impl<'a> UdpDatagram<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < 8 {
            return None;
        }
        let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
        if len < 8 || len > buf.len() {
            return None;
        }
        Some(Self {
            src_port: u16::from_be_bytes([buf[0], buf[1]]),
            dst_port: u16::from_be_bytes([buf[2], buf[3]]),
            checksum: u16::from_be_bytes([buf[6], buf[7]]),
            payload: &buf[8..len],
        })
    }

    pub fn build(src_port: u16, dst_port: u16, payload: &[u8], src_ip: Ipv4Addr, dst_ip: Ipv4Addr) -> Vec<u8> {
        let len = 8 + payload.len();
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(&src_port.to_be_bytes());
        out.extend_from_slice(&dst_port.to_be_bytes());
        out.extend_from_slice(&(len as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
        out.extend_from_slice(payload);

        let mut sum = checksum_pseudo_header(src_ip, dst_ip, IpProtocol::Udp.to_u8(), len as u16);
        for chunk in out.chunks_exact(2) {
            sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
        }
        if out.len() % 2 == 1 {
            sum += (out[out.len() - 1] as u32) << 8;
        }
        let mut csum = finalize_checksum(sum);
        if csum == 0 {
            csum = 0xffff;
        }
        out[6..8].copy_from_slice(&csum.to_be_bytes());
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct UdpFlowKey {
    guest_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
}

#[derive(Debug)]
struct UdpFlow {
    flow_id: u64,
}

#[derive(Debug)]
pub struct UdpNat {
    next_flow_id: u64,
    flows: HashMap<UdpFlowKey, UdpFlow>,
    by_id: HashMap<u64, UdpFlowKey>,
}

impl UdpNat {
    pub fn new() -> Self {
        Self {
            next_flow_id: 1,
            flows: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    pub fn handle_outbound(
        &mut self,
        ip: Ipv4Packet<'_>,
        guest_mac: MacAddr,
        gateway_mac: MacAddr,
        guest_ip: Ipv4Addr,
    ) -> (Vec<Vec<u8>>, Vec<ProxyAction>) {
        let udp = match UdpDatagram::parse(ip.payload) {
            Some(u) => u,
            None => return (Vec::new(), Vec::new()),
        };

        // Only NAT packets originating from the guest IP.
        if ip.src != guest_ip {
            return (Vec::new(), Vec::new());
        }

        let key = UdpFlowKey {
            guest_port: udp.src_port,
            remote_ip: ip.dst,
            remote_port: udp.dst_port,
        };
        let flow_id = if let Some(flow) = self.flows.get(&key) {
            flow.flow_id
        } else {
            let id = self.next_flow_id;
            self.next_flow_id += 1;
            self.flows.insert(key, UdpFlow { flow_id: id });
            self.by_id.insert(id, key);
            id
        };

        let action = ProxyAction::UdpSend {
            flow_id,
            dst_ip: ip.dst,
            dst_port: udp.dst_port,
            data: udp.payload.to_vec(),
        };

        // No immediate response frames for a typical UDP NAT.
        let _ = (guest_mac, gateway_mac);
        (Vec::new(), vec![action])
    }

    pub fn on_proxy_data(
        &mut self,
        flow_id: u64,
        data: &[u8],
        guest_mac: MacAddr,
        gateway_mac: MacAddr,
        guest_ip: Ipv4Addr,
    ) -> Option<Vec<u8>> {
        let key = *self.by_id.get(&flow_id)?;
        let udp = UdpDatagram::build(key.remote_port, key.guest_port, data, key.remote_ip, guest_ip);
        let ip = Ipv4PacketBuilder::new()
            .src(key.remote_ip)
            .dst(guest_ip)
            .protocol(IpProtocol::Udp)
            .payload(udp)
            .build();
        Some(build_ethernet_frame(guest_mac, gateway_mac, EtherType::Ipv4, &ip))
    }

    #[allow(dead_code)]
    pub fn handle_proxy_event(
        &mut self,
        event: ProxyEvent,
        guest_mac: MacAddr,
        gateway_mac: MacAddr,
        guest_ip: Ipv4Addr,
    ) -> Vec<Vec<u8>> {
        match event {
            ProxyEvent::UdpData { flow_id, data } => self
                .on_proxy_data(flow_id, &data, guest_mac, gateway_mac, guest_ip)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }
}
