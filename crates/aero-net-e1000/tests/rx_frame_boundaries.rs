use aero_net_e1000::{E1000Device, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};
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

fn build_ethernet_frame_with_len(len: usize) -> Vec<u8> {
    assert!(
        len >= MIN_L2_FRAME_LEN,
        "frame must include at least dest/src/ethertype"
    );
    let payload_len = len - MIN_L2_FRAME_LEN;

    let mut frame = Vec::with_capacity(len);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend(std::iter::repeat_n(0xAB, payload_len));
    assert_eq!(frame.len(), len);
    frame
}

fn build_vlan_frame_with_len(len: usize) -> Vec<u8> {
    // VLAN header: TPID(0x8100) + TCI(2 bytes) inserted before Ethertype.
    const VLAN_HDR_LEN: usize = 4;
    let hdr_len = MIN_L2_FRAME_LEN + VLAN_HDR_LEN;
    assert!(len >= hdr_len, "VLAN frame too short");
    let payload_len = len - hdr_len;

    let mut frame = Vec::with_capacity(len);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x8100u16.to_be_bytes()); // TPID
    frame.extend_from_slice(&0x0001u16.to_be_bytes()); // TCI (VLAN ID 1)
    frame.extend_from_slice(&0x0800u16.to_be_bytes()); // inner ethertype
    frame.extend(std::iter::repeat_n(0xCD, payload_len));
    assert_eq!(frame.len(), len);
    frame
}

fn build_qinq_frame_with_len(len: usize) -> Vec<u8> {
    // QinQ header: outer TPID(0x88A8)+TCI + inner TPID(0x8100)+TCI inserted before Ethertype.
    const QINQ_HDR_LEN: usize = 8;
    let hdr_len = MIN_L2_FRAME_LEN + QINQ_HDR_LEN;
    assert!(len >= hdr_len, "QinQ frame too short");
    let payload_len = len - hdr_len;

    let mut frame = Vec::with_capacity(len);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x88A8u16.to_be_bytes()); // outer TPID (802.1ad)
    frame.extend_from_slice(&0x0001u16.to_be_bytes()); // outer TCI
    frame.extend_from_slice(&0x8100u16.to_be_bytes()); // inner TPID
    frame.extend_from_slice(&0x0002u16.to_be_bytes()); // inner TCI
    frame.extend_from_slice(&0x0800u16.to_be_bytes()); // inner ethertype
    frame.extend(std::iter::repeat_n(0xEF, payload_len));
    assert_eq!(frame.len(), len);
    frame
}

#[test]
fn rx_accepts_all_frame_lengths_in_model_range() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

    // A ring with N descriptors provides N-1 usable RX buffers; we need one per frame length.
    let usable = (MAX_L2_FRAME_LEN - MIN_L2_FRAME_LEN) + 1;
    let desc_count = usable + 1;

    let ring_base = 0x2000u64;
    let buf_base = 0x10_000u64;
    let buf_stride = 2048u64; // Default RX buffer size.

    // Descriptor ring + buffers fit comfortably within 4MiB.
    let mut dma = TestDma::new(0x40_0000);

    dev.mmio_write_u32_reg(REG_RDBAL, ring_base as u32);
    dev.mmio_write_u32_reg(REG_RDBAH, 0);
    dev.mmio_write_u32_reg(REG_RDLEN, (desc_count * 16) as u32);
    dev.mmio_write_u32_reg(REG_RDH, 0);
    dev.mmio_write_u32_reg(REG_RDT, (desc_count - 1) as u32);
    dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN); // 2048-byte buffers by default.

    for idx in 0..desc_count {
        let desc_addr = ring_base + (idx as u64) * 16;
        let buf_addr = buf_base + (idx as u64) * buf_stride;
        write_rx_desc(&mut dma, desc_addr, buf_addr);
    }

    for len in MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN {
        let frame = build_ethernet_frame_with_len(len);
        dev.receive_frame(&mut dma, &frame);
    }

    assert_eq!(
        dev.mmio_read_u32(REG_RDH) as usize,
        desc_count - 1,
        "device should have consumed one descriptor per accepted frame length"
    );

    // Each descriptor should have the corresponding frame length and no RXE error.
    for (idx, len) in (MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).enumerate() {
        let desc_addr = ring_base + (idx as u64) * 16;
        let (desc_len, status, errors) = read_rx_desc_fields(&mut dma, desc_addr);
        assert_eq!(
            desc_len as usize, len,
            "descriptor length mismatch at idx={idx}"
        );
        assert_eq!(status & 0x03, 0x03, "DD|EOP should be set at idx={idx}");
        assert_eq!(errors, 0, "errors should be clear at idx={idx}");
    }

    // The tail descriptor is intentionally kept unused.
    let tail_desc_addr = ring_base + ((desc_count - 1) as u64) * 16;
    let (desc_len, status, errors) = read_rx_desc_fields(&mut dma, tail_desc_addr);
    assert_eq!(desc_len, 0);
    assert_eq!(status, 0);
    assert_eq!(errors, 0);

    // Spot-check a few buffers for exact bytes (first/middle/last).
    for &len in &[MIN_L2_FRAME_LEN, 1000, MAX_L2_FRAME_LEN] {
        let idx = len - MIN_L2_FRAME_LEN;
        let buf_addr = buf_base + (idx as u64) * buf_stride;
        let expected = build_ethernet_frame_with_len(len);
        assert_eq!(dma.read_vec(buf_addr, expected.len()), expected);
    }
}

