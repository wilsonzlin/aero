#![cfg(feature = "io-snapshot")]

use aero_io_snapshot::io::net::state::{E1000DeviceState, E1000TxPacketState};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_net_e1000::{E1000Device, ICR_RXT0, ICR_TXDW, MIN_L2_FRAME_LEN};
use memory::MemoryBus;

#[derive(Clone)]
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
fn write_tx_desc(dma: &mut TestDma, addr: u64, buf_addr: u64, len: u16, cmd: u8) {
    write_u64_le(dma, addr, buf_addr);
    dma.write(addr + 8, &len.to_le_bytes());
    dma.write(addr + 10, &[0u8]); // cso
    dma.write(addr + 11, &[cmd]);
    dma.write(addr + 12, &[0u8]); // status
    dma.write(addr + 13, &[0u8]); // css
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

/// Minimal advanced TX context descriptor (16 bytes).
#[allow(clippy::too_many_arguments)]
fn write_tx_ctx_desc(
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
) {
    let mut bytes = [0u8; 16];
    bytes[0] = ipcss;
    bytes[1] = ipcso;
    bytes[2..4].copy_from_slice(&ipcse.to_le_bytes());
    bytes[4] = tucss;
    bytes[5] = tucso;
    bytes[6..8].copy_from_slice(&tucse.to_le_bytes());

    // DTYP=CTXT in byte 10, DEXT in cmd byte 11.
    bytes[10] = 0x2 << 4;
    bytes[11] = 1 << 5;

    bytes[12..14].copy_from_slice(&mss.to_le_bytes());
    bytes[14] = hdr_len;
    bytes[15] = 0; // tcp_hdr_len (unused by the device model)

    dma.write(addr, &bytes);
}

/// Minimal advanced TX data descriptor (16 bytes).
fn write_tx_data_desc(dma: &mut TestDma, addr: u64, buf_addr: u64, len: u16, cmd: u8, popts: u8) {
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&buf_addr.to_le_bytes());
    bytes[8..10].copy_from_slice(&len.to_le_bytes());
    bytes[10] = 0x3 << 4; // DTYP=DATA
    bytes[11] = cmd;
    bytes[12] = 0; // status
    bytes[13] = popts;
    dma.write(addr, &bytes);
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

fn build_test_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(MIN_L2_FRAME_LEN + payload.len());
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // dst
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // src
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
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

    frame.resize(frame.len() + payload_len, 0);
    frame
}

