use aero_net_stack::packet::*;
use aero_net_stack::{Action, DnsResolved, NetworkStack, StackConfig, TcpProxyEvent};
use core::net::Ipv4Addr;

#[test]
fn dhcp_dns_tcp_flow() {
    let mut stack = NetworkStack::new(StackConfig::default());
    let guest_mac = MacAddr([0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee]);

    // --- DHCP handshake ---
    let xid = 0x1020_3040;
    let discover = build_dhcp_discover(xid, guest_mac);
    let discover_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &discover,
    );

    let actions = stack.process_outbound_ethernet(&discover_frame, 0);
    let offer = extract_single_frame(&actions);
    let offer_msg = parse_dhcp_from_frame(&offer);
    assert_eq!(offer_msg.options.message_type, Some(DhcpMessageType::Offer));
    assert_eq!(offer_msg.yiaddr, stack.config().guest_ip);

    let request = build_dhcp_request(
        xid,
        guest_mac,
        stack.config().guest_ip,
        stack.config().gateway_ip,
    );
    let request_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &request,
    );
    let actions = stack.process_outbound_ethernet(&request_frame, 1);
    let ack = extract_single_frame(&actions);
    let ack_msg = parse_dhcp_from_frame(&ack);
    assert_eq!(ack_msg.options.message_type, Some(DhcpMessageType::Ack));
    assert!(stack.is_ip_assigned());

    // Networking is default-deny until enabled.
    let syn_denied = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        Ipv4Addr::new(93, 184, 216, 34),
        40000,
        80,
        100,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&syn_denied, 2);
    assert!(actions
        .iter()
        .all(|a| !matches!(a, Action::TcpProxyConnect { .. })));
    assert!(actions.iter().any(|a| matches!(a, Action::EmitFrame(_))));

    stack.set_network_enabled(true);

    // --- DNS lookup ---
    let dns_query = build_dns_query(0x1234, "example.com");
    let dns_frame = wrap_udp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        stack.config().dns_ip,
        53000,
        53,
        &dns_query,
    );
    let actions = stack.process_outbound_ethernet(&dns_frame, 10);
    let (dns_req_id, name) = match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
        _ => panic!("expected single DnsResolve action, got {actions:?}"),
    };
    assert_eq!(name, "example.com");

    let dns_actions = stack.handle_dns_resolved(
        DnsResolved {
            request_id: dns_req_id,
            name,
            addr: Some(Ipv4Addr::new(93, 184, 216, 34)),
            ttl_secs: 60,
        },
        11,
    );
    let dns_resp_frame = extract_single_frame(&dns_actions);
    assert_dns_response_has_a_record(&dns_resp_frame, 0x1234, [93, 184, 216, 34]);

    // --- TCP connect + data ---
    let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
    let guest_port = 40001;
    let guest_isn = 5000;
    let syn = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_isn,
        0,
        TcpFlags::SYN,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&syn, 20);
    let (conn_id, syn_ack_frame) = extract_tcp_connect_and_frame(&actions);
    let syn_ack = parse_tcp_from_frame(&syn_ack_frame);
    assert_eq!(
        syn_ack.flags & (TcpFlags::SYN | TcpFlags::ACK),
        TcpFlags::SYN | TcpFlags::ACK
    );
    assert_eq!(syn_ack.ack, guest_isn + 1);

    // Proxy connects.
    let proxy_actions = stack.handle_tcp_proxy_event(
        TcpProxyEvent::Connected {
            connection_id: conn_id,
        },
        21,
    );
    assert!(proxy_actions.is_empty());

    // Complete handshake.
    let ack = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_isn + 1,
        syn_ack.seq + 1,
        TcpFlags::ACK,
        &[],
    );
    let actions = stack.process_outbound_ethernet(&ack, 22);
    assert!(actions.is_empty());

    // Send application data.
    let payload = b"GET / HTTP/1.0\r\n\r\n";
    let psh = wrap_tcp_ipv4_eth(
        guest_mac,
        stack.config().our_mac,
        stack.config().guest_ip,
        remote_ip,
        guest_port,
        80,
        guest_isn + 1,
        syn_ack.seq + 1,
        TcpFlags::ACK | TcpFlags::PSH,
        payload,
    );
    let actions = stack.process_outbound_ethernet(&psh, 23);
    assert!(actions.iter().any(|a| matches!(a, Action::TcpProxySend { connection_id, data } if *connection_id == conn_id && data == payload)));
    assert!(actions.iter().any(|a| matches!(a, Action::EmitFrame(_))));

    // Receive data back from proxy.
    let resp_payload = b"HTTP/1.0 200 OK\r\n\r\n";
    let actions = stack.handle_tcp_proxy_event(
        TcpProxyEvent::Data {
            connection_id: conn_id,
            data: resp_payload.to_vec(),
        },
        24,
    );
    let frame = extract_single_frame(&actions);
    let seg = parse_tcp_from_frame(&frame);
    assert_eq!(seg.payload, resp_payload);
    assert_eq!(
        seg.flags & (TcpFlags::ACK | TcpFlags::PSH),
        TcpFlags::ACK | TcpFlags::PSH
    );
}

