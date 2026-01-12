#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_devices::pci::PciBdf;
use aero_machine::{Machine, MachineConfig};
use aero_net_backend::NetworkBackend;
use aero_virtio::devices::net_offload::VirtioNetHdr;
use aero_virtio::pci::{
    VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG,
    VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

#[derive(Debug, Default)]
struct BackendState {
    tx: Vec<Vec<u8>>,
    rx: VecDeque<Vec<u8>>,
}

#[derive(Clone)]
struct TestBackend(Rc<RefCell<BackendState>>);

impl NetworkBackend for TestBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.0.borrow_mut().tx.push(frame);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.0.borrow_mut().rx.pop_front()
    }
}

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
    isr: u64,
    device: u64,
    notify_mult: u32,
}

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    // PCI config mechanism #1: 0x8000_0000 | bus<<16 | dev<<11 | fn<<8 | (offset & 0xFC)
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1f) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xfc)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_read(0xCFC + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_write(0xCFC + (offset & 3), size, value);
}

fn read_config_space_256(m: &mut Machine, bdf: PciBdf) -> [u8; 256] {
    let mut out = [0u8; 256];
    for off in (0..256u16).step_by(4) {
        let v = cfg_read(m, bdf, off, 4);
        out[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

fn parse_caps(cfg: &[u8; 256]) -> Caps {
    let mut caps = Caps::default();
    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        assert_eq!(cfg[ptr], 0x09, "expected vendor-specific PCI cap");
        let next = cfg[ptr + 1] as usize;
        let cfg_type = cfg[ptr + 3];
        let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
        match cfg_type {
            VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
            VIRTIO_PCI_CAP_NOTIFY_CFG => {
                caps.notify = offset;
                caps.notify_mult = u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
            }
            VIRTIO_PCI_CAP_ISR_CFG => caps.isr = offset,
            VIRTIO_PCI_CAP_DEVICE_CFG => caps.device = offset,
            _ => {}
        }
        ptr = next;
    }
    caps
}

fn write_desc(m: &mut Machine, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u64::from(index) * 16;
    m.write_physical_u64(base, addr);
    m.write_physical_u32(base + 8, len);
    m.write_physical_u16(base + 12, flags);
    m.write_physical_u16(base + 14, next);
}

#[test]
fn virtio_net_tx_and_rx_complete_via_machine_network_backend() {
    let cfg = MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        virtio_net_mac_addr: Some([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]),
        enable_e1000: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();

    let state = Rc::new(RefCell::new(BackendState::default()));
    m.set_network_backend(Box::new(TestBackend(state.clone())));

    let bdf = aero_devices::pci::profile::VIRTIO_NET.bdf;

    // Start with PCI Bus Mastering disabled. We'll prove that TX is gated on COMMAND.BME (bit 2)
    // by attempting to transmit once (expecting no DMA), then enabling BME and retrying.
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command & !(1 << 2)));

    // Read BAR0 base address via PCI config ports.
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-net BAR0 to be assigned");

    // Parse virtio vendor-specific caps to find BAR0 offsets.
    let cfg_bytes = read_config_space_256(&mut m, bdf);
    let caps = parse_caps(&cfg_bytes);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    // Feature negotiation: accept everything the device offers.
    m.write_physical_u8(bar0_base + caps.common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    m.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    m.write_physical_u32(bar0_base + caps.common, 0);
    let f0 = m.read_physical_u32(bar0_base + caps.common + 0x04);
    m.write_physical_u32(bar0_base + caps.common + 0x08, 0);
    m.write_physical_u32(bar0_base + caps.common + 0x0c, f0);

    m.write_physical_u32(bar0_base + caps.common, 1);
    let f1 = m.read_physical_u32(bar0_base + caps.common + 0x04);
    m.write_physical_u32(bar0_base + caps.common + 0x08, 1);
    m.write_physical_u32(bar0_base + caps.common + 0x0c, f1);

    m.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    m.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Contract v1 config layout: mac + status + max_virtqueue_pairs.
    let dev_cfg = m.read_physical_bytes(bar0_base + caps.device, 10);
    assert_eq!(&dev_cfg[0..6], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    // Place virtqueues in RAM above 2MiB so they are not affected by A20 wrap even if A20 is
    // disabled.
    let rx_desc = 0x200000;
    let rx_avail = 0x201000;
    let rx_used = 0x202000;
    let tx_desc = 0x203000;
    let tx_avail = 0x204000;
    let tx_used = 0x205000;

    // Configure RX queue 0.
    m.write_physical_u16(bar0_base + caps.common + 0x16, 0);
    assert!(m.read_physical_u16(bar0_base + caps.common + 0x18) >= 8);
    m.write_physical_u64(bar0_base + caps.common + 0x20, rx_desc);
    m.write_physical_u64(bar0_base + caps.common + 0x28, rx_avail);
    m.write_physical_u64(bar0_base + caps.common + 0x30, rx_used);
    m.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    // Configure TX queue 1.
    m.write_physical_u16(bar0_base + caps.common + 0x16, 1);
    assert!(m.read_physical_u16(bar0_base + caps.common + 0x18) >= 8);
    m.write_physical_u64(bar0_base + caps.common + 0x20, tx_desc);
    m.write_physical_u64(bar0_base + caps.common + 0x28, tx_avail);
    m.write_physical_u64(bar0_base + caps.common + 0x30, tx_used);
    m.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    // TX: header + payload.
    let hdr_addr = 0x206000;
    let payload_addr = 0x206100;
    let hdr = [0u8; VirtioNetHdr::BASE_LEN];
    let payload = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\x08\x00";
    m.write_physical(hdr_addr, &hdr);
    m.write_physical(payload_addr, payload);

    write_desc(
        &mut m,
        tx_desc,
        0,
        hdr_addr,
        hdr.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(&mut m, tx_desc, 1, payload_addr, payload.len() as u32, 0, 0);

    m.write_physical_u16(tx_avail, 0);
    m.write_physical_u16(tx_avail + 2, 1);
    m.write_physical_u16(tx_avail + 4, 0);
    m.write_physical_u16(tx_used, 0);
    m.write_physical_u16(tx_used + 2, 0);

    // Notify TX queue 1 and poll the machine once.
    m.write_physical_u16(bar0_base + caps.notify + u64::from(caps.notify_mult), 1);
    m.poll_network();

    assert!(state.borrow().tx.is_empty(), "TX should be gated while BME=0");
    assert_eq!(m.read_physical_u16(tx_used + 2), 0);

    // Enable PCI Bus Mastering so the device is allowed to DMA.
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command | (1 << 2)));

    // Notify TX queue 1 again and poll.
    m.write_physical_u16(bar0_base + caps.notify + u64::from(caps.notify_mult), 1);
    m.poll_network();

    assert_eq!(state.borrow().tx, vec![payload.to_vec()]);
    assert_eq!(m.read_physical_u16(tx_used + 2), 1);
    assert_eq!(m.read_physical_u32(tx_used + 8), 0);

    // RX: guest posts a buffer, then host delivers a packet later.
    let rx_hdr_addr = 0x207000;
    let rx_payload_addr = 0x207100;
    m.write_physical(rx_hdr_addr, &[0xaa; VirtioNetHdr::BASE_LEN]);
    m.write_physical(rx_payload_addr, &[0xbb; 64]);

    write_desc(
        &mut m,
        rx_desc,
        0,
        rx_hdr_addr,
        hdr.len() as u32,
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

    m.write_physical_u16(rx_avail, 0);
    m.write_physical_u16(rx_avail + 2, 1);
    m.write_physical_u16(rx_avail + 4, 0);
    m.write_physical_u16(rx_used, 0);
    m.write_physical_u16(rx_used + 2, 0);

    m.write_physical_u16(bar0_base + caps.notify, 0);
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
