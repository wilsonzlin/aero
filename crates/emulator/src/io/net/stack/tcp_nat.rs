use std::collections::HashMap;
use std::net::Ipv4Addr;

use super::ethernet::{build_ethernet_frame, EtherType, MacAddr};
use super::ipv4::{checksum_pseudo_header, finalize_checksum, IpProtocol, Ipv4Packet, Ipv4PacketBuilder};
use super::ProxyAction;

const FLAG_FIN: u16 = 0x01;
const FLAG_SYN: u16 = 0x02;
const FLAG_RST: u16 = 0x04;
const FLAG_PSH: u16 = 0x08;
const FLAG_ACK: u16 = 0x10;

#[derive(Debug, Clone, Copy)]
pub struct TcpSegment<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u16,
    pub window: u16,
    pub payload: &'a [u8],
}

impl<'a> TcpSegment<'a> {
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < 20 {
            return None;
        }
        let data_offset = (buf[12] >> 4) as usize * 4;
        if data_offset < 20 || data_offset > buf.len() {
            return None;
        }
        Some(Self {
            src_port: u16::from_be_bytes([buf[0], buf[1]]),
            dst_port: u16::from_be_bytes([buf[2], buf[3]]),
            seq: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ack: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
            flags: buf[13] as u16,
            window: u16::from_be_bytes([buf[14], buf[15]]),
            payload: &buf[data_offset..],
        })
    }
}

