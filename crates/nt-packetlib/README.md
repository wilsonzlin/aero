# nt-packetlib

Reusable packet parsing/crafting helpers for Aero's in-emulator network stack.

Design goals:

- **Zero-copy-ish parsing**: parsers borrow the original `&[u8]` and expose typed accessors without allocating.
- **Checksum correctness**: helpers for IPv4 header checksums and TCP/UDP pseudo-header checksums.
- **Robustness**: parsing APIs return `Result<_, PacketError>`; malformed input should never panic.
- **Flexible build targets**: supports `no_std` with an optional `alloc` feature.

## Cargo features

- `std` (default): enables `std` + `alloc`.
- `alloc`: enables `alloc` for `no_std` environments; provides `build_vec()` helpers for builders.

## Packet layers

Parsing:

- Ethernet II: `ethernet::EthernetFrame`
- ARP: `arp::ArpPacket`
- IPv4: `ipv4::Ipv4Packet` (options exposed as raw bytes)
- UDP: `udp::UdpPacket`
- TCP: `tcp::TcpSegment`
- ICMPv4: `icmp::Icmpv4Packet` + echo helper

Building/serialization:

- Ethernet: `ethernet::EthernetFrameBuilder`
- ARP: `arp::ArpPacketBuilder`, `arp::ArpReplyFrameBuilder`
- IPv4: `ipv4::Ipv4PacketBuilder`
- UDP: `udp::UdpPacketBuilder`
- TCP: `tcp::TcpSegmentBuilder`
- ICMPv4: `icmp::IcmpEchoBuilder`
- Minimal DHCP: `dhcp::DhcpOfferAckBuilder` (+ `dhcp::DhcpMessage` parser for requests)
- Minimal DNS: `dns::DnsResponseBuilder` (+ `dns::parse_single_query`)

## Example: build an ARP reply frame

```rust
use core::net::Ipv4Addr;
use nt_packetlib::io::net::packet::{arp::ArpReplyFrameBuilder, MacAddr};

let reply = ArpReplyFrameBuilder {
    sender_mac: MacAddr([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
    sender_ip: Ipv4Addr::new(10, 0, 2, 2),
    target_mac: MacAddr([0xde, 0xad, 0xbe, 0xef, 0x00, 0x01]),
    target_ip: Ipv4Addr::new(10, 0, 2, 15),
}
.build_vec()
.unwrap();
```

