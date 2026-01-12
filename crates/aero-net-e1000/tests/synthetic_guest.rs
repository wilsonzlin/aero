use aero_net_e1000::{E1000Device, E1000_IO_SIZE, E1000_MMIO_SIZE, ICR_RXT0, ICR_TXDW};
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

#[test]
fn pci_bars_probe_and_program() {
    let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);

    // Probe BAR0 size.
    dev.pci_write_u32(0x10, 0xFFFF_FFFF);
    let mask = dev.pci_read_u32(0x10);
    assert_eq!(mask, (!(E1000_MMIO_SIZE - 1)) & 0xFFFF_FFF0);
    // Sub-dword reads should observe the same probe mask.
    assert_eq!(dev.pci_config_read(0x12, 2), 0xFFFE);

    // Program BAR0.
    dev.pci_write_u32(0x10, 0xFEBE_0000);
    assert_eq!(dev.pci_read_u32(0x10), 0xFEBE_0000);
    assert_eq!(dev.pci_config_read(0x12, 2), 0xFEBE);

    // Misaligned BAR0 writes should be masked to the BAR size (0x20_000) like real hardware.
    dev.pci_write_u32(0x10, 0xFEBF_1234);
    assert_eq!(dev.pci_read_u32(0x10), 0xFEBE_0000);
    assert_eq!(dev.pci_config_read(0x12, 2), 0xFEBE);

    // Probe BAR1 size (I/O BAR).
    dev.pci_write_u32(0x14, 0xFFFF_FFFF);
    let mask = dev.pci_read_u32(0x14);
    assert_eq!(mask, ((!(E1000_IO_SIZE - 1)) & 0xFFFF_FFFC) | 0x1);
    assert_eq!(dev.pci_config_read(0x14, 2), 0xFFC1);

    // Program BAR1 (bit0 must remain set).
    dev.pci_write_u32(0x14, 0xC000);
    assert_eq!(dev.pci_read_u32(0x14), 0xC001);

    // Misaligned BAR1 writes should be masked to the BAR size (0x40) and keep bit0 set.
    dev.pci_write_u32(0x14, 0xC012);
    assert_eq!(dev.pci_read_u32(0x14), 0xC001);
}

#[test]
fn pci_bar1_io_indicator_bit_is_present_in_byte_reads() {
    // Regression test: BAR1 is an I/O BAR, so bit0 must read back as 1 even when read as 8/16-bit
    // config space accesses. (The device model stores BAR values both in decoded fields and the
    // raw config byte array.)
    let dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
    assert_eq!(dev.pci_config_read(0x14, 1), 0x1);
    assert_eq!(dev.pci_config_read(0x14, 2), 0x1);
}

#[test]
fn pci_bar_partial_writes_update_decoded_bar_fields() {
    // Regression test: if a guest performs 8/16-bit writes into BAR dwords, the decoded BAR fields
    // must remain coherent (since BAR reads consult the decoded fields for probe behavior).
    let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);

    // Program BAR0 by writing only the high 16 bits.
    dev.pci_config_write(0x12, 2, 0xFEBF);
    assert_eq!(dev.pci_read_u32(0x10), 0xFEBE_0000);

    // Program BAR1 via a 16-bit write; bit0 must remain set.
    dev.pci_config_write(0x14, 2, 0xC000);
    assert_eq!(dev.pci_read_u32(0x14), 0xC001);
}

#[test]
fn pci_bar_cross_boundary_accesses_are_coherent() {
    // Regression test: reads/writes that straddle the BAR0/BAR1 boundary (e.g. 16-bit at 0x13)
    // should still observe probe masks and keep decoded BAR fields coherent.
    let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);

    // Probe BAR0, then perform a cross-boundary read that includes BAR0 high byte (0x13) and BAR1
    // low byte (0x14). BAR0 probe mask is 0xFFFE_0000, whose high byte is 0xFF; BAR1 default low
    // byte is 0x01.
    dev.pci_write_u32(0x10, 0xFFFF_FFFF);
    assert_eq!(dev.pci_config_read(0x13, 2), 0x01FF);

    // Program BAR0, then update BAR0 high byte + BAR1 low byte with a single 16-bit config write.
    dev.pci_write_u32(0x10, 0xFEBE_0000);
    dev.pci_config_write(0x13, 2, 0xA55A);

    // BAR0 bytes 0..2 remain from 0xFEBE_0000; high byte is updated to 0x5A.
    assert_eq!(dev.pci_read_u32(0x10), 0x5ABE_0000);
    // BAR1 low byte is masked to the I/O BAR size (0x40) and bit0 remains set.
    assert_eq!(dev.pci_read_u32(0x14), 0x0000_0081);
    // Cross-boundary read should observe the masked result.
    assert_eq!(dev.pci_config_read(0x13, 2), 0x815A);
}

