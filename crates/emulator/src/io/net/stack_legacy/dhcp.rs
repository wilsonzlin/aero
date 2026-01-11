use std::net::Ipv4Addr;

use super::ethernet::MacAddr;
use super::NetConfig;

const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DhcpMessageType {
    Discover = 1,
    Offer = 2,
    Request = 3,
    Decline = 4,
    Ack = 5,
    Nak = 6,
    Release = 7,
    Inform = 8,
}

#[derive(Debug, Clone)]
pub struct DhcpMessage {
    pub xid: u32,
    pub flags: u16,
    pub client_mac: MacAddr,
    pub requested_ip: Option<Ipv4Addr>,
    pub server_identifier: Option<Ipv4Addr>,
    pub message_type: DhcpMessageType,
}

impl DhcpMessage {
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < 240 {
            return None;
        }
        // BOOTREQUEST (1) from the guest or BOOTREPLY (2) from the server.
        if buf[0] != 1 && buf[0] != 2 {
            return None;
        }
        if buf[1] != 1 || buf[2] != 6 {
            return None;
        }
        let xid = u32::from_be_bytes(buf[4..8].try_into().ok()?);
        let flags = u16::from_be_bytes(buf[10..12].try_into().ok()?);
        let client_mac = MacAddr::new(buf[28..34].try_into().ok()?);
        if buf[236..240] != DHCP_MAGIC_COOKIE {
            return None;
        }

        let mut msg_type = None;
        let mut requested_ip = None;
        let mut server_id = None;

        let mut idx = 240;
        while idx < buf.len() {
            let code = buf[idx];
            idx += 1;
            match code {
                0 => continue,       // pad
                255 => break,        // end
                _ => {
                    if idx >= buf.len() {
                        break;
                    }
                    let len = buf[idx] as usize;
                    idx += 1;
                    if idx + len > buf.len() {
                        break;
                    }
                    let data = &buf[idx..idx + len];
                    idx += len;
                    match code {
                        53 if len == 1 => {
                            msg_type = Some(match data[0] {
                                1 => DhcpMessageType::Discover,
                                2 => DhcpMessageType::Offer,
                                3 => DhcpMessageType::Request,
                                4 => DhcpMessageType::Decline,
                                5 => DhcpMessageType::Ack,
                                6 => DhcpMessageType::Nak,
                                7 => DhcpMessageType::Release,
                                8 => DhcpMessageType::Inform,
                                _ => return None,
                            });
                        }
                        50 if len == 4 => {
                            requested_ip = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
                        }
                        54 if len == 4 => {
                            server_id = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
                        }
                        _ => {}
                    }
                }
            }
        }

        Some(Self {
            xid,
            flags,
            client_mac,
            requested_ip,
            server_identifier: server_id,
            message_type: msg_type?,
        })
    }
}

#[derive(Debug)]
pub struct DhcpServer {
    cfg: NetConfig,
}

impl DhcpServer {
    pub fn new(cfg: NetConfig) -> Self {
        Self { cfg }
    }

    pub fn handle_message(&mut self, buf: &[u8], src_mac: MacAddr) -> Option<Vec<u8>> {
        let msg = DhcpMessage::parse(buf)?;
        // Only accept requests from the guest.
        if buf.get(0).copied() != Some(1) {
            return None;
        }
        // Source MAC mismatch is usually benign (Ethernet frame vs chaddr), but use it as a hint
        // for stability in tests.
        let _ = src_mac;

        match msg.message_type {
            DhcpMessageType::Discover => Some(self.build_offer(msg.xid, msg.flags, msg.client_mac)),
            DhcpMessageType::Request => {
                // Only ACK requests for the IP we're willing to lease.
                if let Some(req_ip) = msg.requested_ip {
                    if req_ip != self.cfg.guest_ip {
                        return None;
                    }
                }
                Some(self.build_ack(msg.xid, msg.flags, msg.client_mac))
            }
            _ => None,
        }
    }

    fn build_offer(&self, xid: u32, flags: u16, client_mac: MacAddr) -> Vec<u8> {
        self.build_reply(xid, flags, client_mac, DhcpMessageType::Offer)
    }

    fn build_ack(&self, xid: u32, flags: u16, client_mac: MacAddr) -> Vec<u8> {
        self.build_reply(xid, flags, client_mac, DhcpMessageType::Ack)
    }

