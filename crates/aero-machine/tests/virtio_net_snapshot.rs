#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;
use std::{cell::RefCell, collections::VecDeque};

use aero_devices::pci::{profile, PciBdf, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_net_backend::NetworkBackend;
use aero_platform::interrupts::InterruptController;
use aero_virtio::devices::net_offload::VirtioNetHdr;
use aero_virtio::pci::{
    VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG,
    VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER,
    VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
use pretty_assertions::{assert_eq, assert_ne};

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
    isr: u64,
    device: u64,
    notify_mult: u32,
}

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

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    // PCI config mechanism #1: 0x8000_0000 | bus<<16 | dev<<11 | fn<<8 | (offset & 0xFC)
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1f) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xfc)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
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
                // COMMON_CFG has offset 0 in our current layout; keep it for completeness.
                VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                    caps.notify = offset;
                    caps.notify_mult =
                        u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
                }
                VIRTIO_PCI_CAP_ISR_CFG => caps.isr = offset,
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
fn snapshot_restore_roundtrips_virtio_net_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        // Keep this test focused on virtio-net + PCI INTx snapshot restore behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let virtio = vm.virtio_net().expect("virtio-net enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    // Configure the PIC so a level-triggered IRQ line becomes observable as a pending vector.
    // This config is snapshotted and should be restored before we re-drive INTx.
    let (gsi, expected_vector) = {
        let bdf = profile::VIRTIO_NET.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let gsi_u8 = u8::try_from(gsi).expect("gsi must fit in ISA IRQ range for legacy PIC");
        assert!(
            gsi_u8 < 16,
            "test assumes virtio-net routes to a legacy PIC IRQ (0-15); got GSI {gsi}"
        );
        let vector = if gsi_u8 < 8 {
            0x20u8.wrapping_add(gsi_u8)
        } else {
            0x28u8.wrapping_add(gsi_u8.wrapping_sub(8))
        };

        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false); // unmask cascade
        ints.pic_mut().set_masked(gsi_u8, false); // unmask routed IRQ (GSI 10-13)

        (gsi, vector)
    };

    let bdf = profile::VIRTIO_NET.bdf;

    // Enable PCI bus mastering so the device is allowed to DMA.
    let command = cfg_read(&mut vm, bdf, 0x04, 2) as u16;
    let command = (command | (1 << 2)) & !(1 << 10);
    cfg_write(&mut vm, bdf, 0x04, 2, u32::from(command));

    // Read BAR0 base address via PCI config ports.
    let bar0_lo = cfg_read(&mut vm, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut vm, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-net BAR0 to be assigned");

    // Parse virtio vendor-specific caps to find BAR0 offsets.
    let cfg_bytes = read_config_space_256(&mut vm, bdf);
    let caps = parse_caps(&cfg_bytes);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
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
    let tx_desc = 0x203000;
    let tx_avail = 0x204000;
    let tx_used = 0x205000;

    // Configure RX queue 0 (not exercised directly by this test, but keeps the setup realistic).
    vm.write_physical_u16(bar0_base + caps.common + 0x16, 0);
    assert!(vm.read_physical_u16(bar0_base + caps.common + 0x18) >= 8);
    vm.write_physical_u64(bar0_base + caps.common + 0x20, rx_desc);
    vm.write_physical_u64(bar0_base + caps.common + 0x28, rx_avail);
    vm.write_physical_u64(bar0_base + caps.common + 0x30, rx_used);
    vm.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    // Configure TX queue 1.
    vm.write_physical_u16(bar0_base + caps.common + 0x16, 1);
    assert!(vm.read_physical_u16(bar0_base + caps.common + 0x18) >= 8);
    vm.write_physical_u64(bar0_base + caps.common + 0x20, tx_desc);
    vm.write_physical_u64(bar0_base + caps.common + 0x28, tx_avail);
    vm.write_physical_u64(bar0_base + caps.common + 0x30, tx_used);
    vm.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    // TX: header + payload.
    let hdr_addr = 0x206000;
    let payload_addr = 0x206100;
    let hdr = [0u8; VirtioNetHdr::BASE_LEN];
    let payload = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\x08\x00";
    vm.write_physical(hdr_addr, &hdr);
    vm.write_physical(payload_addr, payload);

    write_desc(
        &mut vm,
        tx_desc,
        0,
        hdr_addr,
        hdr.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        &mut vm,
        tx_desc,
        1,
        payload_addr,
        payload.len() as u32,
        0,
        0,
    );

    // Initialize rings. We intentionally do *not* write the notify register: the platform polling
    // path treats `avail.idx != next_avail` as pending work.
    vm.write_physical_u16(rx_avail, 0);
    vm.write_physical_u16(rx_avail + 2, 0);
    vm.write_physical_u16(rx_used, 0);
    vm.write_physical_u16(rx_used + 2, 0);

    vm.write_physical_u16(tx_avail, 0);
    vm.write_physical_u16(tx_avail + 2, 1);
    vm.write_physical_u16(tx_avail + 4, 0);
    vm.write_physical_u16(tx_used, 0);
    vm.write_physical_u16(tx_used + 2, 0);

    // Poll the machine once to process the TX chain and latch a virtio legacy interrupt.
    vm.poll_network();
    assert_eq!(vm.read_physical_u16(tx_used + 2), 1);
    assert!(
        virtio.borrow().irq_level(),
        "expected virtio transport to latch legacy IRQ after TX completion"
    );

    // The canonical machine snapshots the PCI INTx router, but the virtio-net INTx level is
    // surfaced through polling. We intentionally do *not* sync it pre-snapshot, so the platform
    // interrupt controller should not see it yet.
    assert_eq!(interrupts.borrow().get_pending(), None);

    let expected_virtio_state = {
        let dev = virtio.borrow();
        aero_snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            aero_snapshot::DeviceId::VIRTIO_NET,
            &*dev,
        )
    };

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind: read ISR to clear the
    // latched legacy interrupt.
    let _isr = vm.read_physical_u8(bar0_base + caps.isr);
    assert!(!virtio.borrow().irq_level());

    let mutated_virtio_state = {
        let dev = virtio.borrow();
        aero_snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            aero_snapshot::DeviceId::VIRTIO_NET,
            &*dev,
        )
    };
    assert_ne!(
        mutated_virtio_state.data, expected_virtio_state.data,
        "virtio-net state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the virtio-net instance (host wiring/backends live outside snapshots).
    let virtio_after = vm.virtio_net().expect("virtio-net still enabled");
    assert!(
        Rc::ptr_eq(&virtio, &virtio_after),
        "restore must not replace the virtio-net instance"
    );

    let restored_virtio_state = {
        let dev = virtio_after.borrow();
        aero_snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            aero_snapshot::DeviceId::VIRTIO_NET,
            &*dev,
        )
    };
    assert_eq!(restored_virtio_state.data, expected_virtio_state.data);

    // After restore, the virtio-net's asserted INTx level should be re-driven into the platform
    // interrupt sink via PCI routing.
    assert_eq!(
        interrupts.borrow().get_pending(),
        Some(expected_vector),
        "expected PCI INTx (GSI {gsi}) to deliver vector 0x{expected_vector:02x} after restore"
    );
}

