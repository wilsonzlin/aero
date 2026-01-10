//! Minimal DHCP (BOOTP) message builder.
//!
//! DHCP is technically above UDP, but the emulator network stack often needs to
//! craft simple OFFER/ACK responses. This module intentionally implements only
//! the subset we need for that.

use core::net::Ipv4Addr;

use super::{ensure_out_buf_len, MacAddr, PacketError};

pub const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

pub const DHCP_OP_BOOTREQUEST: u8 = 1;
pub const DHCP_OP_BOOTREPLY: u8 = 2;

pub const DHCP_OPT_MESSAGE_TYPE: u8 = 53;
pub const DHCP_OPT_REQUESTED_IP: u8 = 50;
pub const DHCP_OPT_SERVER_IDENTIFIER: u8 = 54;
pub const DHCP_OPT_SUBNET_MASK: u8 = 1;
pub const DHCP_OPT_ROUTER: u8 = 3;
pub const DHCP_OPT_DNS: u8 = 6;
pub const DHCP_OPT_IP_LEASE_TIME: u8 = 51;
pub const DHCP_OPT_RENEWAL_TIME: u8 = 58;
pub const DHCP_OPT_REBINDING_TIME: u8 = 59;
pub const DHCP_OPT_END: u8 = 255;

pub const DHCP_MSG_DISCOVER: u8 = 1;
pub const DHCP_MSG_OFFER: u8 = 2;
pub const DHCP_MSG_REQUEST: u8 = 3;
pub const DHCP_MSG_ACK: u8 = 5;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

impl DhcpMessageType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => DhcpMessageType::Discover,
            2 => DhcpMessageType::Offer,
            3 => DhcpMessageType::Request,
            4 => DhcpMessageType::Decline,
            5 => DhcpMessageType::Ack,
            6 => DhcpMessageType::Nak,
            7 => DhcpMessageType::Release,
            8 => DhcpMessageType::Inform,
            _ => return None,
        })
    }
}

/// Parsed subset of a DHCP message (sufficient for a minimal DHCP server).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DhcpMessage {
    pub op: u8,
    pub transaction_id: u32,
    pub flags: u16,
    pub client_mac: MacAddr,
    pub message_type: DhcpMessageType,
    pub requested_ip: Option<Ipv4Addr>,
    pub server_identifier: Option<Ipv4Addr>,
}

