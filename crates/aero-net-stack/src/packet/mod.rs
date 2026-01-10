#![forbid(unsafe_code)]

mod arp;
mod dhcp;
mod dns;
mod ethernet;
mod icmp;
mod ipv4;
mod tcp;
mod udp;

pub use arp::{ArpOperation, ArpPacket};
pub use dhcp::{DhcpMessage, DhcpMessageType, DhcpOption, DhcpOptions};
pub use dns::{DnsMessage, DnsQuestion, DnsResponseCode, DnsType};
pub use ethernet::{EtherType, EthernetFrame, MacAddr};
pub use icmp::{IcmpEchoPacket, IcmpPacket};
pub use ipv4::{Ipv4Packet, Ipv4Protocol};
pub use tcp::{TcpFlags, TcpSegment};
pub use udp::UdpDatagram;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Truncated,
    Invalid(&'static str),
}
