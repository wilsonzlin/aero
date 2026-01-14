#![cfg(not(target_arch = "wasm32"))]

use std::rc::Rc;

use aero_devices::pci::{
    profile, PciBdf, PciDevice, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::InterruptController;
use aero_virtio::devices::blk::{VIRTIO_BLK_SECTOR_SIZE, VIRTIO_BLK_T_IN};
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
fn snapshot_restore_roundtrips_virtio_blk_state_and_redrives_intx_level() {
    let mut vm = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep this test focused on virtio-blk + PCI INTx snapshot restore behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let virtio_blk = vm.virtio_blk().expect("virtio-blk enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");
    let pci_intx = vm.pci_intx_router().expect("pc platform enabled");

    // Configure the PIC so a level-triggered IRQ line becomes observable as a pending vector.
    // This config is snapshotted and should be restored before we re-drive INTx.
    let (gsi, expected_vector) = {
        let bdf = profile::VIRTIO_BLK.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let gsi_u8 = u8::try_from(gsi).expect("gsi must fit in ISA IRQ range for legacy PIC");
        assert!(
            gsi_u8 < 16,
            "test assumes virtio-blk routes to a legacy PIC IRQ (0-15); got GSI {gsi}"
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

    let bdf = profile::VIRTIO_BLK.bdf;

    // Enable PCI memory decoding + bus mastering so the device is reachable and allowed to DMA.
    let command = cfg_read(&mut vm, bdf, 0x04, 2) as u16;
    let command = (command | (1 << 1) | (1 << 2)) & !(1 << 10);
    cfg_write(&mut vm, bdf, 0x04, 2, u32::from(command));

    // Read BAR0 base address via PCI config ports.
    let bar0_lo = cfg_read(&mut vm, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut vm, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-blk BAR0 to be assigned");

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
    let desc = 0x200000;
    let avail = 0x201000;
    let used = 0x202000;

    // Configure queue 0.
    vm.write_physical_u16(bar0_base + caps.common + 0x16, 0);
    assert!(vm.read_physical_u16(bar0_base + caps.common + 0x18) >= 8);
    vm.write_physical_u64(bar0_base + caps.common + 0x20, desc);
    vm.write_physical_u64(bar0_base + caps.common + 0x28, avail);
    vm.write_physical_u64(bar0_base + caps.common + 0x30, used);
    vm.write_physical_u16(bar0_base + caps.common + 0x1c, 1);

    // Build a single READ request (one sector) so the queue completes and asserts legacy INTx.
    let hdr_addr = 0x203000;
    let data_addr = 0x204000;
    let status_addr = 0x205000;

    // virtio-blk request header: type:u32, reserved:u32, sector:u64
    let mut hdr = [0u8; 16];
    hdr[0..4].copy_from_slice(&VIRTIO_BLK_T_IN.to_le_bytes());
    hdr[4..8].copy_from_slice(&0u32.to_le_bytes());
    hdr[8..16].copy_from_slice(&0u64.to_le_bytes());
    vm.write_physical(hdr_addr, &hdr);
    vm.write_physical(data_addr, &[0xAA; VIRTIO_BLK_SECTOR_SIZE as usize]);
    vm.write_physical(status_addr, &[0xFF]);

    write_desc(
        &mut vm,
        desc,
        0,
        hdr_addr,
        hdr.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        &mut vm,
        desc,
        1,
        data_addr,
        VIRTIO_BLK_SECTOR_SIZE as u32,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        2,
    );
    write_desc(&mut vm, desc, 2, status_addr, 1, VIRTQ_DESC_F_WRITE, 0);

    // Initialize rings.
    vm.write_physical_u16(avail, 0); // flags
    vm.write_physical_u16(avail + 2, 1); // idx
    vm.write_physical_u16(avail + 4, 0); // ring[0] = desc 0

    vm.write_physical_u16(used, 0); // flags
    vm.write_physical_u16(used + 2, 0); // idx

    // Process the request once; this should advance used.idx and latch a virtio legacy interrupt.
    vm.process_virtio_blk();
    assert_eq!(vm.read_physical_u16(used + 2), 1);
    assert!(
        virtio_blk.borrow().irq_level(),
        "expected virtio transport to latch legacy IRQ after blk completion"
    );

    // Mirror the canonical PCI config-space state into the virtio transport before taking an
    // expected serialized blob, matching the behavior of `Machine::device_states`.
    {
        let mut dev = virtio_blk.borrow_mut();
        dev.set_pci_command(command);
        dev.config_mut().set_bar_base(0, bar0_base);
    }

    // The canonical machine snapshots the PCI INTx router, but the virtio-blk INTx level is
    // surfaced through polling. We intentionally do *not* sync it pre-snapshot, so the platform
    // interrupt controller should not see it yet.
    assert_eq!(interrupts.borrow().get_pending(), None);

    let expected_virtio_state = virtio_blk.borrow().save_state();
    let snapshot = vm.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind: read ISR to clear the
    // latched legacy interrupt.
    let _isr = vm.read_physical_u8(bar0_base + caps.isr);
    assert!(!virtio_blk.borrow().irq_level());

    let mutated_virtio_state = virtio_blk.borrow().save_state();
    assert_ne!(
        mutated_virtio_state, expected_virtio_state,
        "virtio-blk state mutation did not change serialized state; test is not effective"
    );

    vm.restore_snapshot_bytes(&snapshot).unwrap();

    // Restore should not replace the virtio-blk instance (host wiring/backends live outside snapshots).
    let virtio_after = vm.virtio_blk().expect("virtio-blk still enabled");
    assert!(
        Rc::ptr_eq(&virtio_blk, &virtio_after),
        "restore must not replace the virtio-blk instance"
    );

    assert_eq!(virtio_after.borrow().save_state(), expected_virtio_state);

    // After restore, the virtio-blk's asserted INTx level should be re-driven into the platform
    // interrupt sink via PCI routing.
    assert_eq!(
        interrupts.borrow().get_pending(),
        Some(expected_vector),
        "expected PCI INTx (GSI {gsi}) to deliver vector 0x{expected_vector:02x} after restore"
    );
}
