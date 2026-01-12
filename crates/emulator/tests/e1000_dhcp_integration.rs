#![cfg(not(target_arch = "wasm32"))]

use std::collections::VecDeque;
use std::net::Ipv4Addr;

use aero_net_e1000::E1000Device;
use aero_net_pump::tick_e1000;
use aero_net_stack::packet::{
    DhcpMessage, DhcpMessageType, EtherType, EthernetFrame, EthernetFrameBuilder, Ipv4Packet,
    Ipv4PacketBuilder, Ipv4Protocol, MacAddr, UdpPacket, UdpPacketBuilder,
};
use emulator::io::net::stack::{Action, NetStackBackend, NetworkStack, StackConfig};
use emulator::io::net::NetworkBackend;
use memory::MemoryBus;

const TX_RING_BASE: u64 = 0x1000;
const RX_RING_BASE: u64 = 0x2000;

const RX_BUF0: u64 = 0x3000;
const RX_BUF1: u64 = 0x3400;
const RX_BUF2: u64 = 0x3800;
const RX_BUF3: u64 = 0x3C00;

const TX_BUF0: u64 = 0x5000;
const TX_BUF1: u64 = 0x6000;

const DESC_LEN: u64 = 16;

const RXD_STAT_DD: u8 = 1 << 0;
const RXD_STAT_EOP: u8 = 1 << 1;

struct TestMem {
    mem: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) {
        let addr = addr as usize;
        self.mem[addr..addr + bytes.len()].copy_from_slice(bytes);
    }

    fn read_vec(&self, addr: u64, len: usize) -> Vec<u8> {
        let addr = addr as usize;
        self.mem[addr..addr + len].to_vec()
    }
}

impl MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let addr = paddr as usize;
        buf.copy_from_slice(&self.mem[addr..addr + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let addr = paddr as usize;
        self.mem[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

fn write_u64_le(mem: &mut TestMem, addr: u64, v: u64) {
    mem.write(addr, &v.to_le_bytes());
}

/// Minimal legacy TX descriptor layout (16 bytes).
fn write_tx_desc(mem: &mut TestMem, addr: u64, buf_addr: u64, len: u16, cmd: u8, status: u8) {
    write_u64_le(mem, addr, buf_addr);
    mem.write(addr + 8, &len.to_le_bytes());
    mem.write(addr + 10, &[0u8]); // cso
    mem.write(addr + 11, &[cmd]);
    mem.write(addr + 12, &[status]);
    mem.write(addr + 13, &[0u8]); // css
    mem.write(addr + 14, &0u16.to_le_bytes()); // special
}

/// Minimal legacy RX descriptor layout (16 bytes).
fn write_rx_desc(mem: &mut TestMem, addr: u64, buf_addr: u64, status: u8) {
    write_u64_le(mem, addr, buf_addr);
    mem.write(addr + 8, &0u16.to_le_bytes()); // length
    mem.write(addr + 10, &0u16.to_le_bytes()); // checksum
    mem.write(addr + 12, &[status]);
    mem.write(addr + 13, &[0u8]); // errors
    mem.write(addr + 14, &0u16.to_le_bytes()); // special
}

#[derive(Debug, Clone, Copy)]
struct RxDesc {
    buffer_addr: u64,
    length: u16,
    status: u8,
    errors: u8,
}

fn read_rx_desc(mem: &mut TestMem, addr: u64) -> RxDesc {
    let mut buf = [0u8; DESC_LEN as usize];
    mem.read_physical(addr, &mut buf);
    RxDesc {
        buffer_addr: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        length: u16::from_le_bytes(buf[8..10].try_into().unwrap()),
        status: buf[12],
        errors: buf[13],
    }
}

fn configure_tx_ring(dev: &mut E1000Device, desc_base: u32, desc_count: u32) {
    dev.mmio_write_u32_reg(0x3800, desc_base); // TDBAL
    dev.mmio_write_u32_reg(0x3804, 0); // TDBAH
    dev.mmio_write_u32_reg(0x3808, desc_count * DESC_LEN as u32); // TDLEN
    dev.mmio_write_u32_reg(0x3810, 0); // TDH
    dev.mmio_write_u32_reg(0x3818, 0); // TDT
    dev.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN
}

fn configure_rx_ring(dev: &mut E1000Device, desc_base: u32, desc_count: u32, tail: u32) {
    dev.mmio_write_u32_reg(0x2800, desc_base); // RDBAL
    dev.mmio_write_u32_reg(0x2804, 0); // RDBAH
    dev.mmio_write_u32_reg(0x2808, desc_count * DESC_LEN as u32); // RDLEN
    dev.mmio_write_u32_reg(0x2810, 0); // RDH
    dev.mmio_write_u32_reg(0x2818, tail); // RDT
    dev.mmio_write_u32_reg(0x0100, 1 << 1); // RCTL.EN (defaults to 2048 buffer)
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
    let udp = UdpPacketBuilder {
        src_port,
        dst_port,
        payload,
    }
    .build_vec(src_ip, dst_ip)
    .expect("build UDP packet");

    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 1,
        flags_fragment: 0x4000, // DF
        ttl: 64,
        protocol: Ipv4Protocol::UDP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &udp,
    }
    .build_vec()
    .expect("build IPv4 packet");

    EthernetFrameBuilder {
        dest_mac: dst_mac,
        src_mac,
        ethertype: EtherType::IPV4,
        payload: &ip,
    }
    .build_vec()
    .expect("build Ethernet frame")
}

fn parse_dhcp_from_frame(frame: &[u8]) -> DhcpMessage {
    let eth = EthernetFrame::parse(frame).expect("parse Ethernet");
    assert_eq!(eth.ethertype(), EtherType::IPV4, "unexpected ethertype");
    let ip = Ipv4Packet::parse(eth.payload()).expect("parse IPv4");
    assert_eq!(ip.protocol(), Ipv4Protocol::UDP, "expected UDP");
    let udp = UdpPacket::parse(ip.payload()).expect("parse UDP");
    assert_eq!(udp.src_port(), 67, "expected DHCP server port");
    assert_eq!(udp.dst_port(), 68, "expected DHCP client port");
    DhcpMessage::parse(udp.payload()).expect("parse DHCP")
}

fn build_dhcp_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
    // BOOTP fixed header (236) + magic cookie (4).
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC len
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]); // magic cookie

    // DHCP options: message type = DISCOVER.
    out.extend_from_slice(&[53, 1, 1]);
    out.push(255); // end
    out
}

