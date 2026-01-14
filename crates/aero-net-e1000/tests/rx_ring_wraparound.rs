use aero_net_e1000::E1000Device;
use memory::MemoryBus;

const REG_RCTL: u32 = 0x0100;
const REG_RDBAL: u32 = 0x2800;
const REG_RDBAH: u32 = 0x2804;
const REG_RDLEN: u32 = 0x2808;
const REG_RDH: u32 = 0x2810;
const REG_RDT: u32 = 0x2818;

const RCTL_EN: u32 = 1 << 1;

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

/// Minimal legacy RX descriptor layout (16 bytes).
fn write_rx_desc(dma: &mut TestDma, addr: u64, buf_addr: u64) {
    write_u64_le(dma, addr, buf_addr);
    dma.write(addr + 8, &0u16.to_le_bytes()); // length
    dma.write(addr + 10, &0u16.to_le_bytes()); // checksum
    dma.write(addr + 12, &[0u8]); // status
    dma.write(addr + 13, &[0u8]); // errors
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

fn read_rx_desc_fields(dma: &mut TestDma, addr: u64) -> (u16, u8, u8) {
    let mut desc_bytes = [0u8; 16];
    dma.read_physical(addr, &mut desc_bytes);
    let length = u16::from_le_bytes([desc_bytes[8], desc_bytes[9]]);
    let status = desc_bytes[12];
    let errors = desc_bytes[13];
    (length, status, errors)
}

fn build_test_frame(tag: u8) -> Vec<u8> {
    let mut frame = Vec::with_capacity(aero_net_e1000::MIN_L2_FRAME_LEN + 4);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(&[tag; 4]);
    frame
}

#[test]
fn rx_ring_consumes_descriptors_in_modulo_order_on_wraparound() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
    let mut dma = TestDma::new(0x40_000);

    // Configure RX ring: 4 descriptors at 0x2000.
    dev.mmio_write_u32_reg(REG_RDBAL, 0x2000);
    dev.mmio_write_u32_reg(REG_RDBAH, 0);
    dev.mmio_write_u32_reg(REG_RDLEN, 4 * 16);

    // Force head near the end so processing wraps back to 0.
    dev.mmio_write_u32_reg(REG_RDH, 2);
    dev.mmio_write_u32_reg(REG_RDT, 1);

    dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN); // 2048-byte buffers by default.

    // Populate all descriptors with buffers; only indices 2,3,0 should be consumed.
    write_rx_desc(&mut dma, 0x2000 + 0 * 16, 0x3000);
    write_rx_desc(&mut dma, 0x2000 + 1 * 16, 0x3400);
    write_rx_desc(&mut dma, 0x2000 + 2 * 16, 0x3800);
    write_rx_desc(&mut dma, 0x2000 + 3 * 16, 0x3C00);

    let frames = [
        build_test_frame(0xA1),
        build_test_frame(0xB2),
        build_test_frame(0xC3),
    ];
    for frame in &frames {
        dev.enqueue_rx_frame(frame.clone());
    }
    dev.poll(&mut dma);

    assert_eq!(
        dev.mmio_read_u32(REG_RDH),
        1,
        "head should wrap around to tail after consuming 3 descriptors"
    );

    // Expected consumption order: 2,3,0.
    let expected = [
        (2u64, 0x3800u64, &frames[0]),
        (3u64, 0x3C00u64, &frames[1]),
        (0u64, 0x3000u64, &frames[2]),
    ];
    for (idx, buf_addr, frame) in expected {
        let desc_addr = 0x2000 + idx * 16;
        let (len, status, errors) = read_rx_desc_fields(&mut dma, desc_addr);
        assert_eq!(
            len as usize,
            frame.len(),
            "length mismatch for desc idx={idx}"
        );
        assert_eq!(status & 0x03, 0x03, "DD|EOP should be set for idx={idx}");
        assert_eq!(errors, 0, "unexpected errors for idx={idx}");
        assert_eq!(
            dma.read_vec(buf_addr, frame.len()),
            *frame,
            "buffer mismatch for desc idx={idx}"
        );
    }

    // Descriptor 1 should remain untouched.
    let (len, status, errors) = read_rx_desc_fields(&mut dma, 0x2000 + 1 * 16);
    assert_eq!(len, 0);
    assert_eq!(status, 0);
    assert_eq!(errors, 0);
}
