#![cfg(feature = "io-snapshot")]

use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotVersion, SnapshotWriter};
use aero_net_e1000::{
    E1000Device, ICR_TXDW, MAX_L2_FRAME_LEN, MAX_TX_AGGREGATE_LEN, MIN_L2_FRAME_LEN,
};
use memory::MemoryBus;
use nt_packetlib::io::net::packet::checksum::ipv4_header_checksum;

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

#[allow(clippy::too_many_arguments)]
fn write_adv_context_desc(
    dma: &mut TestDma,
    addr: u64,
    ipcss: u8,
    ipcso: u8,
    ipcse: u16,
    tucss: u8,
    tucso: u8,
    tucse: u16,
    mss: u16,
    hdr_len: u8,
    cmd: u8,
) {
    dma.write(addr, &[ipcss]);
    dma.write(addr + 1, &[ipcso]);
    dma.write(addr + 2, &ipcse.to_le_bytes());
    dma.write(addr + 4, &[tucss]);
    dma.write(addr + 5, &[tucso]);
    dma.write(addr + 6, &tucse.to_le_bytes());
    dma.write(addr + 8, &0u16.to_le_bytes()); // reserved
    dma.write(addr + 10, &[0x20]); // DTYP=CTXT (0x2 << 4)
    dma.write(addr + 11, &[cmd]);
    dma.write(addr + 12, &mss.to_le_bytes());
    dma.write(addr + 14, &[hdr_len]);
    dma.write(addr + 15, &[0]); // tcp header len (unused)
}

