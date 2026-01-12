use aero_net_backend::L2TunnelBackend;
use aero_net_e1000::{E1000Device, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};
use aero_net_pump::tick_e1000_with_counts;
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

    fn read_vec(&self, addr: u64, len: usize) -> Vec<u8> {
        let addr = addr as usize;
        self.mem[addr..addr + len].to_vec()
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

/// Minimal legacy RX descriptor layout (16 bytes).
fn write_rx_desc(dma: &mut TestDma, addr: u64, buf_addr: u64, status: u8) {
    write_u64_le(dma, addr, buf_addr);
    dma.write(addr + 8, &0u16.to_le_bytes()); // length
    dma.write(addr + 10, &0u16.to_le_bytes()); // checksum
    dma.write(addr + 12, &[status]);
    dma.write(addr + 13, &[0u8]); // errors
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

fn build_test_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    // Ethernet header (dst/src/ethertype).
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn setup_tx_ring(dev: &mut E1000Device) {
    // Configure TX ring: 4 descriptors at 0x1000.
    dev.mmio_write_u32_reg(0x3800, 0x1000); // TDBAL
    dev.mmio_write_u32_reg(0x3804, 0); // TDBAH
    dev.mmio_write_u32_reg(0x3808, 4 * 16); // TDLEN
    dev.mmio_write_u32_reg(0x3810, 0); // TDH
    dev.mmio_write_u32_reg(0x3818, 0); // TDT
    dev.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN
}

fn setup_rx_ring(dev: &mut E1000Device, dma: &mut TestDma) {
    // Configure RX ring: 2 descriptors at 0x2000.
    dev.mmio_write_u32_reg(0x2800, 0x2000); // RDBAL
    dev.mmio_write_u32_reg(0x2804, 0); // RDBAH
    dev.mmio_write_u32_reg(0x2808, 2 * 16); // RDLEN
    dev.mmio_write_u32_reg(0x2810, 0); // RDH
    dev.mmio_write_u32_reg(0x2818, 1); // RDT (1 descriptor available: index 0)
    dev.mmio_write_u32_reg(0x0100, 1 << 1); // RCTL.EN (defaults to 2048 buffer)

    // Populate RX descriptors with guest buffers.
    write_rx_desc(dma, 0x2000, 0x3000, 0);
    write_rx_desc(dma, 0x2010, 0x3400, 0);
}

#[test]
fn tick_e1000_respects_tx_budget_and_drains_tx_to_backend() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    // Enable PCI bus mastering (required for DMA).
    dev.pci_config_write(0x04, 2, 0x4);
    let mut dma = TestDma::new(0x40_000);

    setup_tx_ring(&mut dev);

    // Queue two TX descriptors so the NIC publishes two frames in one poll.
    let pkt0 = build_test_frame(b"pkt0");
    let pkt1 = build_test_frame(b"pkt1");
    dma.write(0x4000, &pkt0);
    dma.write(0x4500, &pkt1);

    // EOP|RS
    write_tx_desc(&mut dma, 0x1000, 0x4000, pkt0.len() as u16, 0b0000_1001, 0);
    write_tx_desc(&mut dma, 0x1010, 0x4500, pkt1.len() as u16, 0b0000_1001, 0);

    // Advance tail to include both descriptors.
    dev.mmio_write_u32_reg(0x3818, 2);

    let mut backend = L2TunnelBackend::with_limits(8, 8, 2048);

    // Budget only allows draining one TX frame per tick.
    let counts = tick_e1000_with_counts(&mut dev, &mut dma, &mut backend, 1, 0);
    assert_eq!(counts.tx_frames, 1);
    assert_eq!(backend.drain_tx_frames(), vec![pkt0.clone()]);

    // Next tick drains the second frame (still queued in the NIC).
    let counts = tick_e1000_with_counts(&mut dev, &mut dma, &mut backend, 1, 0);
    assert_eq!(counts.tx_frames, 1);
    assert_eq!(backend.drain_tx_frames(), vec![pkt1.clone()]);
}

#[test]
fn tick_e1000_filters_invalid_rx_frames_and_flushes_to_guest_memory() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4);
    let mut dma = TestDma::new(0x40_000);

    setup_rx_ring(&mut dev, &mut dma);

    let mut backend = L2TunnelBackend::with_limits(8, 8, 2048);

    // Too short (< MIN_L2_FRAME_LEN) and too large (> MAX_L2_FRAME_LEN) should be dropped by the
    // pump and should not count toward `PumpCounts::rx_frames`.
    backend.push_rx_frame(vec![0u8; MIN_L2_FRAME_LEN - 1]);
    backend.push_rx_frame(vec![0u8; MAX_L2_FRAME_LEN + 1]);

    let valid = build_test_frame(b"ok");
    assert!(
        (MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&valid.len()),
        "test frame length sanity"
    );
    backend.push_rx_frame(valid.clone());

    let before = dma.read_vec(0x3000, valid.len());
    assert_ne!(
        before, valid,
        "sanity: buffer should not contain valid frame yet"
    );

    let counts = tick_e1000_with_counts(&mut dev, &mut dma, &mut backend, 0, 64);
    assert_eq!(counts.rx_frames, 1);

    assert_eq!(dma.read_vec(0x3000, valid.len()), valid);
}
