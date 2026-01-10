#![forbid(unsafe_code)]

use super::{MacAddr, ParseError};
use core::net::Ipv4Addr;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DhcpOption {
    MessageType(DhcpMessageType),
    RequestedIp(Ipv4Addr),
    ServerIdentifier(Ipv4Addr),
    ParameterRequestList(Vec<u8>),
    Unknown { code: u8, data: Vec<u8> },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DhcpOptions {
    pub message_type: Option<DhcpMessageType>,
    pub requested_ip: Option<Ipv4Addr>,
    pub server_identifier: Option<Ipv4Addr>,
    pub parameter_request_list: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpMessage {
    pub op: u8,
    pub htype: u8,
    pub hlen: u8,
    pub hops: u8,
    pub xid: u32,
    pub secs: u16,
    pub flags: u16,
    pub ciaddr: Ipv4Addr,
    pub yiaddr: Ipv4Addr,
    pub siaddr: Ipv4Addr,
    pub giaddr: Ipv4Addr,
    pub chaddr: MacAddr,
    pub options: DhcpOptions,
}

impl DhcpMessage {
    pub fn parse(buf: &[u8]) -> Result<Self, ParseError> {
        if buf.len() < 240 {
            return Err(ParseError::Truncated);
        }

        let op = buf[0];
        let htype = buf[1];
        let hlen = buf[2];
        let hops = buf[3];
        let xid = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let secs = u16::from_be_bytes([buf[8], buf[9]]);
        let flags = u16::from_be_bytes([buf[10], buf[11]]);
        let ciaddr = Ipv4Addr::new(buf[12], buf[13], buf[14], buf[15]);
        let yiaddr = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);
        let siaddr = Ipv4Addr::new(buf[20], buf[21], buf[22], buf[23]);
        let giaddr = Ipv4Addr::new(buf[24], buf[25], buf[26], buf[27]);
        let chaddr = MacAddr(buf[28..34].try_into().unwrap());

        let cookie = &buf[236..240];
        if cookie != [99, 130, 83, 99] {
            return Err(ParseError::Invalid("missing DHCP magic cookie"));
        }

        let mut options = DhcpOptions::default();
        let mut offset = 240usize;
        while offset < buf.len() {
            let code = buf[offset];
            offset += 1;
            match code {
                0 => continue, // Pad
                255 => break,  // End
                _ => {
                    if offset >= buf.len() {
                        return Err(ParseError::Truncated);
                    }
                    let len = buf[offset] as usize;
                    offset += 1;
                    if offset + len > buf.len() {
                        return Err(ParseError::Truncated);
                    }
                    let data = &buf[offset..offset + len];
                    offset += len;
                    match code {
                        53 if len == 1 => {
                            options.message_type = Some(match data[0] {
                                1 => DhcpMessageType::Discover,
                                2 => DhcpMessageType::Offer,
                                3 => DhcpMessageType::Request,
                                4 => DhcpMessageType::Decline,
                                5 => DhcpMessageType::Ack,
                                6 => DhcpMessageType::Nak,
                                7 => DhcpMessageType::Release,
                                8 => DhcpMessageType::Inform,
                                _ => return Err(ParseError::Invalid("unknown DHCP msg type")),
                            });
                        }
                        50 if len == 4 => {
                            options.requested_ip =
                                Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
                        }
                        54 if len == 4 => {
                            options.server_identifier =
                                Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
                        }
                        55 => {
                            options.parameter_request_list = data.to_vec();
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(Self {
            op,
            htype,
            hlen,
            hops,
            xid,
            secs,
            flags,
            ciaddr,
            yiaddr,
            siaddr,
            giaddr,
            chaddr,
            options,
        })
    }

    pub fn serialize_offer_or_ack(
        xid: u32,
        flags: u16,
        client_mac: MacAddr,
        your_ip: Ipv4Addr,
        server_ip: Ipv4Addr,
        subnet_mask: Ipv4Addr,
        router: Ipv4Addr,
        dns_server: Ipv4Addr,
        lease_time_secs: u32,
        msg_type: DhcpMessageType,
    ) -> Vec<u8> {
        let mut out = vec![0u8; 240];
        out[0] = 2; // BOOTREPLY
        out[1] = 1; // Ethernet
        out[2] = 6; // MAC length
        out[3] = 0;
        out[4..8].copy_from_slice(&xid.to_be_bytes());
        out[8..10].copy_from_slice(&0u16.to_be_bytes());
        out[10..12].copy_from_slice(&flags.to_be_bytes());
        // ciaddr = 0
        out[16..20].copy_from_slice(&your_ip.octets());
        out[20..24].copy_from_slice(&server_ip.octets());
        // giaddr = 0
        out[28..34].copy_from_slice(&client_mac.0);
        out[236..240].copy_from_slice(&[99, 130, 83, 99]); // magic cookie

        let mut opts = Vec::new();
        // DHCP message type
        opts.extend_from_slice(&[53, 1, msg_type as u8]);
        // Server identifier
        opts.extend_from_slice(&[54, 4]);
        opts.extend_from_slice(&server_ip.octets());
        // Lease time
        opts.extend_from_slice(&[51, 4]);
        opts.extend_from_slice(&lease_time_secs.to_be_bytes());
        // Subnet mask
        opts.extend_from_slice(&[1, 4]);
        opts.extend_from_slice(&subnet_mask.octets());
        // Router
        opts.extend_from_slice(&[3, 4]);
        opts.extend_from_slice(&router.octets());
        // DNS server
        opts.extend_from_slice(&[6, 4]);
        opts.extend_from_slice(&dns_server.octets());
        // End
        opts.push(255);

        out.extend_from_slice(&opts);
        out
    }
}
