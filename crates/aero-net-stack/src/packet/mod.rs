#![forbid(unsafe_code)]

//! Compatibility shim for packet parsing/building.
//!
//! The user-space network stack originally carried its own packet parsing/building code in
//! `aero-net-stack/src/packet/*`. That code was a near-duplicate of `nt-packetlib` and would drift
//! over time. `nt-packetlib` is now the single source of truth.

pub use nt_packetlib::packet::checksum;
pub use nt_packetlib::packet::MacAddr;
pub use nt_packetlib::packet::PacketError;

/// Backwards-compatible alias for the old `ParseError` type.
pub type ParseError = PacketError;

// Re-export module namespaces for callers that prefer `packet::ethernet::...` paths.
pub use nt_packetlib::packet::{arp, dhcp, dns, ethernet, icmp, ipv4, tcp, udp};

// Preserve the old `use aero_net_stack::packet::*;` ergonomics by re-exporting common items at the
// top-level.
pub use nt_packetlib::packet::arp::{
    ArpPacket, ArpPacketBuilder, ARP_OP_REPLY, ARP_OP_REQUEST, HTYPE_ETHERNET, PTYPE_IPV4,
};
pub use nt_packetlib::packet::dhcp::{
    DhcpMessage, DhcpMessageType, DhcpOfferAckBuilder, DHCP_MSG_ACK, DHCP_MSG_DISCOVER,
    DHCP_MSG_OFFER, DHCP_MSG_REQUEST,
};
pub use nt_packetlib::packet::dns::{
    parse_single_query, qname_to_string, DnsQuery, DnsResponseBuilder, DnsResponseCode,
};
pub use nt_packetlib::packet::ethernet::{
    EthernetFrame, EthernetFrameBuilder, ETHERTYPE_ARP, ETHERTYPE_IPV4,
};
pub use nt_packetlib::packet::icmp::{IcmpEcho, IcmpEchoBuilder, Icmpv4Packet};
pub use nt_packetlib::packet::ipv4::{
    Ipv4Packet, Ipv4PacketBuilder, IPPROTO_ICMP, IPPROTO_TCP, IPPROTO_UDP,
};
pub use nt_packetlib::packet::tcp::{TcpFlags, TcpSegment, TcpSegmentBuilder};
pub use nt_packetlib::packet::udp::{UdpPacket, UdpPacketBuilder};

/// Compatibility constants matching the old `EtherType` struct.
pub struct EtherType;

impl EtherType {
    pub const IPV4: u16 = ETHERTYPE_IPV4;
    pub const ARP: u16 = ETHERTYPE_ARP;
}

/// Compatibility constants matching the old `Ipv4Protocol` struct.
pub struct Ipv4Protocol;

impl Ipv4Protocol {
    pub const ICMP: u8 = IPPROTO_ICMP;
    pub const TCP: u8 = IPPROTO_TCP;
    pub const UDP: u8 = IPPROTO_UDP;
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsType {
    A = 1,
}

/// Compatibility alias for the old `UdpDatagram` name.
pub type UdpDatagram<'a> = UdpPacket<'a>;

/// Compatibility alias for the old `IcmpPacket` name (ICMPv4).
pub type IcmpPacket<'a> = Icmpv4Packet<'a>;

/// Compatibility alias for the old `IcmpEchoPacket` name.
pub type IcmpEchoPacket<'a> = IcmpEcho<'a>;