impl DhcpMessage {
    pub fn parse(buf: &[u8]) -> Result<Self, PacketError> {
        // BOOTP fixed header is 236 bytes, followed by the DHCP magic cookie.
        super::ensure_len(buf, DhcpOfferAckBuilder::BOOTP_FIXED_LEN + 4)?;

        let op = buf[0];
        if op != DHCP_OP_BOOTREQUEST && op != DHCP_OP_BOOTREPLY {
            return Err(PacketError::Malformed("DHCP op is not BOOTREQUEST/BOOTREPLY"));
        }
        let htype = buf[1];
        let hlen = buf[2];
        if htype != 1 || hlen != 6 {
            return Err(PacketError::Unsupported("non-Ethernet DHCP message"));
        }

        let transaction_id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let flags = u16::from_be_bytes([buf[10], buf[11]]);

        let mut mac = [0u8; 6];
        mac.copy_from_slice(&buf[28..34]);
        let client_mac = MacAddr(mac);

        if buf[DhcpOfferAckBuilder::BOOTP_FIXED_LEN..DhcpOfferAckBuilder::BOOTP_FIXED_LEN + 4] != DHCP_MAGIC_COOKIE
        {
            return Err(PacketError::Malformed("missing DHCP magic cookie"));
        }

        let mut message_type = None;
        let mut requested_ip = None;
        let mut server_identifier = None;

        let mut idx = DhcpOfferAckBuilder::BOOTP_FIXED_LEN + 4;
        while idx < buf.len() {
            let code = buf[idx];
            idx += 1;
            match code {
                0 => continue, // pad
                DHCP_OPT_END => break,
                _ => {
                    super::ensure_len(buf, idx + 1)?;
                    let len = buf[idx] as usize;
                    idx += 1;
                    super::ensure_len(buf, idx + len)?;
                    let data = &buf[idx..idx + len];
                    idx += len;

                    match code {
                        DHCP_OPT_MESSAGE_TYPE if len == 1 => {
                            message_type = DhcpMessageType::from_u8(data[0]);
                        }
                        DHCP_OPT_REQUESTED_IP if len == 4 => {
                            requested_ip = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
                        }
                        DHCP_OPT_SERVER_IDENTIFIER if len == 4 => {
                            server_identifier = Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(Self {
            op,
            transaction_id,
            flags,
            client_mac,
            message_type: message_type.ok_or(PacketError::Malformed("missing DHCP message type option"))?,
            requested_ip,
            server_identifier,
        })
    }
}

pub struct DhcpOfferAckBuilder<'a> {
    pub message_type: u8,
    pub transaction_id: u32,
    /// BOOTP flags field from the client request (most notably the broadcast bit).
    ///
    /// For DHCP servers, echoing this value is common practice and helps clients that
    /// require broadcast replies during early configuration.
    pub flags: u16,
    pub client_mac: MacAddr,
    pub your_ip: Ipv4Addr,
    pub server_ip: Ipv4Addr,
    pub subnet_mask: Ipv4Addr,
    pub router: Ipv4Addr,
    pub dns_servers: &'a [Ipv4Addr],
    pub lease_time_secs: u32,
}

impl<'a> DhcpOfferAckBuilder<'a> {
    /// Base BOOTP header length without options.
    pub const BOOTP_FIXED_LEN: usize = 236;

    pub fn len(&self) -> Result<usize, PacketError> {
        // Options length:
        // - Magic cookie (4)
        // - Message type (3)
        // - Server identifier (6)
        // - Subnet mask (6)
        // - Router (6)
        // - DNS servers (2 + 4*N)
        // - Lease time (6)
        // - Renewal time (T1, 6)
        // - Rebinding time (T2, 6)
        // - End (1)
        let mut options_len: usize = 4 + 3 + 6 + 6 + 6 + 6 + 6 + 6 + 1;
        if !self.dns_servers.is_empty() {
            let dns_len = 4 * self.dns_servers.len();
            if dns_len > u8::MAX as usize {
                return Err(PacketError::Malformed("too many DNS servers for DHCP option"));
            }
            options_len = options_len
                .checked_add(2 + dns_len)
                .ok_or(PacketError::Malformed("DHCP options length overflow"))?;
        }
        Self::BOOTP_FIXED_LEN
            .checked_add(options_len)
            .ok_or(PacketError::Malformed("DHCP length overflow"))
    }

    #[cfg(feature = "alloc")]
    pub fn build_vec(&self) -> Result<alloc::vec::Vec<u8>, PacketError> {
        let len = self.len()?;
        let mut buf = alloc::vec![0u8; len];
        let written = self.write(&mut buf)?;
        debug_assert_eq!(written, buf.len());
        Ok(buf)
    }

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        let total_len = self.len()?;
        ensure_out_buf_len(out, total_len)?;

        // BOOTP header.
        out[0] = 2; // op = BOOTREPLY
        out[1] = 1; // htype = ethernet
        out[2] = 6; // hlen
        out[3] = 0; // hops
        out[4..8].copy_from_slice(&self.transaction_id.to_be_bytes());
        out[8..10].copy_from_slice(&0u16.to_be_bytes()); // secs
        out[10..12].copy_from_slice(&self.flags.to_be_bytes());
        out[12..16].copy_from_slice(&Ipv4Addr::UNSPECIFIED.octets()); // ciaddr
        out[16..20].copy_from_slice(&self.your_ip.octets()); // yiaddr
        out[20..24].copy_from_slice(&self.server_ip.octets()); // siaddr
        out[24..28].copy_from_slice(&Ipv4Addr::UNSPECIFIED.octets()); // giaddr

        // chaddr: 16 bytes.
        out[28..44].fill(0);
        out[28..34].copy_from_slice(&self.client_mac.0);

        // sname (64) + file (128)
        out[44..Self::BOOTP_FIXED_LEN].fill(0);

        // Options
        let mut off = Self::BOOTP_FIXED_LEN;
        out[off..off + 4].copy_from_slice(&DHCP_MAGIC_COOKIE);
        off += 4;

        out[off] = DHCP_OPT_MESSAGE_TYPE;
        out[off + 1] = 1;
        out[off + 2] = self.message_type;
        off += 3;

        out[off] = DHCP_OPT_SERVER_IDENTIFIER;
        out[off + 1] = 4;
        out[off + 2..off + 6].copy_from_slice(&self.server_ip.octets());
        off += 6;

        out[off] = DHCP_OPT_SUBNET_MASK;
        out[off + 1] = 4;
        out[off + 2..off + 6].copy_from_slice(&self.subnet_mask.octets());
        off += 6;

        out[off] = DHCP_OPT_ROUTER;
        out[off + 1] = 4;
        out[off + 2..off + 6].copy_from_slice(&self.router.octets());
        off += 6;

        if !self.dns_servers.is_empty() {
            let dns_len = 4 * self.dns_servers.len();
            if dns_len > u8::MAX as usize {
                return Err(PacketError::Malformed("too many DNS servers for DHCP option"));
            }
            out[off] = DHCP_OPT_DNS;
            out[off + 1] = dns_len as u8;
            off += 2;
            for ip in self.dns_servers {
                out[off..off + 4].copy_from_slice(&ip.octets());
                off += 4;
            }
        }

        out[off] = DHCP_OPT_IP_LEASE_TIME;
        out[off + 1] = 4;
        out[off + 2..off + 6].copy_from_slice(&self.lease_time_secs.to_be_bytes());
        off += 6;

        // Renewal/rebinding timers, best-effort defaults (same logic as many DHCP servers).
        let t1 = self.lease_time_secs / 2;
        let t2 = self.lease_time_secs.saturating_mul(7) / 8;

        out[off] = DHCP_OPT_RENEWAL_TIME;
        out[off + 1] = 4;
        out[off + 2..off + 6].copy_from_slice(&t1.to_be_bytes());
        off += 6;

        out[off] = DHCP_OPT_REBINDING_TIME;
        out[off + 1] = 4;
        out[off + 2..off + 6].copy_from_slice(&t2.to_be_bytes());
        off += 6;

        out[off] = DHCP_OPT_END;
        off += 1;

        debug_assert_eq!(off, total_len);
        Ok(total_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_dhcp_offer_minimal() {
        let builder = DhcpOfferAckBuilder {
            message_type: DHCP_MSG_OFFER,
            transaction_id: 0x12345678,
            flags: 0x8000,
            client_mac: MacAddr([0, 1, 2, 3, 4, 5]),
            your_ip: Ipv4Addr::new(10, 0, 0, 100),
            server_ip: Ipv4Addr::new(10, 0, 0, 1),
            subnet_mask: Ipv4Addr::new(255, 255, 255, 0),
            router: Ipv4Addr::new(10, 0, 0, 1),
            dns_servers: &[Ipv4Addr::new(1, 1, 1, 1)],
            lease_time_secs: 3600,
        };
        let mut buf = [0u8; 512];
        let len = builder.write(&mut buf).unwrap();
        assert!(len > DhcpOfferAckBuilder::BOOTP_FIXED_LEN);
        assert_eq!(buf[0], 2);
        assert_eq!(buf[4..8], 0x12345678u32.to_be_bytes());
        assert_eq!(buf[16..20], builder.your_ip.octets());
        assert_eq!(buf[28..34], builder.client_mac.0);
        assert_eq!(buf[DhcpOfferAckBuilder::BOOTP_FIXED_LEN..DhcpOfferAckBuilder::BOOTP_FIXED_LEN + 4], DHCP_MAGIC_COOKIE);

        // Spot-check that T1/T2 are present and match our defaults.
        let opts = &buf[DhcpOfferAckBuilder::BOOTP_FIXED_LEN + 4..len];
        fn find_opt<'a>(opts: &'a [u8], code: u8) -> Option<&'a [u8]> {
            let mut i = 0;
            while i < opts.len() {
                let c = opts[i];
                i += 1;
                if c == 0 {
                    continue;
                }
                if c == 255 {
                    break;
                }
                if i >= opts.len() {
                    break;
                }
                let l = opts[i] as usize;
                i += 1;
                if i + l > opts.len() {
                    break;
                }
                if c == code {
                    return Some(&opts[i..i + l]);
                }
                i += l;
            }
            None
        }

        let t1 = u32::from_be_bytes(find_opt(opts, DHCP_OPT_RENEWAL_TIME).unwrap().try_into().unwrap());
        let t2 = u32::from_be_bytes(find_opt(opts, DHCP_OPT_REBINDING_TIME).unwrap().try_into().unwrap());
        assert_eq!(t1, 1800);
        assert_eq!(t2, 3150);
    }

    #[test]
    fn parse_dhcp_discover() {
        let mac = MacAddr([0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]);
        let xid: u32 = 0x12345678;

        let mut buf = vec![0u8; DhcpOfferAckBuilder::BOOTP_FIXED_LEN + 4];
        buf[0] = DHCP_OP_BOOTREQUEST;
        buf[1] = 1;
        buf[2] = 6;
        buf[4..8].copy_from_slice(&xid.to_be_bytes());
        buf[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
        buf[28..34].copy_from_slice(&mac.0);
        buf[DhcpOfferAckBuilder::BOOTP_FIXED_LEN..DhcpOfferAckBuilder::BOOTP_FIXED_LEN + 4]
            .copy_from_slice(&DHCP_MAGIC_COOKIE);
        buf.extend_from_slice(&[DHCP_OPT_MESSAGE_TYPE, 1, DHCP_MSG_DISCOVER]);
        buf.extend_from_slice(&[DHCP_OPT_END]);

        let msg = DhcpMessage::parse(&buf).unwrap();
        assert_eq!(msg.op, DHCP_OP_BOOTREQUEST);
        assert_eq!(msg.transaction_id, xid);
        assert_eq!(msg.flags, 0x8000);
        assert_eq!(msg.client_mac, mac);
        assert_eq!(msg.message_type, DhcpMessageType::Discover);
    }

    #[test]
    fn parse_dhcp_offer_from_builder() {
        let offer = DhcpOfferAckBuilder {
            message_type: DHCP_MSG_OFFER,
            transaction_id: 0x12345678,
            flags: 0x8000,
            client_mac: MacAddr([0, 1, 2, 3, 4, 5]),
            your_ip: Ipv4Addr::new(10, 0, 0, 100),
            server_ip: Ipv4Addr::new(10, 0, 0, 1),
            subnet_mask: Ipv4Addr::new(255, 255, 255, 0),
            router: Ipv4Addr::new(10, 0, 0, 1),
            dns_servers: &[Ipv4Addr::new(1, 1, 1, 1)],
            lease_time_secs: 3600,
        }
        .build_vec()
        .unwrap();

        let msg = DhcpMessage::parse(&offer).unwrap();
        assert_eq!(msg.op, DHCP_OP_BOOTREPLY);
        assert_eq!(msg.transaction_id, 0x12345678);
        assert_eq!(msg.message_type, DhcpMessageType::Offer);
        assert_eq!(msg.client_mac, MacAddr([0, 1, 2, 3, 4, 5]));
    }
}