fn extract_single_frame(actions: &[Action]) -> Vec<u8> {
    let frames: Vec<Vec<u8>> = actions
        .iter()
        .filter_map(|a| match a {
            Action::EmitFrame(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(frames.len(), 1, "expected 1 EmitFrame, got {actions:?}");
    frames.into_iter().next().unwrap()
}

fn extract_tcp_connect_and_frame(actions: &[Action]) -> (u32, Vec<u8>) {
    let mut conn_id = None;
    let mut frame = None;
    for a in actions {
        match a {
            Action::TcpProxyConnect {
                connection_id,
                remote_ip: _,
                remote_port: _,
            } => conn_id = Some(*connection_id),
            Action::EmitFrame(f) => frame = Some(f.clone()),
            _ => {}
        }
    }
    (
        conn_id.expect("missing TcpProxyConnect"),
        frame.expect("missing EmitFrame"),
    )
}

fn parse_dhcp_from_frame(frame: &[u8]) -> DhcpMessage {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype, EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    assert_eq!(ip.protocol, Ipv4Protocol::UDP);
    let udp = UdpDatagram::parse(ip.payload).unwrap();
    assert_eq!(udp.src_port, 67);
    assert_eq!(udp.dst_port, 68);
    DhcpMessage::parse(udp.payload).unwrap()
}

fn parse_tcp_from_frame(frame: &[u8]) -> TcpSegment<'_> {
    let eth = EthernetFrame::parse(frame).unwrap();
    assert_eq!(eth.ethertype, EtherType::IPV4);
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    assert_eq!(ip.protocol, Ipv4Protocol::TCP);
    TcpSegment::parse(ip.payload).unwrap()
}

fn assert_dns_response_has_a_record(frame: &[u8], id: u16, addr: [u8; 4]) {
    let eth = EthernetFrame::parse(frame).unwrap();
    let ip = Ipv4Packet::parse(eth.payload).unwrap();
    let udp = UdpDatagram::parse(ip.payload).unwrap();
    assert_eq!(udp.src_port, 53);
    let dns = udp.payload;
    assert_eq!(&dns[0..2], &id.to_be_bytes());
    // QR=1
    assert_eq!(dns[2] & 0x80, 0x80);
    // ANCOUNT == 1
    assert_eq!(&dns[6..8], &1u16.to_be_bytes());
    // Answer RDATA is the final 4 bytes for our minimal response.
    assert_eq!(&dns[dns.len() - 4..], &addr);
}

fn wrap_udp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp = UdpDatagram::serialize(src_ip, dst_ip, src_port, dst_port, payload);
    let ip = Ipv4Packet::serialize(src_ip, dst_ip, Ipv4Protocol::UDP, 1, 64, &udp);
    EthernetFrame::serialize(dst_mac, src_mac, EtherType::IPV4, &ip)
}

fn wrap_tcp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    payload: &[u8],
) -> Vec<u8> {
    let tcp = TcpSegment::serialize(
        src_ip, dst_ip, src_port, dst_port, seq, ack, flags, 65535, payload,
    );
    let ip = Ipv4Packet::serialize(src_ip, dst_ip, Ipv4Protocol::TCP, 1, 64, &tcp);
    EthernetFrame::serialize(dst_mac, src_mac, EtherType::IPV4, &ip)
}

fn build_dhcp_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC len
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]);
    out.extend_from_slice(&[53, 1, 1]); // DHCPDISCOVER
    out.push(255);
    out
}

fn build_dhcp_request(
    xid: u32,
    mac: MacAddr,
    requested_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]);
    out.extend_from_slice(&[53, 1, 3]); // DHCPREQUEST
    out.extend_from_slice(&[50, 4]);
    out.extend_from_slice(&requested_ip.octets());
    out.extend_from_slice(&[54, 4]);
    out.extend_from_slice(&server_id.octets());
    out.push(255);
    out
}

fn build_dns_query(id: u16, name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&(DnsType::A as u16).to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}
