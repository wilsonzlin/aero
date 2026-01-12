#![cfg(feature = "io-snapshot")]

use aero_net_e1000::{E1000Device, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};
use memory::MemoryBus;

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

fn write_u64_le(dma: &mut TestDma, addr: u64, v: u64) {
    dma.write(addr, &v.to_le_bytes());
}

/// Minimal legacy TX descriptor layout (16 bytes).
fn write_tx_desc(dma: &mut TestDma, addr: u64, buf_addr: u64, len: u16, cmd: u8, status: u8) {
    write_u64_le(dma, addr, buf_addr);
    dma.write(addr + 8, &len.to_le_bytes());
    dma.write(addr + 10, &[0u8]); // cso
    dma.write(addr + 11, &[cmd]);
    dma.write(addr + 12, &[status]);
    dma.write(addr + 13, &[0u8]); // css
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

fn build_frame(id: u16) -> Vec<u8> {
    let mut frame = Vec::with_capacity(MIN_L2_FRAME_LEN + 2);
    // Ethernet header (dst/src/ethertype).
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(&id.to_le_bytes());
    frame
}

fn frame_id(frame: &[u8]) -> u16 {
    u16::from_le_bytes([frame[MIN_L2_FRAME_LEN], frame[MIN_L2_FRAME_LEN + 1]])
}

#[test]
fn enqueue_rx_frame_is_bounded_and_keeps_most_recent_frames() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

    // Invalid frames are rejected and do not affect queue length.
    dev.enqueue_rx_frame(vec![0u8; MIN_L2_FRAME_LEN - 1]);
    dev.enqueue_rx_frame(vec![0u8; MAX_L2_FRAME_LEN + 1]);

    for id in 0u16..300 {
        dev.enqueue_rx_frame(build_frame(id));
    }

    let snap = dev.snapshot_state();
    assert_eq!(snap.rx_pending.len(), 256);

    let ids: Vec<u16> = snap.rx_pending.iter().map(|f| frame_id(f)).collect();
    let expected: Vec<u16> = (44u16..300).collect();
    assert_eq!(ids, expected);
}

#[test]
fn tx_out_queue_is_bounded_and_keeps_most_recent_frames() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    // Real hardware gates DMA behind PCI COMMAND.BME.
    dev.pci_config_write(0x04, 2, 0x4);
    let mut dma = TestDma::new(0x200_000);

    // Configure TX ring: 512 descriptors at 0x1000.
    dev.mmio_write_u32_reg(0x3800, 0x1000); // TDBAL
    dev.mmio_write_u32_reg(0x3804, 0); // TDBAH
    dev.mmio_write_u32_reg(0x3808, 512 * 16); // TDLEN
    dev.mmio_write_u32_reg(0x3810, 0); // TDH
    dev.mmio_write_u32_reg(0x3818, 0); // TDT
    dev.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN

    // Populate 300 TX descriptors with tiny frames (EOP|RS).
    for id in 0u16..300 {
        let frame = build_frame(id);
        let buf_addr = 0x4000 + (id as u64) * 0x20;
        dma.write(buf_addr, &frame);
        write_tx_desc(
            &mut dma,
            0x1000 + (id as u64) * 16,
            buf_addr,
            frame.len() as u16,
            0x09,
            0,
        );
    }

    dev.mmio_write_u32_reg(0x3818, 300);
    dev.poll(&mut dma);

    let mut ids = Vec::new();
    while let Some(frame) = dev.pop_tx_frame() {
        ids.push(frame_id(&frame));
    }
    assert_eq!(ids.len(), 256);
    let expected: Vec<u16> = (44u16..300).collect();
    assert_eq!(ids, expected);
}