fn build_dhcp_request(xid: u32, mac: MacAddr, requested_ip: Ipv4Addr, server_id: Ipv4Addr) -> Vec<u8> {
    // BOOTP fixed header (236) + magic cookie (4).
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC len
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]); // magic cookie

    // DHCP options:
    // - message type = REQUEST
    // - requested IP
    // - server identifier
    out.extend_from_slice(&[53, 1, 3]);
    out.extend_from_slice(&[50, 4]);
    out.extend_from_slice(&requested_ip.octets());
    out.extend_from_slice(&[54, 4]);
    out.extend_from_slice(&server_id.octets());
    out.push(255); // end
    out
}

/// Deterministic [`NetStackBackend`] wrapper for pump tests.
///
/// `tick_e1000` calls [`NetworkBackend::transmit`], which in the stock backend uses a wall clock
/// (`Instant::elapsed`) to generate `now_ms` timestamps. For deterministic tests we instead forward
/// `transmit()` to [`NetStackBackend::transmit_at`] with an explicit monotonic counter.
struct DeterministicNetStackBackend {
    inner: NetStackBackend,
    now_ms: u64,
    pending_frames: VecDeque<Vec<u8>>,
    pending_actions: VecDeque<Action>,
}

impl DeterministicNetStackBackend {
    fn new(cfg: StackConfig) -> Self {
        Self {
            inner: NetStackBackend::new(cfg),
            now_ms: 0,
            pending_frames: VecDeque::new(),
            pending_actions: VecDeque::new(),
        }
    }

    fn stack(&self) -> &NetworkStack {
        self.inner.stack()
    }

    fn drain_actions(&mut self) -> Vec<Action> {
        self.pending_actions.drain(..).collect()
    }
}

impl NetworkBackend for DeterministicNetStackBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.inner.transmit_at(frame, self.now_ms);
        self.now_ms = self.now_ms.saturating_add(1);

        for f in self.inner.drain_frames() {
            self.pending_frames.push_back(f);
        }
        for a in self.inner.drain_actions() {
            self.pending_actions.push_back(a);
        }
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.pending_frames.pop_front()
    }
}

fn pump_until_rx_desc_done(
    nic: &mut E1000Device,
    mem: &mut TestMem,
    backend: &mut DeterministicNetStackBackend,
    rx_desc_index: u64,
    max_ticks: usize,
) {
    for _ in 0..max_ticks {
        tick_e1000(nic, mem, backend, 8, 8);

        let desc_addr = RX_RING_BASE + rx_desc_index * DESC_LEN;
        let desc = read_rx_desc(mem, desc_addr);
        if (desc.status & (RXD_STAT_DD | RXD_STAT_EOP)) == (RXD_STAT_DD | RXD_STAT_EOP) {
            return;
        }
    }
    panic!("RX descriptor {rx_desc_index} did not complete after {max_ticks} ticks");
}

