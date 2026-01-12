use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{InterruptController, InterruptInput, PlatformInterruptMode};
use pretty_assertions::assert_eq;

use aero_devices::acpi_pm::DEFAULT_PM_TMR_BLK;
use aero_devices::pci::{PciBarDefinition, PciBdf, PciConfigSpace, PciDevice, PciInterruptPin};

fn pc_machine_config() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        // Keep the machine minimal for deterministic platform snapshot tests.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    low: u32,
    high: u32,
) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn snapshot_restore_preserves_acpi_pm_timer_and_it_advances_with_manual_clock() {
    let mut src = Machine::new(pc_machine_config()).unwrap();

    // PM_TMR should start at 0 at power-on with the shared ManualClock at 0.
    assert_eq!(src.io_read(DEFAULT_PM_TMR_BLK, 4), 0);

    // Advance 1s: PM timer frequency is 3.579545 MHz.
    src.tick_platform(1_000_000_000);
    assert_eq!(src.io_read(DEFAULT_PM_TMR_BLK, 4), 3_579_545);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Restoring rewinds time, so the timer should pick up exactly where it was snapshotted.
    assert_eq!(restored.io_read(DEFAULT_PM_TMR_BLK, 4), 3_579_545);

    // Advancing another second should deterministically add another 3_579_545 ticks.
    restored.tick_platform(1_000_000_000);
    assert_eq!(restored.io_read(DEFAULT_PM_TMR_BLK, 4), 7_159_090);
}

#[test]
fn snapshot_restore_pci_config_bar_programming_survives() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = src.pci_config_ports().expect("pc platform enabled");

    struct TestDev {
        cfg: PciConfigSpace,
    }

    impl PciDevice for TestDev {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(PciBdf::new(0, 1, 0), Box::new(TestDev { cfg }));

    // Program BAR0 via the standard PCI config mechanism #1 ports.
    let bar0_addr = 0x8000_0000u32 | (1u32 << 11) | 0x10;
    src.io_write(0xCF8, 4, bar0_addr);
    src.io_write(0xCFC, 4, 0x8000_0000);

    src.io_write(0xCF8, 4, bar0_addr);
    assert_eq!(src.io_read(0xCFC, 4), 0x8000_0000);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = restored.pci_config_ports().unwrap();

    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(PciBdf::new(0, 1, 0), Box::new(TestDev { cfg }));

    restored.restore_snapshot_bytes(&snap).unwrap();

    restored.io_write(0xCF8, 4, bar0_addr);
    assert_eq!(restored.io_read(0xCFC, 4), 0x8000_0000);
}

#[test]
fn snapshot_restore_syncs_pci_intx_levels_into_interrupt_controller() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();
    let pci_intx = src.pci_intx_router().unwrap();

    // Route IOAPIC GSI10 -> vector 0x51, active-low, level-triggered, masked.
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = 0x51u32 | (1 << 13) | (1 << 15) | (1 << 16);
        program_ioapic_entry(&mut *ints, 10, low, 0);
    }

    // Assert a PCI INTx line that routes to GSI10 (device 0, INTA#).
    {
        let mut ints = interrupts.borrow_mut();
        pci_intx
            .borrow_mut()
            .assert_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntA, &mut *ints);
    }

    // Corrupt the sink state to simulate a snapshot taken at an inconsistent point: the router
    // thinks the line is asserted, but the platform interrupt controller has it deasserted.
    interrupts.borrow_mut().lower_irq(InterruptInput::Gsi(10));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Unmask the IOAPIC entry. If Machine restore called `PciIntxRouter::sync_levels_to_sink`,
    // the asserted GSI level is re-driven and unmasking delivers immediately.
    {
        let mut ints = interrupts.borrow_mut();
        let low = 0x51u32 | (1 << 13) | (1 << 15);
        program_ioapic_entry(&mut *ints, 10, low, 0);
    }

    assert_eq!(interrupts.borrow().get_pending(), Some(0x51));
}

#[test]
fn snapshot_restore_preserves_lapic_timer_state() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // APIC mode required for LAPIC delivery.
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Program LAPIC timer (one-shot, vector 0x40, initial count 10).
    interrupts
        .borrow()
        .lapic_mmio_write(0x3E0, &0xBu32.to_le_bytes()); // Divide config
    interrupts
        .borrow()
        .lapic_mmio_write(0x320, &0x40u32.to_le_bytes()); // LVT Timer
    interrupts
        .borrow()
        .lapic_mmio_write(0x380, &10u32.to_le_bytes()); // Initial count

    interrupts.borrow().tick(9);
    assert_eq!(interrupts.borrow().get_pending(), None);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    interrupts.borrow().tick(1);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x40));
}

#[test]
fn snapshot_restore_polls_hpet_once_to_reassert_level_lines() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();
    let hpet = src.hpet().unwrap();

    // Route GSI2 -> vector 0x61, level-triggered, masked. (GSI2 is active-high in our board wiring.)
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = 0x61u32 | (1 << 15) | (1 << 16);
        program_ioapic_entry(&mut *ints, 2, low, 0);
    }

    // Configure HPET timer0 for a level-triggered interrupt and arm it such that it becomes
    // pending immediately once HPET is enabled.
    {
        let mut ints = interrupts.borrow_mut();
        let mut hpet = hpet.borrow_mut();

        // Timer0 config: route=2, level-triggered, interrupt enabled.
        let timer0_cfg = (2u64 << 9) | (1 << 1) | (1 << 2);
        hpet.mmio_write(0x100, 8, timer0_cfg, &mut *ints);

        // Arm comparator at 0 so it's immediately pending once enabled.
        hpet.mmio_write(0x108, 8, 0, &mut *ints);

        // Enable HPET.
        hpet.mmio_write(0x010, 8, 1, &mut *ints);

        let status = hpet.mmio_read(0x020, 8, &mut *ints);
        assert_ne!(status & 1, 0, "timer0 interrupt status must be set");
    }

    // Corrupt the sink state: HPET has an interrupt pending (general_int_status bit set), but the
    // platform interrupt controller line is deasserted. HPET `irq_asserted` is not snapshotted, so
    // restore must poll once to re-drive the level line.
    interrupts.borrow_mut().lower_irq(InterruptInput::Gsi(2));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Unmask the IOAPIC entry. If Machine restore called `Hpet::poll()` after state restore, the
    // asserted level is re-driven and unmasking delivers immediately.
    {
        let mut ints = interrupts.borrow_mut();
        let low = 0x61u32 | (1 << 15); // unmasked
        program_ioapic_entry(&mut *ints, 2, low, 0);
    }

    assert_eq!(interrupts.borrow().get_pending(), Some(0x61));
}
