//! Minimal DHCP (BOOTP) message builder.
//!
//! DHCP is technically above UDP, but the emulator network stack often needs to
//! craft simple OFFER/ACK responses. This module intentionally implements only
//! the subset we need for that.

use core::net::Ipv4Addr;

use super::{ensure_out_buf_len, MacAddr, PacketError};

pub const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

pub const DHCP_OPT_MESSAGE_TYPE: u8 = 53;
pub const DHCP_OPT_SERVER_IDENTIFIER: u8 = 54;
pub const DHCP_OPT_SUBNET_MASK: u8 = 1;
pub const DHCP_OPT_ROUTER: u8 = 3;
pub const DHCP_OPT_DNS: u8 = 6;
pub const DHCP_OPT_IP_LEASE_TIME: u8 = 51;
pub const DHCP_OPT_END: u8 = 255;

pub const DHCP_MSG_OFFER: u8 = 2;
pub const DHCP_MSG_ACK: u8 = 5;

pub struct DhcpOfferAckBuilder<'a> {
    pub message_type: u8,
    pub transaction_id: u32,
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

    pub fn write(&self, out: &mut [u8]) -> Result<usize, PacketError> {
        // Worst-case options: we only write a small set.
        // Compute exact length so callers can use the returned size.
        let mut options_len = 4; // magic cookie
        options_len += 3; // msg type
        options_len += 6; // server identifier
        options_len += 6; // subnet mask
        options_len += 6; // router
        if !self.dns_servers.is_empty() {
            options_len += 2 + 4 * self.dns_servers.len();
        }
        options_len += 6; // lease time
        options_len += 1; // end

        let total_len = Self::BOOTP_FIXED_LEN + options_len;
        ensure_out_buf_len(out, total_len)?;

        // BOOTP header.
        out[0] = 2; // op = BOOTREPLY
        out[1] = 1; // htype = ethernet
        out[2] = 6; // hlen
        out[3] = 0; // hops
        out[4..8].copy_from_slice(&self.transaction_id.to_be_bytes());
        out[8..10].copy_from_slice(&0u16.to_be_bytes()); // secs
        out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // flags: broadcast
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
    }
}

