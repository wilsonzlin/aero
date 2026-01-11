use nt_packetlib::io::net::packet::{
    arp::ArpPacket, dhcp::DhcpMessage, dns::parse_single_query, ethernet::EthernetFrame,
    icmp::Icmpv4Packet, ipv4::Ipv4Packet, tcp::TcpSegment, udp::UdpPacket,
};

struct XorShift64(u64);

impl XorShift64 {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }

    fn gen_vec(&mut self, max_len: usize) -> Vec<u8> {
        let len = (self.next_u64() as usize) % (max_len + 1);
        let mut v = vec![0u8; len];
        for b in &mut v {
            *b = self.next_u8();
        }
        v
    }
}

#[test]
fn fuzz_parsers_do_not_panic() {
    let mut rng = XorShift64(0x1234_5678_9abc_def0);
    for _ in 0..10_000 {
        let data = rng.gen_vec(2048);

        let _ = EthernetFrame::parse(&data);
        let _ = ArpPacket::parse(&data);
        let _ = Ipv4Packet::parse(&data);
        let _ = UdpPacket::parse(&data);
        let _ = TcpSegment::parse(&data);
        let _ = Icmpv4Packet::parse(&data);
        let _ = parse_single_query(&data);
        let _ = DhcpMessage::parse(&data);
    }
}