#[test]
fn pci_config_oob_accesses_do_not_panic() {
    // Robustness regression test: malformed / buggy callers should not be able to panic by issuing
    // out-of-bounds PCI config space accesses.
    let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);

    let vendor_id = dev.pci_config_read(0x00, 2);

    // Reads beyond the 256-byte config space should return 0.
    assert_eq!(dev.pci_config_read(0x100, 1), 0);
    assert_eq!(dev.pci_config_read(0xFF, 2), 0);
    assert_eq!(dev.pci_config_read(0xFF, 4), 0);
    assert_eq!(dev.pci_config_read(0xFD, 4), 0);

    // Writes beyond the config space should be ignored.
    dev.pci_config_write(0x100, 4, 0xDEAD_BEEF);
    dev.pci_config_write(0xFF, 2, 0xBEEF);
    dev.pci_config_write(0xFD, 4, 0x1234_5678);
    dev.pci_config_write(0x00, 3, 0xBAD0_C0DE); // invalid size

    assert_eq!(dev.pci_config_read(0x00, 2), vendor_id);
}

#[test]
fn eeprom_read_returns_mac_words() {
    let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let mut dev = E1000Device::new(mac);

    // Read EEPROM word 0.
    dev.mmio_write_u32_reg(0x0014, 1); // START + addr (word 0)
    let eerd = dev.mmio_read_u32(0x0014);
    let data = (eerd >> 16) as u16;
    assert_eq!(data, u16::from_le_bytes([mac[0], mac[1]]));
}

#[test]
fn synthetic_guest_tx_and_rx() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    // Real hardware requires PCI Bus Master Enable before the NIC may DMA descriptors/buffers.
    dev.pci_config_write(0x04, 2, 0x4);
    let mut dma = TestDma::new(0x40_000);

    // Enable interrupts for both RX and TX.
    dev.mmio_write_u32_reg(0x00D0, ICR_RXT0 | ICR_TXDW); // IMS

    // Configure TX ring: 4 descriptors at 0x1000.
    dev.mmio_write_u32_reg(0x3800, 0x1000); // TDBAL
    dev.mmio_write_u32_reg(0x3804, 0); // TDBAH
    dev.mmio_write_u32_reg(0x3808, 4 * 16); // TDLEN
    dev.mmio_write_u32_reg(0x3810, 0); // TDH
    dev.mmio_write_u32_reg(0x3818, 0); // TDT
    dev.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN

    // Configure RX ring: 2 descriptors at 0x2000.
    dev.mmio_write_u32_reg(0x2800, 0x2000); // RDBAL
    dev.mmio_write_u32_reg(0x2804, 0); // RDBAH
    dev.mmio_write_u32_reg(0x2808, 2 * 16); // RDLEN
    dev.mmio_write_u32_reg(0x2810, 0); // RDH
    dev.mmio_write_u32_reg(0x2818, 1); // RDT
    dev.mmio_write_u32_reg(0x0100, 1 << 1); // RCTL.EN (defaults to 2048 buffer)

    // Populate RX descriptors with guest buffers.
    write_rx_desc(&mut dma, 0x2000, 0x3000, 0);
    write_rx_desc(&mut dma, 0x2010, 0x3400, 0);

    // Guest TX: descriptor 0 points at packet buffer 0x4000.
    let pkt_out = build_test_frame(b"guest->host");
    dma.write(0x4000, &pkt_out);
    write_tx_desc(
        &mut dma,
        0x1000,
        0x4000,
        pkt_out.len() as u16,
        0b0000_1001,
        0,
    ); // EOP|RS
    dev.mmio_write_u32_reg(0x3818, 1);
    dev.poll(&mut dma); // TDT advances TX processing via DMA.

    assert_eq!(dev.pop_tx_frame().as_deref(), Some(pkt_out.as_slice()));
    assert!(dev.irq_level());
    let causes = dev.mmio_read_u32(0x00C0);
    assert_eq!(causes & ICR_TXDW, ICR_TXDW);

    // Host RX: deliver frame into guest ring.
    let pkt_in = build_test_frame(b"host->guest");
    dev.receive_frame(&mut dma, &pkt_in);

    assert_eq!(dma.read_vec(0x3000, pkt_in.len()), pkt_in);
    assert!(dev.irq_level());
    let causes = dev.mmio_read_u32(0x00C0);
    assert_eq!(causes & ICR_RXT0, ICR_RXT0);
}

