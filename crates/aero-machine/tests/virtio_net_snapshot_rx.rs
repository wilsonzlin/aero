#![cfg(not(target_arch = "wasm32"))]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_devices::pci::{profile, PciBdf};
use aero_machine::{Machine, MachineConfig};
use aero_net_backend::NetworkBackend;
use aero_virtio::devices::net_offload::VirtioNetHdr;
use aero_virtio::pci::{
    VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

#[derive(Debug, Default)]
struct BackendState {
    rx: VecDeque<Vec<u8>>,
}

#[derive(Clone)]
struct TestBackend(Rc<RefCell<BackendState>>);

impl NetworkBackend for TestBackend {
    fn transmit(&mut self, _frame: Vec<u8>) {}

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.0.borrow_mut().rx.pop_front()
    }
}

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
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
        let cap_id = cfg[ptr];
        let next = cfg[ptr + 1] as usize;
        if cap_id == 0x09 {
            let cfg_type = cfg[ptr + 3];
            let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
            match cfg_type {
                VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                    caps.notify = offset;
                    caps.notify_mult =
                        u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
                }
                VIRTIO_PCI_CAP_DEVICE_CFG => caps.device = offset,
                _ => {}
            }
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
fn snapshot_restore_does_not_duplicate_cached_virtio_net_rx_buffers() {
    let cfg = MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        // Keep the machine minimal and deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg).unwrap();

    let backend_state = Rc::new(RefCell::new(BackendState::default()));
    vm.set_network_backend(Box::new(TestBackend(backend_state.clone())));

    let bdf = profile::VIRTIO_NET.bdf;

    // Enable PCI Bus Mastering so the device is allowed to DMA.
    let command = cfg_read(&mut vm, bdf, 0x04, 2) as u16;
    cfg_write(&mut vm, bdf, 0x04, 2, u32::from(command | (1 << 2)));

    // Read BAR0 base address via PCI config ports.
    let bar0_lo = cfg_read(&mut vm, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut vm, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-net BAR0 to be assigned");

    // Parse virtio vendor-specific caps to find BAR0 offsets.
    let cfg_bytes = read_config_space_256(&mut vm, bdf);
    let caps = parse_caps(&cfg_bytes);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    // Feature negotiation: accept everything the device offers.
    vm.write_physical_u8(bar0_base + caps.common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    vm.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    vm.write_physical_u32(bar0_base + caps.common, 0);
    let f0 = vm.read_physical_u32(bar0_base + caps.common + 0x04);
    vm.write_physical_u32(bar0_base + caps.common + 0x08, 0);
    vm.write_physical_u32(bar0_base + caps.common + 0x0c, f0);

    vm.write_physical_u32(bar0_base + caps.common, 1);
    let f1 = vm.read_physical_u32(bar0_base + caps.common + 0x04);
    vm.write_physical_u32(bar0_base + caps.common + 0x08, 1);
    vm.write_physical_u32(bar0_base + caps.common + 0x0c, f1);

    vm.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    vm.write_physical_u8(
        bar0_base + caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Place virtqueues in RAM above 2MiB so they are not affected by A20 wrap even if A20 is
    // disabled.
    let rx_desc = 0x200000;
    let rx_avail = 0x201000;
    let rx_used = 0x202000;

    // Configure RX queue 0.
    vm.write_physical_u16(bar0_base + caps.common + 0x16, 0);
    let rx_notify_off = vm.read_physical_u16(bar0_base + caps.common + 0x1e);
    vm.write_physical_u64(bar0_base + caps.common + 0x20, rx_desc);
    vm.write_physical_u64(bar0_base + caps.common + 0x28, rx_avail);
    vm.write_physical_u64(bar0_base + caps.common + 0x30, rx_used);
    vm.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    // Guest posts one RX buffer but backend has no frame yet. This causes the virtio-net device to
    // pop the chain and cache it internally (advancing `next_avail`) without producing a used entry.
    let rx_hdr_addr = 0x203000;
    let rx_payload_addr = 0x204000;
    vm.write_physical(rx_hdr_addr, &[0xaa; VirtioNetHdr::BASE_LEN]);
    vm.write_physical(rx_payload_addr, &[0xbb; 128]);

    write_desc(
        &mut vm,
        rx_desc,
        0,
        rx_hdr_addr,
        VirtioNetHdr::BASE_LEN as u32,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        1,
    );
    write_desc(
        &mut vm,
        rx_desc,
        1,
        rx_payload_addr,
        128,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // Initialize rings.
    vm.write_physical_u16(rx_avail, 0);
    vm.write_physical_u16(rx_avail + 2, 1);
    vm.write_physical_u16(rx_avail + 4, 0);
    vm.write_physical_u16(rx_used, 0);
    vm.write_physical_u16(rx_used + 2, 0);

    // Notify RX queue 0 and poll once. No frame is available, so used.idx should not advance.
    let rx_notify_addr =
        bar0_base + caps.notify + u64::from(rx_notify_off) * u64::from(caps.notify_mult);
    vm.write_physical_u16(rx_notify_addr, 0);
    vm.poll_network();
    assert_eq!(vm.read_physical_u16(rx_used + 2), 0);

    // Capture snapshot while the RX buffer is "in-flight" (popped by the device but not completed).
    let snap = vm.take_snapshot_full().unwrap();

    // Restore into the same machine (device instances must stay stable) and re-attach the host
    // network backend (backends are external state and are intentionally not snapshotted).
    vm.restore_snapshot_bytes(&snap).unwrap();
    vm.set_network_backend(Box::new(TestBackend(backend_state.clone())));

    // Provide *two* backend RX frames and poll. The snapshot contains only a single posted RX
    // buffer. Snapshot restore rewinds `next_avail` so that in-flight RX buffers are replayed, but
    // restore must also clear any pre-restore cached RX chains so the guest's single buffer isn't
    // duplicated (which would allow receiving both frames without the guest re-posting).
    let rx_frame1 = b"\xaa\xbb\xcc\xdd\xee\xff\x00\x01\x02\x03\x04\x05\x08\x00".to_vec();
    let rx_frame2 = b"\x11\x22\x33\x44\x55\x66\x66\x55\x44\x33\x22\x11\x08\x00".to_vec();
    backend_state.borrow_mut().rx.push_back(rx_frame1.clone());
    backend_state.borrow_mut().rx.push_back(rx_frame2.clone());
    vm.poll_network();

    assert_eq!(
        vm.read_physical_u16(rx_used + 2),
        1,
        "expected only one used entry (the snapshot only had one posted RX buffer)"
    );
    assert_eq!(
        vm.read_physical_bytes(rx_payload_addr, rx_frame1.len()),
        rx_frame1
    );
    assert_eq!(
        backend_state.borrow().rx.front().cloned(),
        Some(rx_frame2),
        "second frame should remain queued until the guest posts another RX buffer"
    );
}