fn write_adv_data_desc(dma: &mut TestDma, addr: u64, buf_addr: u64, len: u16, cmd: u8, popts: u8) {
    dma.write(addr, &buf_addr.to_le_bytes());
    dma.write(addr + 8, &len.to_le_bytes());
    dma.write(addr + 10, &[0x30]); // DTYP=DATA (0x3 << 4)
    dma.write(addr + 11, &[cmd]);
    dma.write(addr + 12, &[0]); // status
    dma.write(addr + 13, &[popts]);
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
    dev.mmio_write_u32_reg(REG_IMS, ICR_TXDW);
    dev.mmio_write_u32_reg(REG_TDBAL, tx_ring as u32);
    dev.mmio_write_u32_reg(REG_TDLEN, 4 * 16);
    dev.mmio_write_u32_reg(REG_TDH, 0);
    dev.mmio_write_u32_reg(REG_TDT, 0);
    dev.mmio_write_u32_reg(REG_TCTL, TCTL_EN);

    // Configure RX ring but keep it "full" (RDH==RDT) so pending frames are not flushed.
    let rx_ring = 0x5000u64;
    dev.mmio_write_u32_reg(REG_RDBAL, rx_ring as u32);
    dev.mmio_write_u32_reg(REG_RDLEN, 4 * 16);
    dev.mmio_write_u32_reg(REG_RDH, 0);
    dev.mmio_write_u32_reg(REG_RDT, 0);
    dev.mmio_write_u32_reg(REG_RCTL, RCTL_EN);

    // Populate a register in other_regs.
    dev.mmio_write_u32_reg(0x1234, 0xDEAD_BEEF);

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

    dev.mmio_write_u32_reg(REG_TDT, 2);
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

    restored.mmio_write_u32_reg(REG_TDT, 3);
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

    restored.mmio_write_u32_reg(REG_RDT, 3);
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

#[test]
fn snapshot_roundtrip_restores_advanced_tx_state() {
    // Advanced TX descriptor bits.
    const TXD_CMD_EOP: u8 = 1 << 0;
    const TXD_CMD_RS: u8 = 1 << 3;
    const TXD_CMD_DEXT: u8 = 1 << 5;

    // Advanced TX data descriptor popts flags.
    const POPTS_IXSM: u8 = 0x01;

    let mut dma = TestDma::new(0x20_000);
    let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);
    dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable (allow TX DMA)

    // Configure TX ring.
    let tx_ring = 0x1000u64;
    dev.mmio_write_u32_reg(REG_TDBAL, tx_ring as u32);
    dev.mmio_write_u32_reg(REG_TDLEN, 4 * 16);
    dev.mmio_write_u32_reg(REG_TDH, 0);
    dev.mmio_write_u32_reg(REG_TDT, 0);
    dev.mmio_write_u32_reg(REG_TCTL, TCTL_EN);

    // Build a minimal Ethernet+IPv4 packet; checksum field initially zero.
    let payload = [0x01, 0x02, 0x03, 0x04];
    let ip_total_len = (20 + payload.len()) as u16;
    let mut ipv4 = [
        0x45, 0x00, 0x00, 0x00, 0x12, 0x34, 0x00, 0x00, 64, 17, 0x00, 0x00, 192, 168, 0, 2,
        192, 168, 0, 1,
    ];
    ipv4[2..4].copy_from_slice(&ip_total_len.to_be_bytes());
    let expected_csum = ipv4_header_checksum(&ipv4);

    let mut frame = Vec::new();
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src
    frame.extend_from_slice(&0x0800u16.to_be_bytes()); // ethertype
    frame.extend_from_slice(&ipv4);
    frame.extend_from_slice(&payload);

    let split = 30usize;
    let part1 = frame[..split].to_vec();
    let part2 = frame[split..].to_vec();

    let part1_addr = 0x2000u64;
    let part2_addr = 0x3000u64;
    dma.write(part1_addr, &part1);
    dma.write(part2_addr, &part2);

    // Context descriptor sets up checksum offsets (ipcss/ipcso/ipcse) so IXSM can be applied.
    let ipcss = 14u8;
    let ipcso = (14 + 10) as u8;
    let ipcse = (14 + 20 - 1) as u16;
    let tucss = (14 + 20) as u8;
    let tucso = (14 + 20 + 16) as u8;
    let tucse = (frame.len() - 1) as u16;

    write_adv_context_desc(
        &mut dma,
        tx_ring,
        ipcss,
        ipcso,
        ipcse,
        tucss,
        tucso,
        tucse,
        1460,
        54,
        TXD_CMD_DEXT | TXD_CMD_RS,
    );
    write_adv_data_desc(
        &mut dma,
        tx_ring + 16,
        part1_addr,
        part1.len() as u16,
        TXD_CMD_DEXT | TXD_CMD_RS,
        POPTS_IXSM,
    );
    write_adv_data_desc(
        &mut dma,
        tx_ring + 2 * 16,
        part2_addr,
        part2.len() as u16,
        TXD_CMD_DEXT | TXD_CMD_EOP | TXD_CMD_RS,
        0,
    );

    // Process context + first data descriptor (leave the packet incomplete).
    dev.mmio_write_u32_reg(REG_TDT, 2);
    dev.poll(&mut dma);

    let snapshot = dev.save_state();

    let mut restored = E1000Device::new([0; 6]);
    restored.load_state(&snapshot).expect("load_state");

    // Complete the packet after restore.
    restored.mmio_write_u32_reg(REG_TDT, 3);
    restored.poll(&mut dma);
    let out = restored.pop_tx_frame().expect("expected a TX frame");
    assert_eq!(out.len(), frame.len());
    assert_eq!(out[14 + 10..14 + 12], expected_csum.to_be_bytes());
}

#[test]
fn snapshot_load_rejects_corrupt_or_oversized_fields() {
    let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);

    // Corrupt magic/truncated header.
    assert!(matches!(
        dev.load_state(b"NOPE"),
        Err(SnapshotError::UnexpectedEof | SnapshotError::InvalidMagic)
    ));

    // Oversized tx_partial should be rejected.
    let oversized = vec![0u8; MAX_TX_AGGREGATE_LEN + 1];
    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_bytes(60, oversized);
    let bytes = w.finish();

    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("e1000 tx_partial"));
}

