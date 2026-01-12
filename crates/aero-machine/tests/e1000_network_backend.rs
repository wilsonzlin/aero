use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_devices::pci::PciBdf;
use aero_machine::{Machine, MachineConfig};
use aero_net_backend::NetworkBackend;
use aero_net_e1000::MIN_L2_FRAME_LEN;
use pretty_assertions::assert_eq;

#[derive(Default)]
struct StubNetState {
    tx: Vec<Vec<u8>>,
    rx: VecDeque<Vec<u8>>,
}

struct StubNetBackend {
    state: Rc<RefCell<StubNetState>>,
}

impl NetworkBackend for StubNetBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.state.borrow_mut().tx.push(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.state.borrow_mut().rx.pop_front()
    }
}

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read_u32(m: &mut Machine, bdf: PciBdf, offset: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_read(0xCFC, 4)
}

fn cfg_read_u16(m: &mut Machine, bdf: PciBdf, offset: u8) -> u16 {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_read(0xCFC + u16::from(offset & 3), 2) as u16
}

fn cfg_write_u16(m: &mut Machine, bdf: PciBdf, offset: u8, value: u16) {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_write(0xCFC + u16::from(offset & 3), 2, u32::from(value));
}

#[test]
fn e1000_pci_mmio_tx_rx_are_bridged_via_network_backend_and_gated_by_bme() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_e1000: true,
        // Keep the machine minimal and deterministic for this integration test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let state = Rc::new(RefCell::new(StubNetState::default()));
    m.set_network_backend(Box::new(StubNetBackend { state: state.clone() }));

    let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;

    // BAR0 must be assigned by PCI BIOS POST during machine reset.
    let bar0 = cfg_read_u32(&mut m, bdf, 0x10);
    let bar0_base = u64::from(bar0 & 0xFFFF_FFF0);
    assert!(bar0_base != 0, "expected non-zero BAR0 base after BIOS POST");

    // ----------------------
    // TX: BME gating (BME=0)
    // ----------------------
    let tx_desc_base: u64 = 0x20_000;
    let tx_buf: u64 = 0x21_000;

    // Minimal Ethernet frame (no FCS).
    let tx_frame: Vec<u8> = (0..MIN_L2_FRAME_LEN).map(|i| i as u8).collect();
    assert_eq!(tx_frame.len(), MIN_L2_FRAME_LEN);

    m.write_physical(tx_buf, &tx_frame);

    // Legacy TX descriptor (16 bytes).
    // - buffer addr: tx_buf
    // - length: tx_frame.len()
    // - cmd: EOP|RS
    let mut tx_desc = [0u8; 16];
    tx_desc[0..8].copy_from_slice(&tx_buf.to_le_bytes());
    tx_desc[8..10].copy_from_slice(&(tx_frame.len() as u16).to_le_bytes());
    tx_desc[11] = 0x01 | 0x08; // EOP | RS
    m.write_physical(tx_desc_base, &tx_desc);

    // Program TX ring registers via MMIO.
    // TDBAL/TDBAH/TDLEN/TDH/TDT/TCTL.
    m.write_physical_u32(bar0_base + 0x3800, tx_desc_base as u32);
    m.write_physical_u32(bar0_base + 0x3804, 0);
    m.write_physical_u32(bar0_base + 0x3808, 16 * 4); // 4 descriptors
    m.write_physical_u32(bar0_base + 0x3810, 0);
    m.write_physical_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN
    m.write_physical_u32(bar0_base + 0x3818, 1); // TDT = 1 (one descriptor ready)

    // With Bus Master Enable still clear, polling must not DMA or transmit.
    m.poll_network();
    assert!(state.borrow().tx.is_empty(), "unexpected TX with BME=0");
    let desc_after_bme0 = m.read_physical_bytes(tx_desc_base, 16);
    assert_eq!(desc_after_bme0[12] & 0x01, 0, "DD should remain clear with BME=0");

    // -------------------
    // TX: enable BME, poll
    // -------------------
    let cmd = cfg_read_u16(&mut m, bdf, 0x04);
    cfg_write_u16(&mut m, bdf, 0x04, cmd | (1 << 2)); // Bus Master Enable

    m.poll_network();

    let tx = state.borrow().tx.clone();
    assert_eq!(tx.len(), 1);
    assert_eq!(tx[0], tx_frame);

    let desc_after = m.read_physical_bytes(tx_desc_base, 16);
    assert_eq!(desc_after[12] & 0x01, 0x01, "DD should be set after TX DMA");

    // Disable bus mastering again: DMA must stop immediately, but host->guest frames should still be
    // queued internally for later delivery.
    let cmd = cfg_read_u16(&mut m, bdf, 0x04);
    cfg_write_u16(&mut m, bdf, 0x04, cmd & !(1 << 2));

    // ----------------------
    // RX: inject from backend
    // ----------------------
    let rx_desc_base: u64 = 0x22_000;
    let rx_buf: u64 = 0x23_000;

    // Two RX descriptors (capacity of 1 due to head==tail full/empty rule).
    let mut rx_desc0 = [0u8; 16];
    rx_desc0[0..8].copy_from_slice(&rx_buf.to_le_bytes());
    m.write_physical(rx_desc_base, &rx_desc0);
    m.write_physical(rx_desc_base + 16, &[0u8; 16]); // unused desc1

    // Program RX ring registers via MMIO.
    m.write_physical_u32(bar0_base + 0x2800, rx_desc_base as u32);
    m.write_physical_u32(bar0_base + 0x2804, 0);
    m.write_physical_u32(bar0_base + 0x2808, 16 * 2); // 2 descriptors
    m.write_physical_u32(bar0_base + 0x2810, 0); // RDH
    m.write_physical_u32(bar0_base + 0x2818, 1); // RDT
    m.write_physical_u32(bar0_base + 0x0100, 1 << 1); // RCTL.EN (2048-byte buffers)

    let rx_frame: Vec<u8> = (0..MIN_L2_FRAME_LEN).rev().map(|i| i as u8).collect();
    // Fill the RX buffer with a sentinel value so we can detect unexpected DMA while BME=0.
    m.write_physical(rx_buf, &vec![0xaa; rx_frame.len()]);
    state.borrow_mut().rx.push_back(rx_frame.clone());

    // With BME=0, polling must not DMA into guest memory or update RX descriptors.
    m.poll_network();

    let rx_buf_before_bme = m.read_physical_bytes(rx_buf, rx_frame.len());
    assert_eq!(rx_buf_before_bme, vec![0xaa; rx_frame.len()]);
    let rx_desc_after_bme0 = m.read_physical_bytes(rx_desc_base, 16);
    assert_eq!(rx_desc_after_bme0[12] & 0x03, 0, "RX desc should not complete while BME=0");

    // Re-enable BME: the queued frame should now be DMA'd into the guest RX ring.
    let cmd = cfg_read_u16(&mut m, bdf, 0x04);
    cfg_write_u16(&mut m, bdf, 0x04, cmd | (1 << 2));
    m.poll_network();

    let rx_buf_bytes = m.read_physical_bytes(rx_buf, rx_frame.len());
    assert_eq!(rx_buf_bytes, rx_frame);

    let rx_desc_after = m.read_physical_bytes(rx_desc_base, 16);
    let rx_len = u16::from_le_bytes([rx_desc_after[8], rx_desc_after[9]]) as usize;
    assert_eq!(rx_len, rx_frame.len());
    let status = rx_desc_after[12];
    assert_eq!(status & 0x03, 0x03, "RX descriptor should have DD|EOP set");
}