#[test]
fn dhcp_handshake_end_to_end_e1000_dma_to_netstack() {
    let mut mem = TestMem::new(0x80_000);

    let guest_mac_bytes = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let guest_mac = MacAddr(guest_mac_bytes);

    // --- Configure E1000 (register-only writes) ---
    let mut nic = E1000Device::new(guest_mac_bytes);
    nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

    configure_tx_ring(&mut nic, TX_RING_BASE as u32, 4);
    // RX rings keep one descriptor unused to distinguish full/empty conditions.
    // desc_count=4, tail=3 gives us 3 usable RX descriptors (indices 0..2).
    configure_rx_ring(&mut nic, RX_RING_BASE as u32, 4, 3);

    let rx_bufs = [RX_BUF0, RX_BUF1, RX_BUF2, RX_BUF3];
    for (i, buf) in rx_bufs.into_iter().enumerate() {
        write_rx_desc(&mut mem, RX_RING_BASE + (i as u64) * DESC_LEN, buf, 0);
    }

    // --- Configure deterministic host network stack backend ---
    let mut backend = DeterministicNetStackBackend::new(StackConfig::default());

    // --- DHCPDISCOVER -> DHCPOFFER ---
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
    assert!(
        (aero_net_e1000::MIN_L2_FRAME_LEN..=aero_net_e1000::MAX_L2_FRAME_LEN)
            .contains(&discover_frame.len()),
        "discover frame length out of bounds: {}",
        discover_frame.len()
    );

    mem.write(TX_BUF0, &discover_frame);
    write_tx_desc(
        &mut mem,
        TX_RING_BASE + 0 * DESC_LEN,
        TX_BUF0,
        discover_frame.len() as u16,
        0b0000_1001, // EOP|RS
        0,
    );
    nic.mmio_write_u32_reg(0x3818, 1); // TDT

    pump_until_rx_desc_done(&mut nic, &mut mem, &mut backend, 0, 32);
    assert!(
        backend.drain_actions().is_empty(),
        "unexpected host actions during DHCPDISCOVER"
    );

    let offer_desc = read_rx_desc(&mut mem, RX_RING_BASE + 0 * DESC_LEN);
    assert_eq!(
        offer_desc.status & (RXD_STAT_DD | RXD_STAT_EOP),
        RXD_STAT_DD | RXD_STAT_EOP,
        "offer RX descriptor missing DD/EOP"
    );
    assert_eq!(offer_desc.errors, 0, "offer RX descriptor has errors");
    let offer_frame = mem.read_vec(offer_desc.buffer_addr, offer_desc.length as usize);
    let offer_msg = parse_dhcp_from_frame(&offer_frame);
    assert_eq!(
        offer_msg.message_type,
        DhcpMessageType::Offer,
        "expected DHCPOFFER"
    );
    assert_eq!(offer_msg.transaction_id, xid, "offer XID mismatch");
    assert_eq!(
        offer_msg.your_ip,
        backend.stack().config().guest_ip,
        "offer yiaddr mismatch"
    );

    // --- DHCPREQUEST -> DHCPACK ---
    let request = build_dhcp_request(
        xid,
        guest_mac,
        backend.stack().config().guest_ip,
        backend.stack().config().gateway_ip,
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
    assert!(
        (aero_net_e1000::MIN_L2_FRAME_LEN..=aero_net_e1000::MAX_L2_FRAME_LEN)
            .contains(&request_frame.len()),
        "request frame length out of bounds: {}",
        request_frame.len()
    );

    mem.write(TX_BUF1, &request_frame);
    write_tx_desc(
        &mut mem,
        TX_RING_BASE + 1 * DESC_LEN,
        TX_BUF1,
        request_frame.len() as u16,
        0b0000_1001, // EOP|RS
        0,
    );
    nic.mmio_write_u32_reg(0x3818, 2); // TDT

    // With 4 descriptors and tail=3, only descriptor index 2 is still available. The stack emits
    // both broadcast and unicast DHCP replies, so we only expect one to be DMA-written here.
    pump_until_rx_desc_done(&mut nic, &mut mem, &mut backend, 2, 32);
    assert!(
        backend.drain_actions().is_empty(),
        "unexpected host actions during DHCPREQUEST"
    );

    let ack_desc = read_rx_desc(&mut mem, RX_RING_BASE + 2 * DESC_LEN);
    assert_eq!(
        ack_desc.status & (RXD_STAT_DD | RXD_STAT_EOP),
        RXD_STAT_DD | RXD_STAT_EOP,
        "ack RX descriptor missing DD/EOP"
    );
    assert_eq!(ack_desc.errors, 0, "ack RX descriptor has errors");
    let ack_frame = mem.read_vec(ack_desc.buffer_addr, ack_desc.length as usize);
    let ack_msg = parse_dhcp_from_frame(&ack_frame);
    assert_eq!(ack_msg.message_type, DhcpMessageType::Ack, "expected DHCPACK");
    assert_eq!(ack_msg.transaction_id, xid, "ack XID mismatch");
    assert_eq!(
        ack_msg.your_ip, offer_msg.your_ip,
        "ack yiaddr did not match offer"
    );
}

