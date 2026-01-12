use aero_net_e1000::E1000Device;
use memory::MemoryBus;

struct TestDma {
    mem: Vec<u8>,
}

impl TestDma {
    fn new(size: usize) -> Self {
        Self { mem: vec![0u8; size] }
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

/// Minimal legacy RX descriptor layout (16 bytes).
fn write_rx_desc(dma: &mut TestDma, addr: u64, buf_addr: u64, status: u8) {
    write_u64_le(dma, addr, buf_addr);
    dma.write(addr + 8, &0u16.to_le_bytes()); // length
    dma.write(addr + 10, &0u16.to_le_bytes()); // checksum
    dma.write(addr + 12, &[status]);
    dma.write(addr + 13, &[0u8]); // errors
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

fn build_test_frame(payload_len: usize) -> Vec<u8> {
    let mut frame = Vec::with_capacity(aero_net_e1000::MIN_L2_FRAME_LEN + payload_len);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend(std::iter::repeat(0xAB).take(payload_len));
    frame
}

#[test]
fn rx_descriptor_is_marked_error_when_frame_exceeds_configured_buffer_len() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4);
    let mut dma = TestDma::new(0x40_000);

    // Configure RX ring: 2 descriptors at 0x2000.
    dev.mmio_write_u32_reg(0x2800, 0x2000); // RDBAL
    dev.mmio_write_u32_reg(0x2804, 0); // RDBAH
    dev.mmio_write_u32_reg(0x2808, 2 * 16); // RDLEN
    dev.mmio_write_u32_reg(0x2810, 0); // RDH
    dev.mmio_write_u32_reg(0x2818, 1); // RDT (one descriptor available)

    // Enable RX with a 256-byte buffer size (`BSEX=0`, `BSIZE=0b11`).
    let rctl = (1u32 << 1) | (0b11u32 << 16);
    dev.mmio_write_u32_reg(0x0100, rctl);

    // Populate RX descriptors with guest buffers.
    write_rx_desc(&mut dma, 0x2000, 0x3000, 0);
    write_rx_desc(&mut dma, 0x2010, 0x3400, 0);

    // Pre-fill the guest buffer with a sentinel value to ensure the device doesn't write a
    // truncated frame.
    dma.write(0x3000, &vec![0xCCu8; 256]);

    // Deliver a frame larger than the configured RX buffer size.
    let pkt = build_test_frame(300);
    assert!(
        pkt.len() > 256,
        "test frame should exceed 256-byte RX buffer"
    );
    dev.receive_frame(&mut dma, &pkt);

    // The RX buffer should remain untouched.
    assert_eq!(dma.read_vec(0x3000, 256), vec![0xCCu8; 256]);

    // Descriptor should be marked done with RXE error and length=0.
    let mut desc_bytes = [0u8; 16];
    dma.read_physical(0x2000, &mut desc_bytes);
    let length = u16::from_le_bytes([desc_bytes[8], desc_bytes[9]]);
    let status = desc_bytes[12];
    let errors = desc_bytes[13];
    assert_eq!(length, 0);
    assert_eq!(status & 0x03, 0x03, "DD|EOP should be set");
    assert_eq!(errors & 0x80, 0x80, "RXE should be set");
}