#[test]
fn snapshot_restore_replays_inflight_virtio_net_rx_buffers() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        enable_e1000: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let backend_state = Rc::new(RefCell::new(BackendState::default()));
    vm.set_network_backend(Box::new(TestBackend(backend_state.clone())));

    let bdf = profile::VIRTIO_NET.bdf;

    // Enable PCI bus mastering so the device is allowed to DMA.
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

    // Place virtqueue in RAM above 2MiB so it is not affected by A20 wrap even if A20 is disabled.
    let rx_desc = 0x200000;
    let rx_avail = 0x201000;
    let rx_used = 0x202000;
    let buf_addr = 0x203000;
    let buf_len = 64u32;

    // Configure RX queue 0.
    vm.write_physical_u16(bar0_base + caps.common + 0x16, 0);
    assert!(vm.read_physical_u16(bar0_base + caps.common + 0x18) >= 8);
    vm.write_physical_u64(bar0_base + caps.common + 0x20, rx_desc);
    vm.write_physical_u64(bar0_base + caps.common + 0x28, rx_avail);
    vm.write_physical_u64(bar0_base + caps.common + 0x30, rx_used);
    vm.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    // Guest posts one RX buffer that will remain in-flight at snapshot time.
    vm.write_physical(buf_addr, &vec![0xccu8; buf_len as usize]);
    write_desc(
        &mut vm,
        rx_desc,
        0,
        buf_addr,
        buf_len,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // avail idx = 1, ring[0] = 0.
    vm.write_physical_u16(rx_avail, 0);
    vm.write_physical_u16(rx_avail + 2, 1);
    vm.write_physical_u16(rx_avail + 4, 0);

    vm.write_physical_u16(rx_used, 0);
    vm.write_physical_u16(rx_used + 2, 0);

    // Poll once with no incoming frames: transport should pop the avail entry and cache it in the
    // virtio-net device (no used entry yet).
    vm.poll_network();
    assert_eq!(vm.read_physical_u16(rx_used + 2), 0);
    let virtio = vm.virtio_net().expect("virtio-net enabled");
    let (next_avail, next_used, _event_idx) = virtio
        .borrow()
        .debug_queue_progress(0)
        .expect("rxq should be configured");
    assert_eq!(next_avail, 1);
    assert_eq!(next_used, 0);

    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate post-snapshot runtime state: deliver a frame so the cached RX buffer is consumed.
    let frame1 = vec![0x11u8; 14];
    backend_state.borrow_mut().rx.push_back(frame1.clone());
    vm.poll_network();
    assert_eq!(vm.read_physical_u16(rx_used + 2), 1);
    assert_eq!(
        vm.read_physical_u32(rx_used + 8),
        (VirtioNetHdr::BASE_LEN + frame1.len()) as u32
    );
    assert!(backend_state.borrow().rx.is_empty());

    // Restoring should rewind guest-visible state (used.idx back to 0), while also ensuring the
    // in-flight RX buffer becomes available again for host RX.
    vm.restore_snapshot_bytes(&snapshot).unwrap();
    assert_eq!(vm.read_physical_u16(rx_used + 2), 0);
    assert_eq!(
        vm.read_physical_bytes(buf_addr, buf_len as usize),
        vec![0xccu8; buf_len as usize],
        "guest RX buffer should be restored back to its pre-snapshot contents"
    );

    // Snapshots intentionally do not embed host backends, and `restore_snapshot_*` drops any
    // currently attached network backend. Reattach our test backend so RX can make progress.
    vm.set_network_backend(Box::new(TestBackend(backend_state.clone())));

    // Now deliver another frame without re-posting any RX buffers. Without the restore fix, the
    // virtio-net device has no cached buffers and the transport thinks the avail entry was
    // already consumed, so RX would stall forever.
    let frame2 = vec![0x22u8; 14];
    backend_state.borrow_mut().rx.push_back(frame2.clone());
    vm.poll_network();

    assert_eq!(vm.read_physical_u16(rx_used + 2), 1);
    assert_eq!(
        vm.read_physical_u32(rx_used + 8),
        (VirtioNetHdr::BASE_LEN + frame2.len()) as u32
    );
    assert_eq!(
        vm.read_physical_bytes(buf_addr + VirtioNetHdr::BASE_LEN as u64, frame2.len()),
        frame2
    );
    assert!(backend_state.borrow().rx.is_empty());
}
