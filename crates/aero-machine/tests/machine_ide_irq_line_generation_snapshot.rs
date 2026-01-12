#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile::IDE_PIIX3;
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices_storage::pci_ide::PRIMARY_PORTS;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, InterruptInput, PlatformInterruptMode,
};
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
use pretty_assertions::assert_eq;

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | ((u32::from(bdf.bus)) << 16)
        | ((u32::from(bdf.device & 0x1F)) << 11)
        | ((u32::from(bdf.function & 0x07)) << 8)
        | (u32::from(offset) & 0xFC)
}

fn write_cfg_u16(m: &mut Machine, bdf: PciBdf, offset: u8, value: u16) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    vector: u8,
) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, u32::from(vector));
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, 0);
}

#[test]
fn snapshot_restore_redrives_asserted_ide_irq14_even_if_platform_line_state_was_desynced() {
    const RAM_SIZE: u64 = 2 * 1024 * 1024;
    const IDE_GSI: u32 = 14;
    const VECTOR: u8 = 0x60;

    let cfg = MachineConfig {
        ram_size_bytes: RAM_SIZE,
        enable_pc_platform: true,
        enable_ide: true,
        // Keep this test focused on interrupt line bookkeeping across snapshot/restore.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Attach a disk so the primary channel responds to IDENTIFY.
    let capacity = 8 * SECTOR_SIZE as u64;
    let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    src.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Switch to APIC mode, but do not unmask/program the IOAPIC until after we have forced the
    // interrupt line low. This ensures no LAPIC IRR state is created before snapshot.
    {
        let interrupts = src.platform_interrupts().unwrap();
        interrupts
            .borrow_mut()
            .set_mode(PlatformInterruptMode::Apic);
    }

    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut src, bdf, 0x04, 0x0001); // PCI COMMAND.IO

    // Trigger a primary-channel IRQ14 by issuing IDENTIFY DEVICE.
    src.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);
    src.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);
    let word0 = src.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
    assert_eq!(word0, 0x0040);

    // Drive the IDE IRQ14 line into the platform interrupt controller, then intentionally
    // desynchronize the sink by forcing GSI14 low without updating the cached `PlatformIrqLine`
    // state. This simulates the state immediately after restore, where device models must re-drive
    // their line levels into a freshly restored interrupt sink.
    src.poll_pci_intx_lines();
    let interrupts = src.platform_interrupts().unwrap();
    assert!(
        interrupts.borrow().gsi_level(IDE_GSI),
        "sanity: IDE IRQ14 should assert GSI14 before desync"
    );
    interrupts
        .borrow_mut()
        .lower_irq(InterruptInput::IsaIrq(14));
    assert!(
        !interrupts.borrow().gsi_level(IDE_GSI),
        "sanity: test should desync sink state before snapshot"
    );

    // Now that the line is electrically low, program the IOAPIC entry for GSI14 -> VECTOR. Since
    // this is edge-triggered and the pin is low, unmasking must not deliver an interrupt yet.
    {
        let mut ints = interrupts.borrow_mut();
        program_ioapic_entry(&mut ints, IDE_GSI, VECTOR);
    }
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None,
        "sanity: IOAPIC unmask should not deliver without an edge"
    );

    let snap = src.take_snapshot_full().unwrap();

    // Restore into a new machine, but first force the cached IDE irq line state to be high so this
    // test exercises the irq_line_generation invalidation logic during restore.
    let mut restored = Machine::new(cfg.clone()).unwrap();
    {
        let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        restored
            .attach_ide_primary_master_disk(Box::new(disk))
            .unwrap();
        write_cfg_u16(&mut restored, bdf, 0x04, 0x0001);
        restored.io_write(PRIMARY_PORTS.cmd_base + 6, 1, 0xA0);
        restored.io_write(PRIMARY_PORTS.cmd_base + 7, 1, 0xEC);
        let word0 = restored.io_read(PRIMARY_PORTS.cmd_base, 2) as u16;
        assert_eq!(word0, 0x0040);
        restored.poll_pci_intx_lines();
    }

    restored.restore_snapshot_bytes(&snap).unwrap();

    let restored_interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(
        restored_interrupts.borrow().mode(),
        PlatformInterruptMode::Apic,
        "interrupt mode should survive snapshot restore"
    );
    assert!(
        restored_interrupts.borrow().gsi_level(IDE_GSI),
        "restored machine should re-drive the asserted IDE IRQ14 line into GSI14"
    );
    assert_eq!(
        PlatformInterruptController::get_pending(&*restored_interrupts.borrow()),
        Some(VECTOR),
        "re-driving IRQ14 after restore should create an IOAPIC edge and a pending LAPIC vector"
    );
}