fn build_tcp_segment(
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u16,
    window: u16,
    payload: &[u8],
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
) -> Vec<u8> {
    let header_len = 20usize;
    let total_len = header_len + payload.len();
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&src_port.to_be_bytes());
    out.extend_from_slice(&dst_port.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&ack.to_be_bytes());
    out.push((5u8 << 4) | 0); // data offset + reserved
    out.push(flags as u8);
    out.extend_from_slice(&window.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out.extend_from_slice(&0u16.to_be_bytes()); // urgent pointer
    out.extend_from_slice(payload);

    let mut sum = checksum_pseudo_header(src_ip, dst_ip, IpProtocol::Tcp.to_u8(), total_len as u16);
    for chunk in out.chunks_exact(2) {
        sum += u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
    }
    if out.len() % 2 == 1 {
        sum += (out[out.len() - 1] as u32) << 8;
    }
    let csum = finalize_checksum(sum);
    out[16..18].copy_from_slice(&csum.to_be_bytes());
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TcpFlowKey {
    guest_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
}

#[derive(Debug)]
struct TcpConn {
    conn_id: u64,
    guest_ip: Ipv4Addr,
    guest_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,

    guest_next: u32,
    guest_isn: u32,

    our_isn: u32,
    our_send_next: u32,

    syn_acked: bool,

    // Remote->guest unacked data.
    send_buf: Vec<u8>,
    send_buf_seq: u32,

    fin_sent: bool,
    fin_seq: u32,
    fin_acked: bool,
    guest_fin_received: bool,
}

impl TcpConn {
    fn new(
        conn_id: u64,
        guest_ip: Ipv4Addr,
        guest_port: u16,
        remote_ip: Ipv4Addr,
        remote_port: u16,
        guest_isn: u32,
        our_isn: u32,
    ) -> Self {
        Self {
            conn_id,
            guest_ip,
            guest_port,
            remote_ip,
            remote_port,
            guest_next: guest_isn.wrapping_add(1),
            guest_isn,
            our_isn,
            our_send_next: our_isn.wrapping_add(1),
            syn_acked: false,
            send_buf: Vec::new(),
            send_buf_seq: our_isn.wrapping_add(1),
            fin_sent: false,
            fin_seq: 0,
            fin_acked: false,
            guest_fin_received: false,
        }
    }

    fn on_guest_ack(&mut self, ack: u32) {
        if !self.syn_acked && ack.wrapping_sub(self.our_isn) >= 1 {
            self.syn_acked = true;
        }

        if !self.send_buf.is_empty() {
            let start = self.send_buf_seq;
            if ack.wrapping_sub(start) as usize <= self.send_buf.len() {
                let acked = ack.wrapping_sub(start) as usize;
                self.send_buf.drain(0..acked);
                self.send_buf_seq = ack;
            }
        }

        if self.fin_sent && !self.fin_acked && ack.wrapping_sub(self.fin_seq) >= 1 {
            self.fin_acked = true;
        }
    }

    fn should_remove(&self) -> bool {
        // Remove when both sides have exchanged FINs and the guest ACKed our FIN.
        self.guest_fin_received && self.fin_sent && self.fin_acked
    }
}

#[derive(Debug)]
pub struct TcpNat {
    next_conn_id: u64,
    conns: HashMap<TcpFlowKey, TcpConn>,
    by_id: HashMap<u64, TcpFlowKey>,
}

impl TcpNat {
    pub fn new() -> Self {
        Self {
            next_conn_id: 1,
            conns: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    pub fn handle_outbound(
        &mut self,
        ip: Ipv4Packet<'_>,
        guest_mac: MacAddr,
        gateway_mac: MacAddr,
        guest_ip: Ipv4Addr,
    ) -> (Vec<Vec<u8>>, Vec<ProxyAction>, u64, u64) {
        let seg = match TcpSegment::parse(ip.payload) {
            Some(s) => s,
            None => return (Vec::new(), Vec::new(), 0, 0),
        };

        if ip.src != guest_ip {
            return (Vec::new(), Vec::new(), 0, 0);
        }

        let key = TcpFlowKey {
            guest_port: seg.src_port,
            remote_ip: ip.dst,
            remote_port: seg.dst_port,
        };

        let mut frames = Vec::new();
        let mut actions = Vec::new();
        let mut opened = 0u64;
        let mut closed = 0u64;

        // New connection: SYN without ACK.
        if (seg.flags & FLAG_SYN) != 0 && (seg.flags & FLAG_ACK) == 0 {
            if let Some(existing) = self.conns.get(&key) {
                // Retransmitted SYN: resend SYN-ACK with the same ISNs.
                if existing.guest_isn == seg.seq {
                    frames.push(build_syn_ack(existing, guest_mac, gateway_mac));
                    return (frames, actions, 0, 0);
                }
            }

            // Replace any prior connection on this tuple.
            if let Some(old) = self.conns.remove(&key) {
                self.by_id.remove(&old.conn_id);
                closed += 1;
            }

            let conn_id = self.next_conn_id;
            self.next_conn_id += 1;
            let our_isn = isn_for_conn(conn_id);

            let conn = TcpConn::new(
                conn_id,
                guest_ip,
                seg.src_port,
                ip.dst,
                seg.dst_port,
                seg.seq,
                our_isn,
            );
            frames.push(build_syn_ack(&conn, guest_mac, gateway_mac));
            actions.push(ProxyAction::TcpConnect {
                conn_id,
                dst_ip: ip.dst,
                dst_port: seg.dst_port,
            });
            opened += 1;

            self.by_id.insert(conn_id, key);
            self.conns.insert(key, conn);
            return (frames, actions, opened, closed);
        }

        let mut remove_conn: Option<u64> = None;

        {
            let conn = match self.conns.get_mut(&key) {
                Some(c) => c,
                None => return (Vec::new(), Vec::new(), 0, 0),
            };

            // ACK updates.
            if (seg.flags & FLAG_ACK) != 0 {
                conn.on_guest_ack(seg.ack);
            }

            // RST tears everything down immediately.
            if (seg.flags & FLAG_RST) != 0 {
                let conn_id = conn.conn_id;
                actions.push(ProxyAction::TcpClose { conn_id });
                remove_conn = Some(conn_id);
            } else {
                // Handle inbound payload, with basic duplicate suppression.
                let mut new_payload: &[u8] = &[];
                let mut payload_was_duplicate = false;
                if !seg.payload.is_empty() {
                    let expected = conn.guest_next;
                    if seg.seq == expected {
                        new_payload = seg.payload;
                    } else if seg.seq < expected {
                        // Duplicate/overlapping segment. Forward only the unseen tail.
                        let end_seq = seg.seq.wrapping_add(seg.payload.len() as u32);
                        if end_seq > expected {
                            let offset = expected.wrapping_sub(seg.seq) as usize;
                            new_payload = &seg.payload[offset..];
                        } else {
                            payload_was_duplicate = true;
                        }
                    } else {
                        // Out-of-order: ACK what we have and drop.
                        frames.push(build_ack(conn, guest_mac, gateway_mac));
                        return (frames, actions, opened, closed);
                    }
                }

                if !new_payload.is_empty() {
                    conn.guest_next = conn.guest_next.wrapping_add(new_payload.len() as u32);
                    actions.push(ProxyAction::TcpSend {
                        conn_id: conn.conn_id,
                        data: new_payload.to_vec(),
                    });
                    frames.push(build_ack(conn, guest_mac, gateway_mac));
                } else if payload_was_duplicate {
                    // Retransmit tolerance: re-ACK already-consumed bytes without forwarding.
                    frames.push(build_ack(conn, guest_mac, gateway_mac));
                }

                // FIN processing (after payload).
                if (seg.flags & FLAG_FIN) != 0 {
                    let fin_seq = seg.seq.wrapping_add(seg.payload.len() as u32);
                    if fin_seq == conn.guest_next {
                        conn.guest_next = conn.guest_next.wrapping_add(1);
                        conn.guest_fin_received = true;
                    }

                    // ACK the FIN regardless (idempotent).
                    frames.push(build_ack(conn, guest_mac, gateway_mac));

                    // Close the host-side connection.
                    actions.push(ProxyAction::TcpClose { conn_id: conn.conn_id });

                    // Also close the guest-facing side immediately.
                    if !conn.fin_sent {
                        frames.push(build_fin(conn, guest_mac, gateway_mac));
                    }
                }

                if conn.should_remove() {
                    remove_conn = Some(conn.conn_id);
                }
            }
        }

        if let Some(conn_id) = remove_conn {
            self.conns.remove(&key);
            self.by_id.remove(&conn_id);
            closed += 1;
        }

        (frames, actions, opened, closed)
    }

    pub fn on_proxy_connected(&mut self, _conn_id: u64) {
        // Currently unused: we optimistically ACK SYNs immediately.
    }

    pub fn on_proxy_connect_failed(
        &mut self,
        conn_id: u64,
        gateway_mac: MacAddr,
        guest_mac: Option<MacAddr>,
    ) -> (Option<Vec<u8>>, u64) {
        let key = match self.by_id.remove(&conn_id) {
            Some(k) => k,
            None => return (None, 0),
        };
        let conn = match self.conns.remove(&key) {
            Some(c) => c,
            None => return (None, 0),
        };
        let guest_mac = match guest_mac {
            Some(m) => m,
            None => return (None, 1),
        };

        let rst = build_tcp_segment(
            conn.remote_port,
            conn.guest_port,
            conn.our_isn.wrapping_add(1),
            conn.guest_next,
            FLAG_RST | FLAG_ACK,
            65535,
            &[],
            conn.remote_ip,
            conn.guest_ip,
        );
        let ip = Ipv4PacketBuilder::new()
            .src(conn.remote_ip)
            .dst(conn.guest_ip)
            .protocol(IpProtocol::Tcp)
            .payload(rst)
            .build();
        (
            Some(build_ethernet_frame(guest_mac, gateway_mac, EtherType::Ipv4, &ip)),
            1,
        )
    }

    pub fn on_proxy_data(
        &mut self,
        conn_id: u64,
        data: &[u8],
        guest_mac: MacAddr,
        gateway_mac: MacAddr,
    ) -> Vec<Vec<u8>> {
        let key = match self.by_id.get(&conn_id) {
            Some(k) => *k,
            None => return Vec::new(),
        };
        let conn = match self.conns.get_mut(&key) {
            Some(c) => c,
            None => return Vec::new(),
        };
        if data.is_empty() {
            return Vec::new();
        }

        if conn.send_buf.is_empty() {
            conn.send_buf_seq = conn.our_send_next;
        }
        conn.send_buf.extend_from_slice(data);

        let seg = build_tcp_segment(
            conn.remote_port,
            conn.guest_port,
            conn.our_send_next,
            conn.guest_next,
            FLAG_ACK | FLAG_PSH,
            65535,
            data,
            conn.remote_ip,
            conn.guest_ip,
        );
        conn.our_send_next = conn.our_send_next.wrapping_add(data.len() as u32);

        let ip = Ipv4PacketBuilder::new()
            .src(conn.remote_ip)
            .dst(conn.guest_ip)
            .protocol(IpProtocol::Tcp)
            .payload(seg)
            .build();
        vec![build_ethernet_frame(guest_mac, gateway_mac, EtherType::Ipv4, &ip)]
    }

    pub fn on_proxy_closed(
        &mut self,
        conn_id: u64,
        guest_mac: MacAddr,
        gateway_mac: MacAddr,
    ) -> (Vec<Vec<u8>>, u64) {
        let key = match self.by_id.get(&conn_id) {
            Some(k) => *k,
            None => return (Vec::new(), 0),
        };
        let conn = match self.conns.get_mut(&key) {
            Some(c) => c,
            None => return (Vec::new(), 0),
        };
        if conn.fin_sent {
            return (Vec::new(), 0);
        }
        let frame = build_fin(conn, guest_mac, gateway_mac);
        // We keep the connection until the guest ACKs our FIN (so no "closed" yet).
        (vec![frame], 0)
    }
}

fn build_syn_ack(conn: &TcpConn, guest_mac: MacAddr, gateway_mac: MacAddr) -> Vec<u8> {
    let seg = build_tcp_segment(
        conn.remote_port,
        conn.guest_port,
        conn.our_isn,
        conn.guest_next,
        FLAG_SYN | FLAG_ACK,
        65535,
        &[],
        conn.remote_ip,
        conn.guest_ip,
    );
    let ip = Ipv4PacketBuilder::new()
        .src(conn.remote_ip)
        .dst(conn.guest_ip)
        .protocol(IpProtocol::Tcp)
        .payload(seg)
        .build();
    build_ethernet_frame(guest_mac, gateway_mac, EtherType::Ipv4, &ip)
}

fn build_ack(conn: &TcpConn, guest_mac: MacAddr, gateway_mac: MacAddr) -> Vec<u8> {
    let seg = build_tcp_segment(
        conn.remote_port,
        conn.guest_port,
        conn.our_send_next,
        conn.guest_next,
        FLAG_ACK,
        65535,
        &[],
        conn.remote_ip,
        conn.guest_ip,
    );
    let ip = Ipv4PacketBuilder::new()
        .src(conn.remote_ip)
        .dst(conn.guest_ip)
        .protocol(IpProtocol::Tcp)
        .payload(seg)
        .build();
    build_ethernet_frame(guest_mac, gateway_mac, EtherType::Ipv4, &ip)
}

fn build_fin(conn: &mut TcpConn, guest_mac: MacAddr, gateway_mac: MacAddr) -> Vec<u8> {
    let fin_seq = conn.our_send_next;
    let seg = build_tcp_segment(
        conn.remote_port,
        conn.guest_port,
        fin_seq,
        conn.guest_next,
        FLAG_FIN | FLAG_ACK,
        65535,
        &[],
        conn.remote_ip,
        conn.guest_ip,
    );
    conn.fin_sent = true;
    conn.fin_seq = fin_seq;
    conn.our_send_next = conn.our_send_next.wrapping_add(1);

    let ip = Ipv4PacketBuilder::new()
        .src(conn.remote_ip)
        .dst(conn.guest_ip)
        .protocol(IpProtocol::Tcp)
        .payload(seg)
        .build();
    build_ethernet_frame(guest_mac, gateway_mac, EtherType::Ipv4, &ip)
}

fn isn_for_conn(conn_id: u64) -> u32 {
    // A small deterministic LCG to get "random enough" ISNs without pulling a RNG dependency.
    let mut x = conn_id as u32 ^ 0xA5A5_5A5A;
    x = x.wrapping_mul(1103515245).wrapping_add(12345);
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::net::stack::ethernet::{EthernetFrame, MacAddr};

    fn build_ipv4_tcp(
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        seq: u32,
        ack: u32,
        flags: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let tcp = build_tcp_segment(src_port, dst_port, seq, ack, flags, 65535, payload, src_ip, dst_ip);
        Ipv4PacketBuilder::new()
            .src(src_ip)
            .dst(dst_ip)
            .protocol(IpProtocol::Tcp)
            .payload(tcp)
            .build()
    }

    #[test]
    fn tcp_nat_syn_synack_payload_fin() {
        let mut nat = TcpNat::new();
        let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
        let remote_ip = Ipv4Addr::new(93, 184, 216, 34);
        let guest_mac = MacAddr::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        let gateway_mac = MacAddr::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);

        // SYN
        let syn_seq = 1000u32;
        let syn_ip = build_ipv4_tcp(guest_ip, remote_ip, 12345, 80, syn_seq, 0, FLAG_SYN, &[]);
        let syn_pkt = Ipv4Packet::parse(&syn_ip).unwrap();
        let (frames, actions, opened, _) =
            nat.handle_outbound(syn_pkt, guest_mac, gateway_mac, guest_ip);
        assert_eq!(opened, 1);
        assert_eq!(actions.len(), 1);
        let ProxyAction::TcpConnect { conn_id, dst_ip, dst_port } = actions[0] else {
            panic!("expected connect action");
        };
        assert_eq!(dst_ip, remote_ip);
        assert_eq!(dst_port, 80);
        assert_eq!(frames.len(), 1);

        let eth = EthernetFrame::parse(&frames[0]).unwrap();
        assert_eq!(eth.dst, guest_mac);
        let ip = Ipv4Packet::parse(eth.payload).unwrap();
        assert_eq!(ip.src, remote_ip);
        assert_eq!(ip.dst, guest_ip);
        let synack = TcpSegment::parse(ip.payload).unwrap();
        assert_eq!(synack.flags & (FLAG_SYN | FLAG_ACK), FLAG_SYN | FLAG_ACK);
        assert_eq!(synack.ack, syn_seq + 1);
        let our_isn = synack.seq;

        // ACK completing handshake.
        let ack_ip = build_ipv4_tcp(
            guest_ip,
            remote_ip,
            12345,
            80,
            syn_seq + 1,
            our_isn + 1,
            FLAG_ACK,
            &[],
        );
        let ack_pkt = Ipv4Packet::parse(&ack_ip).unwrap();
        let (frames, actions, _, _) =
            nat.handle_outbound(ack_pkt, guest_mac, gateway_mac, guest_ip);
        assert!(frames.is_empty());
        assert!(actions.is_empty());

        // Send payload.
        let payload = b"hello";
        let data_ip = build_ipv4_tcp(
            guest_ip,
            remote_ip,
            12345,
            80,
            syn_seq + 1,
            our_isn + 1,
            FLAG_ACK | FLAG_PSH,
            payload,
        );
        let data_pkt = Ipv4Packet::parse(&data_ip).unwrap();
        let (frames, actions, _, _) =
            nat.handle_outbound(data_pkt, guest_mac, gateway_mac, guest_ip);
        assert_eq!(actions.len(), 1);
        let ProxyAction::TcpSend { conn_id: send_id, data } = &actions[0] else {
            panic!("expected send action");
        };
        assert_eq!(*send_id, conn_id);
        assert_eq!(data, payload);
        assert_eq!(frames.len(), 1, "should ACK payload");

        // FIN.
        let fin_ip = build_ipv4_tcp(
            guest_ip,
            remote_ip,
            12345,
            80,
            syn_seq + 1 + payload.len() as u32,
            our_isn + 1,
            FLAG_ACK | FLAG_FIN,
            &[],
        );
        let fin_pkt = Ipv4Packet::parse(&fin_ip).unwrap();
        let (frames, actions, _, _) =
            nat.handle_outbound(fin_pkt, guest_mac, gateway_mac, guest_ip);
        assert!(!actions.is_empty());
        assert!(matches!(actions[0], ProxyAction::TcpClose { .. }));
        assert_eq!(frames.len(), 2, "ACK + FIN");
    }
}
