use emulator::io::net::trace::{CaptureArtifactOnPanic, FrameDirection, NetTraceConfig, NetTracer};

fn make_ipv4_tcp_frame(
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    flags: u8,
    payload: &[u8],
) -> Vec<u8> {
    let ip_header_len = 20usize;
    let tcp_header_len = 20usize;

    let total_len = ip_header_len + tcp_header_len + payload.len();

    let mut buf = Vec::with_capacity(14 + total_len);

    buf.extend_from_slice(&dst_mac);
    buf.extend_from_slice(&src_mac);
    buf.extend_from_slice(&0x0800u16.to_be_bytes()); // ethertype: IPv4

    buf.push(0x45); // version + IHL
    buf.push(0); // dscp/ecn
    buf.extend_from_slice(&(total_len as u16).to_be_bytes());
    buf.extend_from_slice(&0u16.to_be_bytes()); // identification
    buf.extend_from_slice(&0u16.to_be_bytes()); // flags/fragment
    buf.push(64); // ttl
    buf.push(6); // protocol: TCP
    buf.extend_from_slice(&0u16.to_be_bytes()); // hdr checksum (ignored)
    buf.extend_from_slice(&src_ip);
    buf.extend_from_slice(&dst_ip);

    buf.extend_from_slice(&src_port.to_be_bytes());
    buf.extend_from_slice(&dst_port.to_be_bytes());
    buf.extend_from_slice(&0u32.to_be_bytes()); // seq
    buf.extend_from_slice(&0u32.to_be_bytes()); // ack
    buf.push((tcp_header_len as u8 / 4) << 4); // data offset
    buf.push(flags);
    buf.extend_from_slice(&0u16.to_be_bytes()); // window
    buf.extend_from_slice(&0u16.to_be_bytes()); // checksum (ignored)
    buf.extend_from_slice(&0u16.to_be_bytes()); // urgent

    buf.extend_from_slice(payload);
    buf
}

#[derive(Debug)]
struct Block<'a> {
    block_type: u32,
    body: &'a [u8],
}

fn parse_blocks(mut bytes: &[u8]) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        assert!(bytes.len() >= 12, "pcapng truncated");
        let block_type = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let total_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert!(total_len >= 12, "pcapng invalid block length");
        assert!(total_len % 4 == 0, "pcapng block length not 32-bit aligned");
        assert!(bytes.len() >= total_len, "pcapng truncated block");
        assert_eq!(
            &bytes[total_len - 4..total_len],
            &(total_len as u32).to_le_bytes(),
            "pcapng footer length mismatch",
        );
        let body = &bytes[8..total_len - 4];
        out.push(Block { block_type, body });
        bytes = &bytes[total_len..];
    }
    out
}

fn epb_payload_and_flags(body: &[u8]) -> (&[u8], Option<u32>) {
    assert!(body.len() >= 20);
    let cap_len = u32::from_le_bytes(body[12..16].try_into().unwrap()) as usize;
    let payload_offset = 20;
    let payload_end = payload_offset + cap_len;
    assert!(body.len() >= payload_end);
    let payload = &body[payload_offset..payload_end];

    let mut opt_off = payload_end;
    opt_off = (opt_off + 3) & !3;
    let mut flags = None;
    while opt_off + 4 <= body.len() {
        let code = u16::from_le_bytes(body[opt_off..opt_off + 2].try_into().unwrap());
        let len = u16::from_le_bytes(body[opt_off + 2..opt_off + 4].try_into().unwrap()) as usize;
        opt_off += 4;
        if code == 0 {
            break;
        }
        if code == 2 && len == 4 {
            flags = Some(u32::from_le_bytes(
                body[opt_off..opt_off + 4].try_into().unwrap(),
            ));
        }
        opt_off += len;
        opt_off = (opt_off + 3) & !3;
    }

    (payload, flags)
}

#[test]
fn synthetic_tcp_exchange_is_written_to_pcapng() {
    let tracer = NetTracer::new(NetTraceConfig::default());
    tracer.enable();
    let _artifact =
        CaptureArtifactOnPanic::for_test(&tracer, "synthetic_tcp_exchange_is_written_to_pcapng");

    let guest_mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let remote_mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
    let guest_ip = [10, 0, 2, 15];
    let remote_ip = [93, 184, 216, 34]; // example.com

    let syn = make_ipv4_tcp_frame(
        guest_mac,
        remote_mac,
        guest_ip,
        remote_ip,
        12345,
        80,
        0x02,
        &[],
    );
    let syn_ack = make_ipv4_tcp_frame(
        remote_mac,
        guest_mac,
        remote_ip,
        guest_ip,
        80,
        12345,
        0x12,
        &[],
    );

    tracer.record_ethernet(FrameDirection::GuestTx, &syn);
    tracer.record_ethernet(FrameDirection::GuestRx, &syn_ack);

    let pcapng = tracer.export_pcapng();
    let blocks = parse_blocks(&pcapng);

    let epbs: Vec<_> = blocks
        .iter()
        .filter(|b| b.block_type == 0x0000_0006)
        .collect();
    assert_eq!(epbs.len(), 2, "expected 2 Enhanced Packet Blocks");

    let (tx_payload, tx_flags) = epb_payload_and_flags(epbs[0].body);
    let (rx_payload, rx_flags) = epb_payload_and_flags(epbs[1].body);

    assert!(tx_payload.len() >= 14);
    let tx_ethertype = u16::from_be_bytes([tx_payload[12], tx_payload[13]]);
    assert_eq!(tx_ethertype, 0x0800);
    assert_eq!(tx_payload[23], 6); // IPv4 protocol field
    assert_eq!(tx_flags.map(|v| v & 0b11), Some(2));

    assert!(rx_payload.len() >= 14);
    let rx_ethertype = u16::from_be_bytes([rx_payload[12], rx_payload[13]]);
    assert_eq!(rx_ethertype, 0x0800);
    assert_eq!(rx_payload[23], 6);
    assert_eq!(rx_flags.map(|v| v & 0b11), Some(1));
}