#[test]
fn snapshot_roundtrip_preserves_in_flight_device_state() {
    let mut dev0 = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    let mut mem0 = TestDma::new(0x20000);

    // Required for DMA (descriptor reads/writes) to work in this device model.
    dev0.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

    // Mutate PCI config space too (optional, but part of the device model).
    dev0.pci_write_u32(0x10, 0xFEBF_0000);
    dev0.pci_write_u32(0x14, 0xC000);

    // Enable interrupts (stored in IMS + ICR/irq_level).
    dev0.mmio_write_u32_reg(0x00D0, ICR_RXT0 | ICR_TXDW); // IMS
    dev0.mmio_write_u32_reg(0x00C8, ICR_TXDW); // ICS

    // Program a few registers, including an "unknown" one stored in `other_regs`.
    dev0.mmio_write_u32_reg(0x0018, 0xDEAD_BEEF); // CTRL_EXT
    dev0.mmio_write_u32_reg(0x9000, 0x1122_3344); // other_regs
    dev0.io_write_reg(0x0, 4, 0x1234_5678); // IOADDR shadow (io_reg)

    // Change MAC and RA valid bit; also mutates EEPROM words 0..2.
    dev0.mmio_write_u32_reg(0x5400, u32::from_le_bytes([0x10, 0x20, 0x30, 0x40]));
    dev0.mmio_write_u32_reg(0x5404, u32::from_le_bytes([0x50, 0x60, 0x00, 0x00])); // AV=0

    // Seed EERD/EEPROM state.
    dev0.mmio_write_u32_reg(0x0014, 1); // START + word 0

    // Seed PHY state via MDIC write.
    dev0.mmio_write_u32_reg(0x0020, (4u32 << 16) | (1u32 << 21) | 0x0400_0000 | 0xBEEF);

    // Configure TX ring at 0x1000 with 8 descriptors.
    dev0.mmio_write_u32_reg(0x3800, 0x1000); // TDBAL
    dev0.mmio_write_u32_reg(0x3804, 0); // TDBAH
    dev0.mmio_write_u32_reg(0x3808, 8 * 16); // TDLEN
    dev0.mmio_write_u32_reg(0x3810, 0); // TDH
    dev0.mmio_write_u32_reg(0x3818, 0); // TDT
    dev0.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN

    // Descriptor 0: advanced context descriptor to make tx_ctx non-default.
    write_tx_ctx_desc(
        &mut mem0,
        0x1000,
        14,
        14 + 10,
        14 + 20 - 1,
        14 + 20,
        14 + 20 + 16,
        0x1234,
        10,
        14 + 20 + 20,
    );

    // Descriptor 1: a full legacy packet (to populate tx_out).
    let frame1 = build_test_frame(b"frame1");
    mem0.write(0x2000, &frame1);
    write_tx_desc(
        &mut mem0,
        0x1010,
        0x2000,
        frame1.len() as u16,
        0b0000_1001, // EOP|RS
    );

    // Descriptor 2: first half of a legacy packet (no EOP), to populate tx_partial/tx_state.
    let frame2 = build_test_frame(b"frame2-partial");
    let split = frame2.len() / 2;
    let (frame2_a, frame2_b) = frame2.split_at(split);
    mem0.write(0x3000, frame2_a);
    write_tx_desc(
        &mut mem0,
        0x1020,
        0x3000,
        frame2_a.len() as u16,
        0b0000_1000, // RS
    );

    // Process descriptor 0 (context) + descriptor 1 (frame1).
    dev0.mmio_write_u32_reg(0x3818, 2);
    dev0.poll(&mut mem0);
    // Process descriptor 2 only (partial legacy packet).
    dev0.mmio_write_u32_reg(0x3818, 3);
    dev0.poll(&mut mem0);

    // Queue an RX frame but keep RX disabled so it stays in rx_pending.
    let rx_frame = build_test_frame(b"rx-pending");
    dev0.enqueue_rx_frame(rx_frame.clone());

    // Take snapshot and clone guest memory to simulate a VM snapshot.
    let snapshot = dev0.save_state();
    let mut mem1 = mem0.clone();

    let mut dev1 = E1000Device::new([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    dev1.load_state(&snapshot).unwrap();

    // Subsequent MMIO reads should match (including those with side-effects like ICR).
    assert_eq!(dev0.pci_read_u32(0x10), dev1.pci_read_u32(0x10));
    assert_eq!(dev0.pci_read_u32(0x14), dev1.pci_read_u32(0x14));

    assert_eq!(dev0.io_read(0x0, 4), dev1.io_read(0x0, 4)); // IOADDR shadow
    assert_eq!(dev0.mmio_read_u32(0x0018), dev1.mmio_read_u32(0x0018)); // CTRL_EXT
    assert_eq!(dev0.mmio_read_u32(0x9000), dev1.mmio_read_u32(0x9000)); // other_regs
    assert_eq!(dev0.mmio_read_u32(0x5400), dev1.mmio_read_u32(0x5400)); // RAL0
    assert_eq!(dev0.mmio_read_u32(0x5404), dev1.mmio_read_u32(0x5404)); // RAH0 (AV bit)
    assert_eq!(dev0.mmio_read_u32(0x0014), dev1.mmio_read_u32(0x0014)); // EERD state
    assert_eq!(dev0.mmio_read_u32(0x0020), dev1.mmio_read_u32(0x0020)); // MDIC state

    assert_eq!(dev0.irq_level(), dev1.irq_level());
    assert!(dev0.irq_level());
    let icr0 = dev0.mmio_read_u32(0x00C0);
    let icr1 = dev1.mmio_read_u32(0x00C0);
    assert_eq!(icr0, icr1);
    assert!(!dev0.irq_level());
    assert!(!dev1.irq_level());

    // Verify EEPROM and PHY arrays affect subsequent reads (not just EERD/MDIC registers).
    dev0.mmio_write_u32_reg(0x0014, 1 | (2 << 8));
    dev1.mmio_write_u32_reg(0x0014, 1 | (2 << 8));
    let eerd0 = dev0.mmio_read_u32(0x0014);
    let eerd1 = dev1.mmio_read_u32(0x0014);
    assert_eq!(eerd0, eerd1);
    let eeprom_word2 = (eerd0 >> 16) as u16;
    assert_eq!(eeprom_word2, u16::from_le_bytes([0x50, 0x60]));

    dev0.mmio_write_u32_reg(0x0020, (4u32 << 16) | (1u32 << 21) | 0x0800_0000);
    dev1.mmio_write_u32_reg(0x0020, (4u32 << 16) | (1u32 << 21) | 0x0800_0000);
    let mdic0 = dev0.mmio_read_u32(0x0020);
    let mdic1 = dev1.mmio_read_u32(0x0020);
    assert_eq!(mdic0 & 0xFFFF, 0xBEEF);
    assert_eq!(mdic0, mdic1);

    // pop_tx_frame() should match for frames that were queued before snapshot.
    assert_eq!(dev0.pop_tx_frame(), dev1.pop_tx_frame());
    assert_eq!(dev0.pop_tx_frame(), None);
    assert_eq!(dev1.pop_tx_frame(), None);

    // Finish the in-progress legacy packet after restore and ensure output matches.
    mem0.write(0x4000, frame2_b);
    mem1.write(0x4000, frame2_b);
    write_tx_desc(
        &mut mem0,
        0x1030,
        0x4000,
        frame2_b.len() as u16,
        0b0000_1001, // EOP|RS
    );
    write_tx_desc(
        &mut mem1,
        0x1030,
        0x4000,
        frame2_b.len() as u16,
        0b0000_1001, // EOP|RS
    );

    dev0.mmio_write_u32_reg(0x3818, 4);
    dev1.mmio_write_u32_reg(0x3818, 4);
    dev0.poll(&mut mem0);
    dev1.poll(&mut mem1);

    assert_eq!(dev0.pop_tx_frame(), dev1.pop_tx_frame());
    assert_eq!(dev0.pop_tx_frame(), None);
    assert_eq!(dev1.pop_tx_frame(), None);

    // Use the restored tx_ctx for a TSO packet and verify segmentation happens.
    let tso_frame = build_ipv4_tcp_frame(30);
    mem0.write(0x5000, &tso_frame);
    mem1.write(0x5000, &tso_frame);
    write_tx_data_desc(
        &mut mem0,
        0x1040,
        0x5000,
        tso_frame.len() as u16,
        0b1010_1001, // DEXT|TSE|EOP|RS
        0x03,        // IXSM|TXSM
    );
    write_tx_data_desc(
        &mut mem1,
        0x1040,
        0x5000,
        tso_frame.len() as u16,
        0b1010_1001, // DEXT|TSE|EOP|RS
        0x03,
    );

    dev0.mmio_write_u32_reg(0x3818, 5);
    dev1.mmio_write_u32_reg(0x3818, 5);
    dev0.poll(&mut mem0);
    dev1.poll(&mut mem1);

    let mut tso_out0 = Vec::new();
    while let Some(frame) = dev0.pop_tx_frame() {
        tso_out0.push(frame);
    }
    let mut tso_out1 = Vec::new();
    while let Some(frame) = dev1.pop_tx_frame() {
        tso_out1.push(frame);
    }
    assert_eq!(tso_out0, tso_out1);
    assert_eq!(tso_out0.len(), 3, "expected TSO segmentation");

    // Flush the snapshotted rx_pending queue and ensure delivery matches.
    write_rx_desc(&mut mem0, 0xA000, 0xB000);
    write_rx_desc(&mut mem0, 0xA010, 0xB800);
    write_rx_desc(&mut mem1, 0xA000, 0xB000);
    write_rx_desc(&mut mem1, 0xA010, 0xB800);

    for (dev, mem) in [(&mut dev0, &mut mem0), (&mut dev1, &mut mem1)] {
        dev.mmio_write_u32_reg(0x2800, 0xA000); // RDBAL
        dev.mmio_write_u32_reg(0x2804, 0); // RDBAH
        dev.mmio_write_u32_reg(0x2808, 2 * 16); // RDLEN
        dev.mmio_write_u32_reg(0x2810, 0); // RDH
        dev.mmio_write_u32_reg(0x2818, 1); // RDT
        dev.mmio_write_u32_reg(0x0100, 1 << 1); // RCTL.EN
        dev.poll(mem);
    }

    assert_eq!(mem0.read_vec(0xB000, rx_frame.len()), rx_frame);
    assert_eq!(mem1.read_vec(0xB000, rx_frame.len()), rx_frame);

    assert_eq!(dev0.irq_level(), dev1.irq_level());
    assert!(dev0.irq_level());
    let icr0 = dev0.mmio_read_u32(0x00C0);
    let icr1 = dev1.mmio_read_u32(0x00C0);
    assert_eq!(icr0 & ICR_RXT0, ICR_RXT0);
    assert_eq!(icr0, icr1);
}

#[test]
fn snapshot_state_restore_state_roundtrip_preserves_state_and_continues_dma() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    let mut mem = TestDma::new(0x80_000);
    dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

    // Program a few non-default guest-visible registers.
    dev.mmio_write_u32_reg(0x00D0, ICR_RXT0 | ICR_TXDW); // IMS
    dev.mmio_write_u32_reg(0x0018, 0x1234_5678); // CTRL_EXT
    dev.mmio_write_u32_reg(0x1234, 0xDEAD_BEEF); // other_regs

    // Change MAC + set AV.
    dev.mmio_write_u32_reg(0x5400, u32::from_le_bytes([0x10, 0x11, 0x12, 0x13]));
    dev.mmio_write_u32_reg(0x5404, u32::from_le_bytes([0x14, 0x15, 0, 0]) | (1u32 << 31));

    // Exercise IOADDR shadow state.
    dev.io_write_reg(0x0, 4, 0x0400); // select TCTL

    // Configure RX ring: 2 descriptors at 0x2000.
    dev.mmio_write_u32_reg(0x2800, 0x2000); // RDBAL
    dev.mmio_write_u32_reg(0x2804, 0); // RDBAH
    dev.mmio_write_u32_reg(0x2808, 2 * 16); // RDLEN
    dev.mmio_write_u32_reg(0x2810, 0); // RDH
    dev.mmio_write_u32_reg(0x2818, 1); // RDT
    dev.mmio_write_u32_reg(0x0100, 1 << 1); // RCTL.EN
    write_rx_desc(&mut mem, 0x2000, 0x3000);
    write_rx_desc(&mut mem, 0x2010, 0x3400);

    // Configure TX ring: 4 descriptors at 0x1000.
    dev.mmio_write_u32_reg(0x3800, 0x1000); // TDBAL
    dev.mmio_write_u32_reg(0x3804, 0); // TDBAH
    dev.mmio_write_u32_reg(0x3808, 4 * 16); // TDLEN
    dev.mmio_write_u32_reg(0x3810, 0); // TDH
    dev.mmio_write_u32_reg(0x3818, 0); // TDT
    dev.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN

    // Descriptor 0: a complete packet that ends up in tx_out.
    let tx_frame_a = build_test_frame(b"tx-a");
    mem.write(0x6000, &tx_frame_a);
    write_tx_desc(
        &mut mem,
        0x1000,
        0x6000,
        tx_frame_a.len() as u16,
        0b0000_1001, // EOP|RS
    );

    // Descriptor 1: first half of a packet (no EOP) so we snapshot in-flight TX state.
    let tx_frame_b = build_test_frame(b"tx-b-split-across-descriptors");
    let split = tx_frame_b.len() / 2;
    let (tx_b0, tx_b1) = tx_frame_b.split_at(split);
    mem.write(0x4000, tx_b0);
    mem.write(0x5000, tx_b1);
    write_tx_desc(
        &mut mem,
        0x1010,
        0x4000,
        tx_b0.len() as u16,
        0b0000_1000, // RS
    );

    // Process desc0+desc1, leaving desc2 pending.
    dev.mmio_write_u32_reg(0x3818, 2);
    dev.poll(&mut mem);

    // Disable TX before advertising the final descriptor.
    dev.mmio_write_u32_reg(0x0400, 0);
    write_tx_desc(
        &mut mem,
        0x1020,
        0x5000,
        tx_b1.len() as u16,
        0b0000_1001, // EOP|RS
    );
    dev.mmio_write_u32_reg(0x3818, 3);

    // Queue an RX frame but don't flush it yet.
    let rx_frame = build_test_frame(b"rx-pending");
    dev.enqueue_rx_frame(rx_frame.clone());

    let irq_before = dev.irq_level();
    let snap = dev.snapshot_state();

    assert_eq!(snap.tx_out, vec![tx_frame_a.clone()]);
    assert_eq!(snap.rx_pending, vec![rx_frame.clone()]);
    assert_eq!(snap.tx_partial, tx_b0);
    assert_eq!(snap.tctl, 0);
    assert_eq!(snap.tdh, 2);
    assert_eq!(snap.tdt, 3);
    assert!(matches!(snap.tx_state, E1000TxPacketState::Legacy { .. }));

    // Ensure roundtrip TLV encoding/decoding is stable.
    let bytes = snap.save_state();
    let mut decoded = E1000DeviceState::default();
    decoded.load_state(&bytes).unwrap();
    assert_eq!(decoded, snap);

    let mem_snap = mem.clone();

    let mut restored = E1000Device::new([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    restored.restore_state(&decoded);
    assert_eq!(restored.snapshot_state(), snap);
    assert_eq!(restored.irq_level(), irq_before);

    let run_to_completion =
        |dev: &mut E1000Device, mem: &mut TestDma| -> (Vec<Vec<u8>>, Vec<u8>) {
            // Re-enable TX and let poll process TX + flush RX.
            dev.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN
            dev.poll(mem);

            let mut tx_frames = Vec::new();
            while let Some(frame) = dev.pop_tx_frame() {
                tx_frames.push(frame);
            }

            assert_eq!(dev.mmio_read_u32(0x2810), 1, "expected one RX descriptor to be consumed");
            let rx_buf = mem.read_vec(0x3000, rx_frame.len());
            (tx_frames, rx_buf)
        };

    let (expected_tx, expected_rx) = run_to_completion(&mut dev, &mut mem);
    let mut mem2 = mem_snap;
    let (actual_tx, actual_rx) = run_to_completion(&mut restored, &mut mem2);

    assert_eq!(actual_tx, expected_tx);
    assert_eq!(actual_rx, expected_rx);
    assert_eq!(expected_tx, vec![tx_frame_a, tx_frame_b]);
    assert_eq!(expected_rx, rx_frame);
}
