use core::net::Ipv4Addr;

use aero_net_e1000::{E1000Device, ICR_TXDW};
use memory::MemoryBus;
use nt_packetlib::io::net::packet::checksum::{internet_checksum, transport_checksum_ipv4};

const REG_ICR: u32 = 0x00C0;
const REG_IMS: u32 = 0x00D0;
const REG_TCTL: u32 = 0x0400;
const REG_TDBAL: u32 = 0x3800;
const REG_TDBAH: u32 = 0x3804;
const REG_TDLEN: u32 = 0x3808;
const REG_TDH: u32 = 0x3810;
const REG_TDT: u32 = 0x3818;

const TCTL_EN: u32 = 1 << 1;

const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_RS: u8 = 1 << 3;
const TXD_CMD_DEXT: u8 = 1 << 5;
const TXD_CMD_TSE: u8 = 1 << 7;

const TXD_POPTS_IXSM: u8 = 0x01;
const TXD_POPTS_TXSM: u8 = 0x02;

struct TestDma {
    mem: Vec<u8>,
}

impl TestDma {
    fn new(size: usize) -> Self {
        Self {
            mem: vec![0u8; size],
        }
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) {
        let addr = addr as usize;
        self.mem[addr..addr + bytes.len()].copy_from_slice(bytes);
    }
}

impl MemoryBus for TestDma {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let addr = paddr as usize;
        buf.copy_from_slice(&self.mem[addr..addr + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let addr = paddr as usize;
        self.mem[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

fn build_ipv4_tcp_frame(payload_len: usize) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + 20 + 20 + payload_len);

    // Ethernet header.
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src
    frame.extend_from_slice(&0x0800u16.to_be_bytes()); // IPv4 ethertype

    let total_len = (20 + 20 + payload_len) as u16;
    // IPv4 header (checksum filled by offload).
    frame.extend_from_slice(&[
        0x45,
        0x00,
        (total_len >> 8) as u8,
        total_len as u8,
        0x12,
        0x34,
        0x00,
        0x00,
        64,
        6,
        0x00,
        0x00,
        192,
        168,
        0,
        2,
        192,
        168,
        0,
        1,
    ]);

    // TCP header (checksum filled by offload).
    frame.extend_from_slice(&1234u16.to_be_bytes()); // src port
    frame.extend_from_slice(&80u16.to_be_bytes()); // dst port
    frame.extend_from_slice(&0x01020304u32.to_be_bytes()); // seq
    frame.extend_from_slice(&0u32.to_be_bytes()); // ack
    frame.push(0x50); // data offset 5
    frame.push(0x18); // PSH+ACK
    frame.extend_from_slice(&4096u16.to_be_bytes()); // window
    frame.extend_from_slice(&0u16.to_be_bytes()); // checksum
    frame.extend_from_slice(&0u16.to_be_bytes()); // urg ptr

    for i in 0..payload_len {
        frame.push((i & 0xFF) as u8);
    }

    frame
}

fn build_ipv4_udp_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + 20 + 8 + payload.len());

    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());

    let total_len = (20 + 8 + payload.len()) as u16;
    frame.extend_from_slice(&[
        0x45,
        0x00,
        (total_len >> 8) as u8,
        total_len as u8,
        0x00,
        0x10,
        0x00,
        0x00,
        64,
        17,
        0x00,
        0x00,
        10,
        0,
        0,
        1,
        10,
        0,
        0,
        2,
    ]);

    let udp_len = (8 + payload.len()) as u16;
    frame.extend_from_slice(&4000u16.to_be_bytes());
    frame.extend_from_slice(&4001u16.to_be_bytes());
    frame.extend_from_slice(&udp_len.to_be_bytes());
    frame.extend_from_slice(&0u16.to_be_bytes()); // checksum

    frame.extend_from_slice(payload);
    frame
}

fn write_tx_ctx_desc(
    dma: &mut TestDma,
    addr: u64,
    frame_len: usize,
    mss: u16,
    hdr_len: u8,
    tcp: bool,
) {
    let ipcss = 14u8;
    let ipcso = ipcss + 10;
    let ipcse = ipcss as u16 + 20 - 1;
    let tucss = (14 + 20) as u8;
    let tucso = tucss + if tcp { 16 } else { 6 };
    let tucse = (frame_len - 1) as u16;

    dma.write(addr, &[ipcss, ipcso]);
    dma.write(addr + 2, &ipcse.to_le_bytes());
    dma.write(addr + 4, &[tucss, tucso]);
    dma.write(addr + 6, &tucse.to_le_bytes());

    // cmd_len (length=0, typ=CTXT, cmd=DEXT)
    dma.write(addr + 8, &0u16.to_le_bytes());
    dma.write(addr + 10, &[0x20]); // DTYP=2 (context)
    dma.write(addr + 11, &[TXD_CMD_DEXT]);

    dma.write(addr + 12, &mss.to_le_bytes());
    dma.write(addr + 14, &[hdr_len, 0]);
}

