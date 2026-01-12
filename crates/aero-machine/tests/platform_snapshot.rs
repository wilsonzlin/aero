use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{InterruptController, InterruptInput, PlatformInterruptMode};
use pretty_assertions::assert_eq;

use aero_devices::acpi_pm::{
    DEFAULT_ACPI_ENABLE, DEFAULT_PM1A_EVT_BLK, DEFAULT_PM_TMR_BLK, DEFAULT_SMI_CMD_PORT,
};
use aero_devices::pci::{PciBarDefinition, PciBdf, PciConfigSpace, PciDevice, PciInterruptPin};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};

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

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_write(0xCFC + (offset & 3), size, value);
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_read(0xCFC + (offset & 3), size)
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
fn snapshot_restore_preserves_rtc_periodic_irq8_pending_vector() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // Put the PIC in a known state:
    // - master offset 0x20, slave offset 0x28
    // - unmask cascade + IRQ8
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(8, false);
    }

    // Enable RTC periodic interrupts (PIE=1) in Status Register B (0x0B).
    src.io_write(0x70, 1, 0x0B);
    src.io_write(0x71, 1, 0x42); // 24h + PIE

    assert_eq!(interrupts.borrow().get_pending(), None);

    // Default reg A rate is 1024Hz, so 1ms is enough to trigger a periodic interrupt.
    src.tick_platform(1_000_000);

    // IRQ8 on the slave PIC with offset 0x28 => vector 0x28.
    assert_eq!(interrupts.borrow().get_pending(), Some(0x28));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), Some(0x28));
}

#[test]
fn snapshot_restore_preserves_pit_phase_and_pulse_accounting() {
    let cfg = pc_machine_config();

    // Baseline: run without snapshot/restore.
    let mut baseline = Machine::new(cfg.clone()).unwrap();

    // Program PIT ch0: access lobyte/hibyte, mode 2 (rate generator), binary.
    baseline.io_write(PIT_CMD, 1, 0x34);
    baseline.io_write(PIT_CH0, 1, 100);
    baseline.io_write(PIT_CH0, 1, 0);

    let pit = baseline.pit().unwrap();
    pit.borrow_mut().advance_ticks(150);
    pit.borrow_mut().advance_ticks(50);
    assert_eq!(pit.borrow_mut().take_irq0_pulses(), 2);

    // Snapshot mid-period and continue after restore.
    let mut src = Machine::new(cfg.clone()).unwrap();
    src.io_write(PIT_CMD, 1, 0x34);
    src.io_write(PIT_CH0, 1, 100);
    src.io_write(PIT_CH0, 1, 0);
    let pit = src.pit().unwrap();
    pit.borrow_mut().advance_ticks(150);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
    let pit = restored.pit().unwrap();
    pit.borrow_mut().advance_ticks(50);

    // The restored PIT includes pulses from before the snapshot (1) and after (1).
    assert_eq!(pit.borrow_mut().take_irq0_pulses(), 2);
}

#[test]
fn snapshot_restore_preserves_pit_irq0_pending_pic_vector() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // Put the PIC in a known state and ensure IRQ0 is unmasked.
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(0, false);
    }

    // Program PIT ch0: access lobyte/hibyte, mode 2 (rate generator), binary. Divisor 100.
    src.io_write(PIT_CMD, 1, 0x34);
    src.io_write(PIT_CH0, 1, 100);
    src.io_write(PIT_CH0, 1, 0);

    assert_eq!(interrupts.borrow().get_pending(), None);

    // Advance exactly one period so we get a single edge-triggered IRQ0 pulse.
    let pit = src.pit().unwrap();
    pit.borrow_mut().advance_ticks(100);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x20));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), Some(0x20));
}

#[test]
fn snapshot_restore_preserves_acpi_sci_pending_irq9_vector() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // Put the PIC in a known state and ensure IRQ9 is unmasked.
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(9, false);
    }

    // Enable ACPI (sets SCI_EN).
    src.io_write(DEFAULT_SMI_CMD_PORT, 1, u32::from(DEFAULT_ACPI_ENABLE));
    // Enable the power button event bit in PM1_EN (bit 8).
    src.io_write(DEFAULT_PM1A_EVT_BLK + 2, 2, 1 << 8);

    // Trigger a power button event via the device model to set PM1_STS and assert SCI.
    src.acpi_pm().unwrap().borrow_mut().trigger_power_button();

    // IRQ9 on the slave PIC with offset 0x28 => vector 0x29.
    assert_eq!(interrupts.borrow().get_pending(), Some(0x29));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), Some(0x29));
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
fn snapshot_restore_preserves_pci_command_bits_and_pic_pending_interrupt() {
    let mut vm = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");

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

    // Install a simple endpoint at 00:01.0 with one MMIO BAR so we can validate both:
    // - guest-programmed BAR base
    // - PCI command register bits (IO/MEM/BME/INTX_DISABLE).
    let bdf = PciBdf::new(0, 1, 0);
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
        .add_device(bdf, Box::new(TestDev { cfg }));

    // Program BAR0 base and enable decode + bus mastering + INTx disable.
    cfg_write(&mut vm, bdf, 0x10, 4, 0x8000_0000);
    let command: u16 = 0x0007 | (1 << 10); // IO + MEM + BME + INTX_DISABLE
    cfg_write(&mut vm, bdf, 0x04, 2, u32::from(command));

    assert_eq!(cfg_read(&mut vm, bdf, 0x10, 4), 0x8000_0000);
    assert_eq!(cfg_read(&mut vm, bdf, 0x04, 2) as u16, command);

    // Raise a PIC interrupt (IRQ1 => vector 0x21 after setting offsets).
    let vector = 0x21u8;
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
        ints.raise_irq(InterruptInput::IsaIrq(1));
        assert_eq!(ints.get_pending(), Some(vector));
    }

    let snap = vm.take_snapshot_full().unwrap();

    // Mutate the PCI config and clear the interrupt so restore is an observable rewind.
    cfg_write(&mut vm, bdf, 0x10, 4, 0x9000_0000);
    cfg_write(&mut vm, bdf, 0x04, 2, 0);
    {
        let mut ints = interrupts.borrow_mut();
        ints.acknowledge(vector);
        ints.lower_irq(InterruptInput::IsaIrq(1));
        ints.eoi(vector);
        assert_eq!(ints.get_pending(), None);
    }
    assert_eq!(cfg_read(&mut vm, bdf, 0x10, 4), 0x9000_0000);
    assert_eq!(cfg_read(&mut vm, bdf, 0x04, 2), 0);

    vm.restore_snapshot_bytes(&snap).unwrap();

    // PCI config restored.
    assert_eq!(cfg_read(&mut vm, bdf, 0x10, 4), 0x8000_0000);
    assert_eq!(cfg_read(&mut vm, bdf, 0x04, 2) as u16, command);

    // Interrupt restored.
    assert_eq!(interrupts.borrow().get_pending(), Some(vector));
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