#[test]
fn snapshot_load_is_atomic_on_error() {
    // `E1000Device::load_state` should not partially mutate the live device if decoding fails.
    //
    // This matters because snapshot blobs may be loaded from untrusted sources (e.g. downloaded
    // files), and we want a clear "all-or-nothing" semantics for device restore.
    let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);

    // Make the device non-trivial so partial application would be observable.
    dev.pci_config_write(0x10, 4, 0xDEAD_BEEF);
    dev.mmio_write_u32_reg(REG_IMS, 0xA5A5_5A5A);
    dev.enqueue_rx_frame(vec![0x11u8; MIN_L2_FRAME_LEN]);

    let before = dev.snapshot_state();

    // Construct a snapshot with a valid header but a malformed MAC field (wrong length).
    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_bytes(80, vec![0u8; 5]); // TAG_MAC_ADDR, must be 6 bytes
    let bytes = w.finish();

    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("e1000 mac"));

    // The device should remain unchanged.
    assert_eq!(dev.snapshot_state(), before);
}

#[test]
fn snapshot_roundtrip_preserves_pci_bar_probe_flags() {
    let mut dev = E1000Device::new([0x52, 0x54, 0, 0x12, 0x34, 0x56]);

    // Put both BARs into probe mode (guest writes all-ones).
    dev.pci_write_u32(0x10, 0xFFFF_FFFF);
    dev.pci_write_u32(0x14, 0xFFFF_FFFF);

    let snapshot = dev.save_state();

    let mut restored = E1000Device::new([0; 6]);
    restored.load_state(&snapshot).expect("load_state");

    // Reads should still return size masks (probe response) after restore.
    assert_eq!(restored.pci_read_u32(0x10), dev.pci_read_u32(0x10));
    assert_eq!(restored.pci_read_u32(0x14), dev.pci_read_u32(0x14));

    // Deterministic encoding check.
    assert_eq!(restored.save_state(), snapshot);
}

#[test]
fn snapshot_rejects_unaligned_pci_bar0() {
    // Tags from `aero_io_snapshot::io::net::state::E1000DeviceState`.
    const TAG_PCI_BAR0: u16 = 2;

    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    // BAR0 must be aligned; the live model always clears the low 4 bits.
    w.field_u32(TAG_PCI_BAR0, 0xDEAD_BEEF);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("e1000 pci bar0"));
}

#[test]
fn snapshot_rejects_invalid_pci_bar1_io_flag() {
    // Tags from `aero_io_snapshot::io::net::state::E1000DeviceState`.
    const TAG_PCI_BAR1: u16 = 4;

    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    // BAR1 is an I/O BAR; bit0 must remain set.
    w.field_u32(TAG_PCI_BAR1, 0x1234_0000);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("e1000 pci bar1"));
}

#[test]
fn snapshot_rejects_out_of_range_tx_ring_indices() {
    // Tags from `aero_io_snapshot::io::net::state::E1000DeviceState`.
    const TAG_TDLEN: u16 = 52;
    const TAG_TDH: u16 = 53;

    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(TAG_TDLEN, 4 * 16);
    w.field_u32(TAG_TDH, 4); // out of range (valid: 0..=3)
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 tx ring indices")
    );
}

#[test]
fn snapshot_rejects_out_of_range_rx_ring_indices() {
    // Tags from `aero_io_snapshot::io::net::state::E1000DeviceState`.
    const TAG_RDLEN: u16 = 42;
    const TAG_RDT: u16 = 44;

    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(TAG_RDLEN, 2 * 16);
    w.field_u32(TAG_RDT, 2); // out of range (valid: 0..=1)
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 rx ring indices")
    );
}

#[test]
fn snapshot_rejects_absurd_tx_ring_len() {
    // Tags from `aero_io_snapshot::io::net::state::E1000DeviceState`.
    const TAG_TDLEN: u16 = 52;

    // The snapshot decoder caps the number of descriptors it will accept.
    // Use a ring just over the limit: 65537 descriptors * 16 bytes/desc.
    let oversized_len = 65_537u32 * 16;

    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(TAG_TDLEN, oversized_len);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 tx ring too large")
    );
}

