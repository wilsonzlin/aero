use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_net_e1000::{E1000Device, ICR_TXDW, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};
use memory::MemoryBus;

const REG_IMS: u32 = 0x00D0;

const REG_RCTL: u32 = 0x0100;
const REG_RDBAL: u32 = 0x2800;
const REG_RDLEN: u32 = 0x2808;
const REG_RDH: u32 = 0x2810;
const REG_RDT: u32 = 0x2818;

const REG_TCTL: u32 = 0x0400;
const REG_TDBAL: u32 = 0x3800;
const REG_TDLEN: u32 = 0x3808;
const REG_TDH: u32 = 0x3810;
const REG_TDT: u32 = 0x3818;

const RCTL_EN: u32 = 1 << 1;
const TCTL_EN: u32 = 1 << 1;

const TXD_CMD_EOP: u8 = 1 << 0;
const TXD_CMD_RS: u8 = 1 << 3;

struct TestDma {
    mem: Vec<u8>,
}

impl TestDma {
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

fn build_test_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(MIN_L2_FRAME_LEN + payload.len());
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(payload);
    assert!((MIN_L2_FRAME_LEN..=MAX_L2_FRAME_LEN).contains(&frame.len()));
    frame
}

fn write_tx_desc(dma: &mut TestDma, addr: u64, buf_addr: u64, len: u16, cmd: u8) {
    dma.write(addr, &buf_addr.to_le_bytes());
    dma.write(addr + 8, &len.to_le_bytes());
    dma.write(addr + 10, &[0]); // cso
    dma.write(addr + 11, &[cmd]);
    dma.write(addr + 12, &[0]); // status
    dma.write(addr + 13, &[0]); // css
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

fn write_rx_desc(dma: &mut TestDma, addr: u64, buf_addr: u64) {
    dma.write(addr, &buf_addr.to_le_bytes());
    dma.write(addr + 8, &0u16.to_le_bytes()); // length
    dma.write(addr + 10, &0u16.to_le_bytes()); // checksum
    dma.write(addr + 12, &[0]); // status
    dma.write(addr + 13, &[0]); // errors
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

#[test]
fn snapshot_roundtrip_preserves_key_state() {
    let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let mut dev = E1000Device::new(mac);
    let mut dma = TestDma::new(0x10000);
    dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

    // Mutate PCI config.
    dev.pci_write_u32(0x10, 0xFEBF_0000);
    dev.pci_write_u32(0x14, 0xC000);

    // Configure TX ring.
    let tx_ring = 0x1000u64;
    dev.mmio_write_u32(REG_IMS, ICR_TXDW);
    dev.mmio_write_u32(REG_TDBAL, tx_ring as u32);
    dev.mmio_write_u32(REG_TDLEN, 4 * 16);
    dev.mmio_write_u32(REG_TDH, 0);
    dev.mmio_write_u32(REG_TDT, 0);
    dev.mmio_write_u32(REG_TCTL, TCTL_EN);

    // Configure RX ring but keep it "full" (RDH==RDT) so pending frames are not flushed.
    let rx_ring = 0x5000u64;
    dev.mmio_write_u32(REG_RDBAL, rx_ring as u32);
    dev.mmio_write_u32(REG_RDLEN, 4 * 16);
    dev.mmio_write_u32(REG_RDH, 0);
    dev.mmio_write_u32(REG_RDT, 0);
    dev.mmio_write_u32(REG_RCTL, RCTL_EN);

    // Populate a register in other_regs.
    dev.mmio_write_u32(0x1234, 0xDEAD_BEEF);

    // Queue RX frames but ensure they remain pending at snapshot time.
    let rx1 = build_test_frame(b"rx1");
    let rx2 = build_test_frame(b"rx2");
    dev.enqueue_rx_frame(rx1.clone());
    dev.enqueue_rx_frame(rx2.clone());

    // Produce one fully-completed TX frame and one partial in-progress packet.
    let tx0 = build_test_frame(b"tx0");
    let tx0_addr = 0x2000u64;
    dma.write(tx0_addr, &tx0);

    let tx1_full = build_test_frame(b"tx1-snap");
    let split = tx1_full.len() / 2;
    let (tx1_part1, tx1_part2) = tx1_full.split_at(split);
    let tx1_part1_addr = 0x3000u64;
    let tx1_part2_addr = 0x3100u64;
    dma.write(tx1_part1_addr, tx1_part1);

    write_tx_desc(
        &mut dma,
        tx_ring,
        tx0_addr,
        tx0.len() as u16,
        TXD_CMD_EOP | TXD_CMD_RS,
    );
    write_tx_desc(
        &mut dma,
        tx_ring + 16,
        tx1_part1_addr,
        tx1_part1.len() as u16,
        TXD_CMD_RS,
    );

    dev.mmio_write_u32(REG_TDT, 2);
    dev.poll(&mut dma);

    assert!(dev.irq_level(), "tx should have raised an interrupt");

    let snapshot = dev.save_state();

    // Restore into a fresh device.
    let mut restored = E1000Device::new([0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]);
    restored.load_state(&snapshot).expect("load_state");

    assert_eq!(restored.mac_addr(), mac);
    assert_eq!(restored.pci_read_u32(0x10), 0xFEBF_0000);
    assert_eq!(restored.pci_read_u32(0x14), 0xC001);
    assert_eq!(restored.mmio_read_u32(REG_IMS), ICR_TXDW);
    assert_eq!(restored.mmio_read_u32(0x1234), 0xDEAD_BEEF);
    assert!(restored.irq_level(), "irq_level should be recomputed on restore");

    assert_eq!(restored.pop_tx_frame().as_deref(), Some(tx0.as_slice()));

    // Complete the partial TX packet after restore.
    dma.write(tx1_part2_addr, tx1_part2);
    write_tx_desc(
        &mut dma,
        tx_ring + 2 * 16,
        tx1_part2_addr,
        tx1_part2.len() as u16,
        TXD_CMD_EOP | TXD_CMD_RS,
    );

    restored.mmio_write_u32(REG_TDT, 3);
    restored.poll(&mut dma);

    assert_eq!(
        restored.pop_tx_frame().as_deref(),
        Some(tx1_full.as_slice()),
        "partial TX state should survive snapshot"
    );
    assert!(restored.pop_tx_frame().is_none());

    // Provide RX descriptors and flush pending frames.
    let rx_buf0 = 0x6000u64;
    let rx_buf1 = 0x7000u64;
    let rx_buf2 = 0x8000u64;
    write_rx_desc(&mut dma, rx_ring, rx_buf0);
    write_rx_desc(&mut dma, rx_ring + 16, rx_buf1);
    write_rx_desc(&mut dma, rx_ring + 2 * 16, rx_buf2);

    restored.mmio_write_u32(REG_RDT, 3);
    restored.poll(&mut dma);

    assert_eq!(dma.read_vec(rx_buf0, rx1.len()), rx1);
    assert_eq!(dma.read_vec(rx_buf1, rx2.len()), rx2);
}

#[test]
fn snapshot_rejects_absurd_other_regs_count() {
    // TAG_OTHER_REGS = 90, encoded as: count (u32) then count*(u32,u32).
    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_bytes(90, Encoder::new().u32(65_537).finish());
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 other_regs count")
    );
}

#[test]
fn snapshot_rejects_absurd_rx_pending_count() {
    // TAG_RX_PENDING = 91, encoded as: count (u32) then each frame length+bytes.
    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_bytes(91, Encoder::new().u32(257).finish());
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 rx_pending count")
    );
}