fn write_tx_data_desc(dma: &mut TestDma, addr: u64, buf_addr: u64, len: u16, cmd: u8, popts: u8) {
    dma.write(addr, &buf_addr.to_le_bytes());
    dma.write(addr + 8, &len.to_le_bytes());
    dma.write(addr + 10, &[0x30]); // DTYP=3 (data)
    dma.write(addr + 11, &[cmd]);
    dma.write(addr + 12, &[0]); // status
    dma.write(addr + 13, &[popts]);
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

#[test]
fn tso_context_descriptor_segments_and_inserts_checksums() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4);
    let mut dma = TestDma::new(0x80_000);

    dev.mmio_write_u32_reg(REG_IMS, ICR_TXDW);

    dev.mmio_write_u32_reg(REG_TDBAL, 0x1000);
    dev.mmio_write_u32_reg(REG_TDBAH, 0);
    dev.mmio_write_u32_reg(REG_TDLEN, 16 * 8);
    dev.mmio_write_u32_reg(REG_TDH, 0);
    dev.mmio_write_u32_reg(REG_TDT, 0);
    dev.mmio_write_u32_reg(REG_TCTL, TCTL_EN);

    let frame = build_ipv4_tcp_frame(4000);
    dma.write(0x4000, &frame);

    let hdr_len = (14 + 20 + 20) as u8;
    write_tx_ctx_desc(&mut dma, 0x1000, frame.len(), 1460, hdr_len, true);
    write_tx_data_desc(
        &mut dma,
        0x1010,
        0x4000,
        frame.len() as u16,
        TXD_CMD_DEXT | TXD_CMD_TSE | TXD_CMD_EOP | TXD_CMD_RS,
        TXD_POPTS_IXSM | TXD_POPTS_TXSM,
    );

    dev.mmio_write_u32_reg(REG_TDT, 2);
    dev.poll(&mut dma);

    assert_ne!(
        dma.mem[0x1000 + 12] & 0x01,
        0,
        "context descriptor should be marked DD"
    );
    assert_ne!(
        dma.mem[0x1010 + 12] & 0x01,
        0,
        "data descriptor should be marked DD"
    );

    let mut out = Vec::new();
    while let Some(frame) = dev.pop_tx_frame() {
        out.push(frame);
    }

    assert_eq!(out.len(), 3);

    let src = Ipv4Addr::new(192, 168, 0, 2);
    let dst = Ipv4Addr::new(192, 168, 0, 1);
    let base_seq = 0x01020304u32;

    for (idx, seg) in out.iter().enumerate() {
        let ip_off = 14usize;
        let tcp_off = 14 + 20;

        let total_len = u16::from_be_bytes([seg[ip_off + 2], seg[ip_off + 3]]) as usize;
        assert_eq!(seg.len(), ip_off + total_len);

        let expected_payload = if idx < 2 { 1460 } else { 4000 - 2 * 1460 };
        assert_eq!(total_len, 20 + 20 + expected_payload);

        let seq = u32::from_be_bytes([
            seg[tcp_off + 4],
            seg[tcp_off + 5],
            seg[tcp_off + 6],
            seg[tcp_off + 7],
        ]);
        assert_eq!(seq, base_seq + (idx as u32) * 1460);

        let psh_set = (seg[tcp_off + 13] & 0x08) != 0;
        assert_eq!(psh_set, idx == out.len() - 1);

        assert_eq!(internet_checksum(&seg[ip_off..ip_off + 20]), 0);
        assert_eq!(transport_checksum_ipv4(src, dst, 6, &seg[tcp_off..]), 0);
    }

    assert!(dev.irq_level());
    let icr = dev.mmio_read_u32(REG_ICR);
    assert_eq!(icr & ICR_TXDW, ICR_TXDW);
    assert!(!dev.irq_level());
}

#[test]
fn checksum_offload_udp_inserts_checksums() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4);
    let mut dma = TestDma::new(0x40_000);

    dev.mmio_write_u32_reg(REG_IMS, ICR_TXDW);

    dev.mmio_write_u32_reg(REG_TDBAL, 0x2000);
    dev.mmio_write_u32_reg(REG_TDBAH, 0);
    dev.mmio_write_u32_reg(REG_TDLEN, 16 * 8);
    dev.mmio_write_u32_reg(REG_TDH, 0);
    dev.mmio_write_u32_reg(REG_TDT, 0);
    dev.mmio_write_u32_reg(REG_TCTL, TCTL_EN);

    let payload = b"hello world";
    let frame = build_ipv4_udp_frame(payload);
    dma.write(0x3000, &frame);

    let hdr_len = (14 + 20 + 8) as u8;
    write_tx_ctx_desc(&mut dma, 0x2000, frame.len(), 0, hdr_len, false);
    write_tx_data_desc(
        &mut dma,
        0x2010,
        0x3000,
        frame.len() as u16,
        TXD_CMD_DEXT | TXD_CMD_EOP | TXD_CMD_RS,
        TXD_POPTS_IXSM | TXD_POPTS_TXSM,
    );

    dev.mmio_write_u32_reg(REG_TDT, 2);
    dev.poll(&mut dma);

    assert_ne!(
        dma.mem[0x2000 + 12] & 0x01,
        0,
        "context descriptor should be marked DD"
    );
    assert_ne!(
        dma.mem[0x2010 + 12] & 0x01,
        0,
        "data descriptor should be marked DD"
    );

    let out = dev.pop_tx_frame().expect("frame");
    assert!(dev.pop_tx_frame().is_none());

    let ip_off = 14usize;
    let udp_off = 14 + 20;

    let src = Ipv4Addr::new(10, 0, 0, 1);
    let dst = Ipv4Addr::new(10, 0, 0, 2);

    assert_eq!(internet_checksum(&out[ip_off..ip_off + 20]), 0);
    assert_eq!(transport_checksum_ipv4(src, dst, 17, &out[udp_off..]), 0);

    assert_eq!(&out[hdr_len as usize..], payload);
}