#[test]
fn snapshot_rejects_absurd_rx_ring_len() {
    // Tags from `aero_io_snapshot::io::net::state::E1000DeviceState`.
    const TAG_RDLEN: u16 = 42;

    // The snapshot decoder caps the number of descriptors it will accept.
    // Use a ring just over the limit: 65537 descriptors * 16 bytes/desc.
    let oversized_len = 65_537u32 * 16;

    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(TAG_RDLEN, oversized_len);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 rx ring too large")
    );
}

#[test]
fn snapshot_rejects_other_regs_duplicate_key() {
    const TAG_OTHER_REGS: u16 = 90;

    // other_regs field encoding: count (u32) then count*(key u32, val u32).
    // Provide the same key twice.
    let other = Encoder::new()
        .u32(2)
        .u32(0x1234)
        .u32(0xDEAD_BEEF)
        .u32(0x1234)
        .u32(0xBEEF_DEAD)
        .finish();

    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_bytes(TAG_OTHER_REGS, other);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 other_regs duplicate key")
    );
}

#[test]
fn snapshot_rejects_inconsistent_irq_level_field() {
    // Tags from `aero_io_snapshot::io::net::state::E1000DeviceState`.
    const TAG_ICR: u16 = 20;
    const TAG_IMS: u16 = 21;
    const TAG_IRQ_LEVEL: u16 = 22;

    // Make computed IRQ level true (icr & ims != 0), but save irq_level=false.
    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(TAG_ICR, ICR_TXDW);
    w.field_u32(TAG_IMS, ICR_TXDW);
    w.field_bool(TAG_IRQ_LEVEL, false);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("e1000 irq_level"));
}

#[test]
fn snapshot_rejects_unsupported_major_version() {
    // Write a snapshot with the correct device ID but an unsupported major version.
    let unsupported = SnapshotVersion::new(<E1000Device as IoSnapshot>::DEVICE_VERSION.major + 1, 0);
    let w = SnapshotWriter::new(<E1000Device as IoSnapshot>::DEVICE_ID, unsupported);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::UnsupportedDeviceMajorVersion {
            found: unsupported.major,
            supported: <E1000Device as IoSnapshot>::DEVICE_VERSION.major,
        }
    );
}

#[test]
fn snapshot_reader_rejects_duplicate_tlv_tags() {
    // SnapshotReader should reject duplicate field tags at parse time.
    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(10, 0x1111_1111);
    w.field_u32(10, 0x2222_2222);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::DuplicateFieldTag(10));
}

#[test]
fn snapshot_rejects_wrong_device_id() {
    let wrong_id: [u8; 4] = *b"NOPE";
    let w = SnapshotWriter::new(wrong_id, <E1000Device as IoSnapshot>::DEVICE_VERSION);
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::DeviceIdMismatch {
            expected: <E1000Device as IoSnapshot>::DEVICE_ID,
            found: wrong_id,
        }
    );
}

#[test]
fn snapshot_rejects_unsupported_format_version() {
    // Snapshot header format:
    // magic (4) + fmt major (2) + fmt minor (2) + device id (4) + dev major (2) + dev minor (2).
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"AERO");
    bytes.extend_from_slice(&2u16.to_le_bytes()); // unsupported format major
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&<E1000Device as IoSnapshot>::DEVICE_ID);
    bytes.extend_from_slice(&<E1000Device as IoSnapshot>::DEVICE_VERSION.major.to_le_bytes());
    bytes.extend_from_slice(&<E1000Device as IoSnapshot>::DEVICE_VERSION.minor.to_le_bytes());

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::UnsupportedFormatVersion {
            found: SnapshotVersion::new(2, 0),
            supported: SnapshotVersion::new(1, 0),
        }
    );
}

#[test]
fn snapshot_reader_rejects_too_many_fields() {
    // SnapshotReader caps the number of TLV fields to keep parsing bounded.
    let mut w = SnapshotWriter::new(
        <E1000Device as IoSnapshot>::DEVICE_ID,
        <E1000Device as IoSnapshot>::DEVICE_VERSION,
    );
    for tag in 1..=4097u16 {
        w.field_bytes(tag, Vec::new());
    }
    let bytes = w.finish();

    let mut dev = E1000Device::new([0; 6]);
    let err = dev.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("too many fields"));
}