#[test]
fn synthetic_guest_tx_and_rx_deferred_dma_via_poll() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    // Real hardware requires PCI Bus Master Enable before the NIC may DMA descriptors/buffers.
    dev.pci_config_write(0x04, 2, 0x4);
    let mut dma = TestDma::new(0x40_000);

    // Enable interrupts for both RX and TX (register-only write).
    dev.mmio_write_u32_reg(0x00D0, ICR_RXT0 | ICR_TXDW); // IMS

    // Configure TX ring: 4 descriptors at 0x1000.
    dev.mmio_write_u32_reg(0x3800, 0x1000); // TDBAL
    dev.mmio_write_u32_reg(0x3804, 0); // TDBAH
    dev.mmio_write_u32_reg(0x3808, 4 * 16); // TDLEN
    dev.mmio_write_u32_reg(0x3810, 0); // TDH
    dev.mmio_write_u32_reg(0x3818, 0); // TDT
    dev.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN

    // Configure RX ring: 2 descriptors at 0x2000.
    dev.mmio_write_u32_reg(0x2800, 0x2000); // RDBAL
    dev.mmio_write_u32_reg(0x2804, 0); // RDBAH
    dev.mmio_write_u32_reg(0x2808, 2 * 16); // RDLEN
    dev.mmio_write_u32_reg(0x2810, 0); // RDH
    dev.mmio_write_u32_reg(0x2818, 1); // RDT
    dev.mmio_write_u32_reg(0x0100, 1 << 1); // RCTL.EN (defaults to 2048 buffer)

    // Populate RX descriptors with guest buffers.
    write_rx_desc(&mut dma, 0x2000, 0x3000, 0);
    write_rx_desc(&mut dma, 0x2010, 0x3400, 0);

    // Guest TX: descriptor 0 points at packet buffer 0x4000.
    let pkt_out = build_test_frame(b"guest->host");
    dma.write(0x4000, &pkt_out);
    write_tx_desc(
        &mut dma,
        0x1000,
        0x4000,
        pkt_out.len() as u16,
        0b0000_1001,
        0,
    ); // EOP|RS

    // Update tail (register-only), then poll to perform DMA.
    dev.mmio_write_u32_reg(0x3818, 1);
    assert!(dev.pop_tx_frame().is_none());
    dev.poll(&mut dma);

    assert_eq!(dev.pop_tx_frame().as_deref(), Some(pkt_out.as_slice()));
    assert!(dev.irq_level());
    let causes = dev.mmio_read_u32(0x00C0);
    assert_eq!(causes & ICR_TXDW, ICR_TXDW);

    // Host RX: enqueue without DMA, then poll to flush.
    let pkt_in = build_test_frame(b"host->guest");
    dev.enqueue_rx_frame(pkt_in.clone());
    assert_ne!(dma.read_vec(0x3000, pkt_in.len()), pkt_in);
    dev.poll(&mut dma);

    assert_eq!(dma.read_vec(0x3000, pkt_in.len()), pkt_in);
    assert!(dev.irq_level());
    let causes = dev.mmio_read_u32(0x00C0);
    assert_eq!(causes & ICR_RXT0, ICR_RXT0);
}