    fn build_reply(&self, xid: u32, flags: u16, client_mac: MacAddr, ty: DhcpMessageType) -> Vec<u8> {
        // BOOTP fixed header is 236 bytes.
        let mut out = vec![0u8; 240];
        out[0] = 2; // BOOTREPLY
        out[1] = 1; // Ethernet
        out[2] = 6; // MAC length
        out[3] = 0; // hops
        out[4..8].copy_from_slice(&xid.to_be_bytes());
        out[10..12].copy_from_slice(&flags.to_be_bytes());

        // yiaddr (your IP).
        out[16..20].copy_from_slice(&self.cfg.guest_ip.octets());
        // siaddr (server IP).
        out[20..24].copy_from_slice(&self.cfg.gateway_ip.octets());
        // chaddr
        out[28..34].copy_from_slice(&client_mac.octets());

        out[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);

        let mut opts = Vec::with_capacity(64);
        opts.extend_from_slice(&[53, 1, ty as u8]); // message type
        opts.extend_from_slice(&[54, 4]);
        opts.extend_from_slice(&self.cfg.gateway_ip.octets()); // server identifier

        opts.extend_from_slice(&[51, 4]);
        opts.extend_from_slice(&self.cfg.dhcp_lease_seconds.to_be_bytes()); // lease

        // T1/T2 (renew/rebind), best-effort.
        let t1 = self.cfg.dhcp_lease_seconds / 2;
        let t2 = self.cfg.dhcp_lease_seconds * 7 / 8;
        opts.extend_from_slice(&[58, 4]);
        opts.extend_from_slice(&t1.to_be_bytes());
        opts.extend_from_slice(&[59, 4]);
        opts.extend_from_slice(&t2.to_be_bytes());

        opts.extend_from_slice(&[1, 4]);
        opts.extend_from_slice(&self.cfg.netmask.octets()); // subnet mask

        opts.extend_from_slice(&[3, 4]);
        opts.extend_from_slice(&self.cfg.gateway_ip.octets()); // router

        opts.extend_from_slice(&[6, 4]);
        opts.extend_from_slice(&self.cfg.dns_ip.octets()); // DNS

        opts.push(255); // end

        out.extend_from_slice(&opts);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
        let mut out = vec![0u8; 240];
        out[0] = 1;
        out[1] = 1;
        out[2] = 6;
        out[4..8].copy_from_slice(&xid.to_be_bytes());
        out[10] = 0x80; // broadcast
        out[28..34].copy_from_slice(&mac.octets());
        out[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);
        out.extend_from_slice(&[53, 1, 1, 255]);
        out
    }

    fn build_request(xid: u32, mac: MacAddr, requested: Ipv4Addr, server: Ipv4Addr) -> Vec<u8> {
        let mut out = vec![0u8; 240];
        out[0] = 1;
        out[1] = 1;
        out[2] = 6;
        out[4..8].copy_from_slice(&xid.to_be_bytes());
        out[10] = 0x80;
        out[28..34].copy_from_slice(&mac.octets());
        out[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);
        out.extend_from_slice(&[53, 1, 3]); // request
        out.extend_from_slice(&[50, 4]);
        out.extend_from_slice(&requested.octets());
        out.extend_from_slice(&[54, 4]);
        out.extend_from_slice(&server.octets());
        out.push(255);
        out
    }

    #[test]
    fn dhcp_discover_offer_request_ack() {
        let cfg = NetConfig::default();
        let mut server = DhcpServer::new(cfg.clone());

        let mac = MacAddr::new([0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]);
        let xid = 0x12345678;

        let offer = server
            .handle_message(&build_discover(xid, mac), mac)
            .expect("offer");
        let offer_msg = DhcpMessage::parse(&offer).unwrap_or_else(|| panic!("invalid offer: {offer:?}"));
        assert_eq!(offer_msg.xid, xid);
        assert_eq!(offer_msg.client_mac, mac);
        assert_eq!(offer_msg.message_type, DhcpMessageType::Offer);

        let ack = server
            .handle_message(&build_request(xid, mac, cfg.guest_ip, cfg.gateway_ip), mac)
            .expect("ack");
        let ack_msg = DhcpMessage::parse(&ack).unwrap_or_else(|| panic!("invalid ack: {ack:?}"));
        assert_eq!(ack_msg.message_type, DhcpMessageType::Ack);
    }
}
