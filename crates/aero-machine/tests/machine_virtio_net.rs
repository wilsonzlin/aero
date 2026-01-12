use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_net_backend::NetworkBackend;
use aero_virtio::devices::net_offload::VirtioNetHdr;
use aero_virtio::pci::{
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
use pretty_assertions::assert_eq;

#[derive(Default, Debug)]
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

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn write_desc(m: &mut Machine, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u64::from(index) * 16;
    m.write_physical_u64(base, addr);
    m.write_physical_u32(base + 8, len);
    m.write_physical_u16(base + 12, flags);
    m.write_physical_u16(base + 14, next);
}

#[test]
fn virtio_net_pci_tx_and_rx_reach_network_backend_and_are_gated_by_bme() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        enable_e1000: false,
        // Keep the machine minimal and deterministic for this integration test.
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();
    enable_a20(&mut m);

    let state = Rc::new(RefCell::new(StubNetState::default()));
    m.set_network_backend(Box::new(StubNetBackend {
        state: state.clone(),
    }));

    let bdf = profile::VIRTIO_NET.bdf;
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");

    // Resolve BAR0 MMIO base address (BAR0 is 64-bit MMIO).
    let (bar0_lo, bar0_hi) = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        (bus.read_config(bdf, 0x10, 4), bus.read_config(bdf, 0x14, 4))
    };
    let bar0_base = (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xFFFF_FFF0);
    assert_ne!(bar0_base, 0, "BAR0 must be assigned by platform PCI POST");

    // Ensure memory decoding is enabled so BAR0 MMIO accesses are routed, but keep Bus Master
    // Enable (BME) clear initially.
    {
        let cmd = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg.bus_mut().read_config(bdf, 0x04, 2) as u16
        };
        let cmd = cmd | (1 << 1); // MEMORY_SPACE
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, 0x04, 2, u32::from(cmd));
    }

    const COMMON: u64 = 0x0000;
    const NOTIFY: u64 = 0x1000;
    const DEVICE: u64 = 0x3000;
    const NOTIFY_MULT: u64 = 4;

    // --------------------
    // Feature negotiation.
    // --------------------
    m.write_physical_u8(bar0_base + COMMON + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    m.write_physical_u8(
        bar0_base + COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    // Accept all features the device offers.
    m.write_physical_u32(bar0_base + COMMON, 0);
    let f0 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 0);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f0);

    m.write_physical_u32(bar0_base + COMMON, 1);
    let f1 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 1);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f1);

    m.write_physical_u8(
        bar0_base + COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    m.write_physical_u8(
        bar0_base + COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Contract v1 config layout: mac + status + max_virtqueue_pairs.
    let cfg = m.read_physical_bytes(bar0_base + DEVICE, 10);
    assert_eq!(cfg.len(), 10);
    assert_ne!(cfg[6] & 1, 0, "expected VIRTIO_NET_S_LINK_UP");

    // ----------------
    // Configure queues
    // ----------------
    // Place all virtqueue structures well above the low-memory firmware scratch space so BIOS POST
    // cannot accidentally overlap with our synthetic rings.
    let rx_desc = 0x0080_0000;
    let rx_avail = 0x0081_0000;
    let rx_used = 0x0082_0000;
    let tx_desc = 0x0090_0000;
    let tx_avail = 0x0091_0000;
    let tx_used = 0x0092_0000;

    // Clear queue memory to deterministic zeros (descriptor tables + avail/used rings).
    let zero_page = vec![0u8; 0x1000];
    m.write_physical(rx_desc, &zero_page);
    m.write_physical(rx_avail, &zero_page);
    m.write_physical(rx_used, &zero_page);
    m.write_physical(tx_desc, &zero_page);
    m.write_physical(tx_avail, &zero_page);
    m.write_physical(tx_used, &zero_page);

    // RX queue 0.
    m.write_physical_u16(bar0_base + COMMON + 0x16, 0);
    let rx_qsize = m.read_physical_u16(bar0_base + COMMON + 0x18);
    assert!(
        rx_qsize >= 8,
        "expected virtqueue size >= 8, got {rx_qsize}"
    );
    m.write_physical_u64(bar0_base + COMMON + 0x20, rx_desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, rx_avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, rx_used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1);

    // TX queue 1.
    m.write_physical_u16(bar0_base + COMMON + 0x16, 1);
    let tx_qsize = m.read_physical_u16(bar0_base + COMMON + 0x18);
    assert!(
        tx_qsize >= 8,
        "expected virtqueue size >= 8, got {tx_qsize}"
    );
    m.write_physical_u64(bar0_base + COMMON + 0x20, tx_desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, tx_avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, tx_used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1);

    // ----------------------
    // TX: BME gating (BME=0)
    // ----------------------
    let tx_hdr_addr = 0x00a0_0000;
    let tx_payload_addr = 0x00a0_0100;
    let tx_hdr = [0u8; VirtioNetHdr::BASE_LEN];
    let tx_frame = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\x08\x00";
    m.write_physical(tx_hdr_addr, &tx_hdr);
    m.write_physical(tx_payload_addr, tx_frame);

    write_desc(
        &mut m,
        tx_desc,
        0,
        tx_hdr_addr,
        tx_hdr.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        &mut m,
        tx_desc,
        1,
        tx_payload_addr,
        tx_frame.len() as u32,
        0,
        0,
    );

    // avail idx = 1, ring[0] = 0
    m.write_physical_u16(tx_avail, 0);
    m.write_physical_u16(tx_avail + 2, 1);
    m.write_physical_u16(tx_avail + 4, 0);
    // used idx = 0
    m.write_physical_u16(tx_used, 0);
    m.write_physical_u16(tx_used + 2, 0);

    // Kick queue 1.
    m.write_physical_u16(bar0_base + NOTIFY + NOTIFY_MULT, 1);
    m.poll_network();

    assert!(
        state.borrow().tx.is_empty(),
        "unexpected TX while Bus Master Enable is clear"
    );
    assert_eq!(
        m.read_physical_u16(tx_used + 2),
        0,
        "unexpected used ring advance while BME=0"
    );

    // Enable bus mastering and poll again: TX should complete.
    {
        let cmd = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg.bus_mut().read_config(bdf, 0x04, 2) as u16
        };
        let cmd = cmd | (1 << 2); // BUS_MASTER
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().write_config(bdf, 0x04, 2, u32::from(cmd));
    }
    m.poll_network();

    let tx = state.borrow().tx.clone();
    assert_eq!(tx, vec![tx_frame.to_vec()]);
    assert_eq!(m.read_physical_u16(tx_used + 2), 1);

    // ----------------------
    // RX: host -> guest frame
    // ----------------------
    let rx_hdr_addr = 0x00b0_0000;
    let rx_payload_addr = 0x00b0_0100;
    m.write_physical(rx_hdr_addr, &[0xaa; VirtioNetHdr::BASE_LEN]);
    m.write_physical(rx_payload_addr, &[0xbb; 64]);

    write_desc(
        &mut m,
        rx_desc,
        0,
        rx_hdr_addr,
        VirtioNetHdr::BASE_LEN as u32,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        1,
    );
    write_desc(
        &mut m,
        rx_desc,
        1,
        rx_payload_addr,
        64,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // avail idx = 1, ring[0] = 0
    m.write_physical_u16(rx_avail, 0);
    m.write_physical_u16(rx_avail + 2, 1);
    m.write_physical_u16(rx_avail + 4, 0);
    // used idx = 0
    m.write_physical_u16(rx_used, 0);
    m.write_physical_u16(rx_used + 2, 0);

    // Kick queue 0 and process once (no host frame yet).
    m.write_physical_u16(bar0_base + NOTIFY, 0);
    m.poll_network();
    assert_eq!(m.read_physical_u16(rx_used + 2), 0);

    let rx_frame = b"\xaa\xbb\xcc\xdd\xee\xff\x00\x01\x02\x03\x04\x05\x08\x00".to_vec();
    state.borrow_mut().rx.push_back(rx_frame.clone());
    m.poll_network();

    assert_eq!(m.read_physical_u16(rx_used + 2), 1);
    assert_eq!(
        m.read_physical_u32(rx_used + 8),
        (VirtioNetHdr::BASE_LEN + rx_frame.len()) as u32
    );
    assert_eq!(
        m.read_physical_bytes(rx_hdr_addr, VirtioNetHdr::BASE_LEN),
        vec![0u8; VirtioNetHdr::BASE_LEN]
    );
    assert_eq!(
        m.read_physical_bytes(rx_payload_addr, rx_frame.len()),
        rx_frame
    );
}