#[test]
fn rx_drops_frames_outside_model_len_bounds_without_dma_writes() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
    let mut dma = TestDma::new(0x10_000);

    // Configure RX ring: 2 descriptors at 0x2000 (1 usable).
    dev.mmio_write_u32_reg(REG_RDBAL, 0x2000);
    dev.mmio_write_u32_reg(REG_RDBAH, 0);
    dev.mmio_write_u32_reg(REG_RDLEN, 2 * 16);
    dev.mmio_write_u32_reg(REG_RDH, 0);
    dev.mmio_write_u32_reg(REG_RDT, 1);
    dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);

    write_rx_desc(&mut dma, 0x2000, 0x3000);
    write_rx_desc(&mut dma, 0x2010, 0x3400);
    dma.write(0x3000, &vec![0xCCu8; 2048]);

    // Too short.
    dev.receive_frame(&mut dma, &vec![0u8; MIN_L2_FRAME_LEN - 1]);
    assert_eq!(dma.read_vec(0x3000, 64), vec![0xCCu8; 64]);
    let (len, status, errors) = read_rx_desc_fields(&mut dma, 0x2000);
    assert_eq!(len, 0);
    assert_eq!(status, 0);
    assert_eq!(errors, 0);
    assert_eq!(dev.mmio_read_u32(REG_RDH), 0);

    // Too long.
    dev.receive_frame(&mut dma, &vec![0u8; MAX_L2_FRAME_LEN + 1]);
    assert_eq!(dma.read_vec(0x3000, 64), vec![0xCCu8; 64]);
    let (len, status, errors) = read_rx_desc_fields(&mut dma, 0x2000);
    assert_eq!(len, 0);
    assert_eq!(status, 0);
    assert_eq!(errors, 0);
    assert_eq!(dev.mmio_read_u32(REG_RDH), 0);
}

#[test]
fn rx_accepts_vlan_and_qinq_frames_at_max_len() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
    let mut dma = TestDma::new(0x40_000);

    // Configure RX ring: 3 descriptors at 0x2000 (2 usable).
    dev.mmio_write_u32_reg(REG_RDBAL, 0x2000);
    dev.mmio_write_u32_reg(REG_RDBAH, 0);
    dev.mmio_write_u32_reg(REG_RDLEN, 3 * 16);
    dev.mmio_write_u32_reg(REG_RDH, 0);
    dev.mmio_write_u32_reg(REG_RDT, 2);
    dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);

    write_rx_desc(&mut dma, 0x2000, 0x3000);
    write_rx_desc(&mut dma, 0x2010, 0x3800);
    write_rx_desc(&mut dma, 0x2020, 0x4000);

    let vlan = build_vlan_frame_with_len(MAX_L2_FRAME_LEN);
    let qinq = build_qinq_frame_with_len(MAX_L2_FRAME_LEN);
    dev.receive_frame(&mut dma, &vlan);
    dev.receive_frame(&mut dma, &qinq);

    // VLAN frame should be delivered unchanged.
    let (len0, status0, errors0) = read_rx_desc_fields(&mut dma, 0x2000);
    assert_eq!(len0 as usize, MAX_L2_FRAME_LEN);
    assert_eq!(status0 & 0x03, 0x03);
    assert_eq!(errors0, 0);
    let out0 = dma.read_vec(0x3000, vlan.len());
    assert_eq!(out0, vlan);
    assert_eq!(&out0[12..14], &0x8100u16.to_be_bytes());

    // QinQ frame should be delivered unchanged.
    let (len1, status1, errors1) = read_rx_desc_fields(&mut dma, 0x2010);
    assert_eq!(len1 as usize, MAX_L2_FRAME_LEN);
    assert_eq!(status1 & 0x03, 0x03);
    assert_eq!(errors1, 0);
    let out1 = dma.read_vec(0x3800, qinq.len());
    assert_eq!(out1, qinq);
    assert_eq!(&out1[12..14], &0x88A8u16.to_be_bytes());
    assert_eq!(&out1[16..18], &0x8100u16.to_be_bytes());
}
