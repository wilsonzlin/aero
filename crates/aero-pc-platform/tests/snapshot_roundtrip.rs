use aero_devices::acpi_pm::{PM1_STS_PWRBTN, DEFAULT_PM_TMR_BLK};
use aero_devices::pci::PciCoreSnapshot;
use aero_devices::pci::{PciBdf, PciInterruptPin};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD, PIT_HZ};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_pc_platform::PcPlatform;
use aero_platform::interrupts::{
    InterruptController, InterruptInput, PlatformInterruptMode, PlatformInterrupts,
};

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

fn ioapic_remote_irr_set(ints: &mut PlatformInterrupts, gsi: u32) -> bool {
    let redtbl_low = 0x10u32 + gsi * 2;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    let low = ints.ioapic_mmio_read(0x10);
    (low & (1 << 14)) != 0
}

fn deterministic_snapshot(dev: &impl IoSnapshot, name: &str) -> Vec<u8> {
    let a = dev.save_state();
    let b = dev.save_state();
    assert_eq!(a, b, "{name} snapshot bytes must be deterministic");
    a
}

#[test]
fn pc_platform_snapshot_roundtrip_preserves_acpi_sci_interrupt_and_platform_devices() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    const SCI_GSI: u32 = 9;
    const SCI_VECTOR: u8 = 0x60;
    const PIT_GSI: u32 = 2;
    const PIT_VECTOR: u8 = 0x40;
    const PIT_DIVISOR: u16 = 20;

    // --- Setup: build a PcPlatform and route SCI through the IOAPIC.
    let mut pc = PcPlatform::new(RAM_SIZE);
    {
        let mut ints = pc.interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // Program SCI (GSI9) to a known vector. ACPI tables specify active-low + level-triggered.
        let low = u32::from(SCI_VECTOR) | (1 << 13) | (1 << 15); // polarity_low + level-triggered
        program_ioapic_entry(&mut ints, SCI_GSI, low, 0);

        // Route PIT IRQ0 (GSI2 in our ACPI/IOAPIC setup) to a known vector.
        program_ioapic_entry(&mut ints, PIT_GSI, u32::from(PIT_VECTOR), 0);
    }

    // Put the i8042 controller into a non-default state (output buffer filled) so snapshot/restore
    // exercises the controller state machine.
    pc.io.write_u8(0x64, 0xAA); // i8042 self-test -> returns 0x55 in output buffer.

    // Put the PCI config ports into a non-default state (address latch set) so PCI core snapshot is
    // meaningfully exercised.
    pc.io.write(0xCF8, 4, 0x8000_0000 | (7 << 11) | 0x3C);
    let expected_pci_addr_latch = pc.io.read(0xCF8, 4);

    // Program PIT channel0 and advance time partway through the period (no IRQ yet).
    pc.io.write_u8(PIT_CMD, 0x34); // ch0, lobyte/hibyte, mode2, binary
    pc.io.write_u8(PIT_CH0, (PIT_DIVISOR & 0xFF) as u8);
    pc.io.write_u8(PIT_CH0, (PIT_DIVISOR >> 8) as u8);

    // Advance ~half a PIT period in a deterministic way.
    let pit_step_ticks: u64 = u64::from(PIT_DIVISOR / 2);
    let pit_step_ns: u64 =
        ((pit_step_ticks as u128) * 1_000_000_000u128).div_ceil(PIT_HZ as u128) as u64;
    pc.tick(pit_step_ns);

    // --- Trigger a level-triggered interrupt source (ACPI power button -> SCI).
    pc.io.write_u8(0xB2, 0xA0); // ACPI enable handshake.
    pc.io.write(0x0402, 2, u32::from(PM1_STS_PWRBTN)); // PM1_EN.PWRBTN_EN
    pc.acpi_pm.borrow_mut().trigger_power_button();

    assert_eq!(pc.interrupts.borrow().get_pending(), Some(SCI_VECTOR));
    {
        let mut ints = pc.interrupts.borrow_mut();
        assert!(
            ioapic_remote_irr_set(&mut ints, SCI_GSI),
            "level-triggered SCI must set IOAPIC Remote-IRR once delivered"
        );
    }

    // --- Snapshot key platform devices (each must be deterministic).
    let interrupts_state = {
        let ints = pc.interrupts.borrow();
        deterministic_snapshot(&*ints, "PlatformInterrupts")
    };
    let pit_state = {
        let pit = pc.pit();
        let pit = pit.borrow();
        deterministic_snapshot(&*pit, "PIT")
    };
    let rtc_state = {
        let rtc = pc.rtc();
        let rtc = rtc.borrow();
        deterministic_snapshot(&*rtc, "RTC")
    };
    let hpet_state = {
        let hpet = pc.hpet();
        let hpet = hpet.borrow();
        deterministic_snapshot(&*hpet, "HPET")
    };
    let acpi_pm_state = {
        let pm = pc.acpi_pm.borrow();
        deterministic_snapshot(&*pm, "ACPI PM")
    };
    let i8042_state = {
        let ctrl = pc.i8042_controller();
        let ctrl = ctrl.borrow();
        deterministic_snapshot(&*ctrl, "i8042")
    };
    let pci_core_state = {
        let mut cfg_ports = pc.pci_cfg.borrow_mut();
        let core = PciCoreSnapshot::new(&mut cfg_ports, &mut pc.pci_intx);
        deterministic_snapshot(&core, "PCI core")
    };

    // --- Restore into a fresh PcPlatform (interrupts first, then devices that may re-drive lines).
    let mut pc2 = PcPlatform::new(RAM_SIZE);

    pc2.interrupts
        .borrow_mut()
        .load_state(&interrupts_state)
        .unwrap();
    pc2.pit().borrow_mut().load_state(&pit_state).unwrap();
    pc2.rtc().borrow_mut().load_state(&rtc_state).unwrap();
    pc2.hpet().borrow_mut().load_state(&hpet_state).unwrap();
    pc2.acpi_pm.borrow_mut().load_state(&acpi_pm_state).unwrap();
    pc2.i8042_controller()
        .borrow_mut()
        .load_state(&i8042_state)
        .unwrap();
    {
        let mut cfg_ports = pc2.pci_cfg.borrow_mut();
        let mut core = PciCoreSnapshot::new(&mut cfg_ports, &mut pc2.pci_intx);
        core.load_state(&pci_core_state).unwrap();
        core.sync_intx_levels_to_sink(&mut *pc2.interrupts.borrow_mut());
    }

    // HPET snapshot does not serialize active IRQ assertions; re-drive after restore.
    {
        let hpet = pc2.hpet();
        let mut hpet = hpet.borrow_mut();
        let mut ints = pc2.interrupts.borrow_mut();
        hpet.poll(&mut *ints);
    }

    // --- Assertions after restore.
    assert_eq!(
        pc2.io.read(0xCF8, 4),
        expected_pci_addr_latch,
        "PCI config address latch should survive snapshot/restore"
    );

    assert_eq!(
        pc2.interrupts.borrow().get_pending(),
        Some(SCI_VECTOR),
        "pending SCI vector should survive snapshot/restore"
    );
    assert!(
        pc2.acpi_pm.borrow().sci_level(),
        "SCI level should remain asserted after restore"
    );

    // Remote-IRR should remain set until EOI, even after acknowledging in the LAPIC.
    {
        let mut ints = pc2.interrupts.borrow_mut();
        assert!(ioapic_remote_irr_set(&mut ints, SCI_GSI));
        ints.acknowledge(SCI_VECTOR);
        assert_eq!(ints.get_pending(), None);
        assert!(
            ioapic_remote_irr_set(&mut ints, SCI_GSI),
            "Remote-IRR should remain set until EOI"
        );
    }

    // Clearing the PM1 status bit should deassert SCI.
    pc2.io.write(0x0400, 2, u32::from(PM1_STS_PWRBTN)); // PM1_STS write-1-to-clear
    assert!(
        !pc2.acpi_pm.borrow().sci_level(),
        "clearing PM1_STS should deassert SCI"
    );

    // EOI after SCI is deasserted should clear Remote-IRR without causing re-delivery.
    {
        let mut ints = pc2.interrupts.borrow_mut();
        assert!(ioapic_remote_irr_set(&mut ints, SCI_GSI));
        ints.eoi(SCI_VECTOR);
        assert_eq!(ints.get_pending(), None);
        assert!(
            !ioapic_remote_irr_set(&mut ints, SCI_GSI),
            "EOI should clear Remote-IRR once SCI is deasserted"
        );
    }

    // PIT should continue counting from its restored phase and eventually deliver an IRQ0 pulse.
    pc2.tick(pit_step_ns);
    assert_eq!(
        pc2.interrupts.borrow().get_pending(),
        Some(PIT_VECTOR),
        "PIT interrupt should be delivered after restore"
    );
    pc2.interrupts.borrow_mut().acknowledge(PIT_VECTOR);
    pc2.interrupts.borrow_mut().eoi(PIT_VECTOR);

    // i8042 output buffer should still contain the self-test response.
    assert_eq!(
        pc2.io.read_u8(0x60),
        0x55,
        "i8042 output buffer should survive snapshot/restore"
    );
}

#[test]
fn pc_platform_snapshot_roundtrip_redrives_hpet_and_pci_intx_levels_after_restore() {
    // This test focuses on the post-restore "re-drive" steps:
    // - HPET: `irq_asserted` is intentionally not serialized, so the first `poll()` after restore
    //   must reassert any level-triggered interrupts whose status bits are pending.
    // - PCI INTx: the router snapshot can't touch the platform sink; `sync_levels_to_sink()` must
    //   re-drive asserted GSIs.

    const RAM_SIZE: usize = 2 * 1024 * 1024;

    // Use vectors in different priority classes so LAPIC priority masking doesn't interfere.
    const HPET_GSI: u32 = 17;
    const HPET_VECTOR: u8 = 0x61; // priority class 0x60

    const PCI_GSI: u32 = 10;
    const PCI_VECTOR: u8 = 0x58; // priority class 0x50

    // HPET MMIO register offsets (see `aero_devices::hpet`).
    const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
    const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;
    const HPET_REG_MAIN_COUNTER: u64 = 0x0F0;
    const HPET_REG_TIMER0_BASE: u64 = 0x100;
    const HPET_TIMER_STRIDE: u64 = 0x20;
    const HPET_REG_TIMER_CONFIG: u64 = 0x00;
    const HPET_REG_TIMER_COMPARATOR: u64 = 0x08;

    const HPET_GEN_CONF_ENABLE: u64 = 1 << 0;
    const HPET_TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
    const HPET_TIMER_CFG_INT_ENABLE: u64 = 1 << 2;
    const HPET_TIMER_CFG_INT_ROUTE_SHIFT: u64 = 9;
    const HPET_TIMER_CFG_INT_ROUTE_MASK: u64 = 0x1F << HPET_TIMER_CFG_INT_ROUTE_SHIFT;

    let mut pc = PcPlatform::new(RAM_SIZE);
    {
        let mut ints = pc.interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // HPET + PCI INTx are typically active-low + level-triggered in PC ACPI setups.
        let hpet_low = u32::from(HPET_VECTOR) | (1 << 13) | (1 << 15);
        program_ioapic_entry(&mut ints, HPET_GSI, hpet_low, 0);

        let pci_low = u32::from(PCI_VECTOR) | (1 << 13) | (1 << 15);
        program_ioapic_entry(&mut ints, PCI_GSI, pci_low, 0);
    }

    // --- HPET: configure timer2 to fire once and latch a pending level interrupt.
    let timer_idx: u64 = 2;
    let timer_cfg_off = HPET_REG_TIMER0_BASE + timer_idx * HPET_TIMER_STRIDE + HPET_REG_TIMER_CONFIG;
    let timer_cmp_off =
        HPET_REG_TIMER0_BASE + timer_idx * HPET_TIMER_STRIDE + HPET_REG_TIMER_COMPARATOR;

    {
        let hpet = pc.hpet();
        let mut hpet = hpet.borrow_mut();
        let mut ints = pc.interrupts.borrow_mut();

        hpet.mmio_write(HPET_REG_GENERAL_CONFIG, 8, HPET_GEN_CONF_ENABLE, &mut *ints);

        let mut cfg = hpet.mmio_read(timer_cfg_off, 8, &mut *ints);
        cfg |= HPET_TIMER_CFG_INT_ENABLE | HPET_TIMER_CFG_INT_LEVEL;
        cfg = (cfg & !HPET_TIMER_CFG_INT_ROUTE_MASK)
            | (u64::from(HPET_GSI) << HPET_TIMER_CFG_INT_ROUTE_SHIFT);
        hpet.mmio_write(timer_cfg_off, 8, cfg, &mut *ints);

        let counter = hpet.mmio_read(HPET_REG_MAIN_COUNTER, 8, &mut *ints);
        hpet.mmio_write(timer_cmp_off, 8, counter.wrapping_add(1), &mut *ints);
    }

    // Advance deterministic time enough for the comparator to fire.
    pc.tick(1_000);
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        Some(HPET_VECTOR),
        "HPET timer interrupt should be delivered before snapshot"
    );

    // Create a snapshot scenario where the HPET interrupt status bit remains set but the GSI line
    // is deasserted in the interrupt controller snapshot (to ensure `poll()` is required after
    // restore to re-drive the level).
    {
        let mut ints = pc.interrupts.borrow_mut();
        ints.acknowledge(HPET_VECTOR);
        ints.lower_irq(InterruptInput::Gsi(HPET_GSI));
        ints.eoi(HPET_VECTOR);
        assert_eq!(ints.get_pending(), None);
    }

    // --- PCI INTx: assert an INTx source, then manually deassert the platform GSI while keeping
    // the router's internal level asserted so `sync_levels_to_sink()` is required on restore.
    let intx_bdf = PciBdf::new(0, 0, 0);
    let intx_pin = PciInterruptPin::IntA;
    pc.pci_intx
        .assert_intx(intx_bdf, intx_pin, &mut *pc.interrupts.borrow_mut());

    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        Some(PCI_VECTOR),
        "PCI INTx vector should be delivered before snapshot"
    );

    {
        let mut ints = pc.interrupts.borrow_mut();
        ints.acknowledge(PCI_VECTOR);
        ints.lower_irq(InterruptInput::Gsi(PCI_GSI));
        ints.eoi(PCI_VECTOR);
        assert_eq!(ints.get_pending(), None);
    }

    // --- Snapshot deterministically.
    let interrupts_state = {
        let ints = pc.interrupts.borrow();
        deterministic_snapshot(&*ints, "PlatformInterrupts")
    };
    let hpet_state = {
        let hpet = pc.hpet();
        let hpet = hpet.borrow();
        deterministic_snapshot(&*hpet, "HPET")
    };
    let pci_core_state = {
        let mut cfg_ports = pc.pci_cfg.borrow_mut();
        let core = PciCoreSnapshot::new(&mut *cfg_ports, &mut pc.pci_intx);
        deterministic_snapshot(&core, "PCI core")
    };

    // --- Restore into a fresh PcPlatform (interrupts first, then other devices).
    let mut pc2 = PcPlatform::new(RAM_SIZE);
    pc2.interrupts
        .borrow_mut()
        .load_state(&interrupts_state)
        .unwrap();

    pc2.hpet().borrow_mut().load_state(&hpet_state).unwrap();

    {
        let mut cfg_ports = pc2.pci_cfg.borrow_mut();
        let mut core = PciCoreSnapshot::new(&mut *cfg_ports, &mut pc2.pci_intx);
        core.load_state(&pci_core_state).unwrap();
    }

    // Re-drive PCI first (via router), then HPET (via `poll()`).
    pc2.sync_pci_intx_levels_to_interrupts();
    {
        let hpet = pc2.hpet();
        let mut hpet = hpet.borrow_mut();
        let mut ints = pc2.interrupts.borrow_mut();
        hpet.poll(&mut *ints);
    }

    // Both vectors should now be pending; HPET has the higher priority class.
    assert_eq!(
        pc2.interrupts.borrow().get_pending(),
        Some(HPET_VECTOR),
        "HPET poll should re-drive pending level interrupt after restore"
    );

    // Service HPET: clear status bit (deassert line) then EOI.
    {
        let mut ints = pc2.interrupts.borrow_mut();
        ints.acknowledge(HPET_VECTOR);
    }
    {
        let hpet = pc2.hpet();
        let mut hpet = hpet.borrow_mut();
        let mut ints = pc2.interrupts.borrow_mut();
        // Timer2 interrupt status is bit2.
        hpet.mmio_write(HPET_REG_GENERAL_INT_STATUS, 8, 1u64 << timer_idx, &mut *ints);
    }
    pc2.interrupts.borrow_mut().eoi(HPET_VECTOR);

    // PCI INTx should now be deliverable.
    assert_eq!(pc2.interrupts.borrow().get_pending(), Some(PCI_VECTOR));
    {
        let mut ints = pc2.interrupts.borrow_mut();
        ints.acknowledge(PCI_VECTOR);
    }

    // Deassert the router source and then EOI.
    pc2.pci_intx
        .deassert_intx(intx_bdf, intx_pin, &mut *pc2.interrupts.borrow_mut());
    pc2.interrupts.borrow_mut().eoi(PCI_VECTOR);

    assert_eq!(pc2.interrupts.borrow().get_pending(), None);
}

#[test]
fn pc_platform_snapshot_roundtrip_preserves_rtc_irq8_and_requires_status_c_clear() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    const RTC_GSI: u32 = 8;
    const RTC_VECTOR: u8 = 0x48;

    const RTC_PORT_INDEX: u16 = 0x70;
    const RTC_PORT_DATA: u16 = 0x71;
    const RTC_REG_STATUS_B: u8 = 0x0B;
    const RTC_REG_STATUS_C: u8 = 0x0C;
    const RTC_REG_B_24H: u8 = 1 << 1;
    const RTC_REG_B_UIE: u8 = 1 << 4;

    let mut pc = PcPlatform::new(RAM_SIZE);
    {
        let mut ints = pc.interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_entry(&mut ints, RTC_GSI, u32::from(RTC_VECTOR), 0);
    }

    // Enable RTC update-ended interrupts (UIE).
    pc.io.write_u8(RTC_PORT_INDEX, RTC_REG_STATUS_B);
    pc.io.write_u8(RTC_PORT_DATA, RTC_REG_B_24H | RTC_REG_B_UIE);

    // Advance one second to trigger UF/IRQ8.
    pc.tick(1_000_000_000);
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        Some(RTC_VECTOR),
        "RTC IRQ8 should deliver after a one-second tick once UIE is enabled"
    );

    // Snapshot deterministically.
    let interrupts_state = {
        let ints = pc.interrupts.borrow();
        deterministic_snapshot(&*ints, "PlatformInterrupts")
    };
    let rtc_state = {
        let rtc = pc.rtc();
        let rtc = rtc.borrow();
        deterministic_snapshot(&*rtc, "RTC")
    };

    // Restore into a fresh platform.
    let mut pc2 = PcPlatform::new(RAM_SIZE);
    pc2.interrupts
        .borrow_mut()
        .load_state(&interrupts_state)
        .unwrap();
    pc2.rtc().borrow_mut().load_state(&rtc_state).unwrap();

    // Pending vector should survive restore.
    assert_eq!(pc2.interrupts.borrow().get_pending(), Some(RTC_VECTOR));

    // Ack + EOI without reading Status C: line stays asserted, so edge-triggered IOAPIC should not
    // re-fire on subsequent ticks.
    pc2.interrupts.borrow_mut().acknowledge(RTC_VECTOR);
    pc2.interrupts.borrow_mut().eoi(RTC_VECTOR);
    assert_eq!(pc2.interrupts.borrow().get_pending(), None);

    pc2.tick(1_000_000_000);
    assert_eq!(
        pc2.interrupts.borrow().get_pending(),
        None,
        "RTC line stays asserted until Status C is read; no new edge should be observed"
    );

    // Reading Status C clears the latch and deasserts IRQ8.
    pc2.io.write_u8(RTC_PORT_INDEX, RTC_REG_STATUS_C);
    let status_c = pc2.io.read_u8(RTC_PORT_DATA);
    assert_ne!(status_c & 0x10, 0, "UF should be set in Status C");

    // Now another second edge should deliver again.
    pc2.tick(1_000_000_000);
    assert_eq!(pc2.interrupts.borrow().get_pending(), Some(RTC_VECTOR));
}

#[test]
fn pc_platform_snapshot_roundtrip_preserves_acpi_pm_timer_progression() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    // The ACPI PM timer is a 24-bit free-running counter at 3.579545MHz.
    const PM_TIMER_FREQUENCY_HZ: u128 = 3_579_545;
    const NS_PER_SEC: u128 = 1_000_000_000;
    const PM_TIMER_MASK_24BIT: u32 = 0x00FF_FFFF;

    let mut pc = PcPlatform::new(RAM_SIZE);

    // At time 0, the timer should read as 0 and be stable without ticking.
    let t0 = (pc.io.read(DEFAULT_PM_TMR_BLK, 4) as u32) & PM_TIMER_MASK_24BIT;
    let t0b = (pc.io.read(DEFAULT_PM_TMR_BLK, 4) as u32) & PM_TIMER_MASK_24BIT;
    assert_eq!(t0, t0b);

    // Advance deterministic time and ensure the timer increments as expected.
    let delta1_ns: u64 = 1_000_000; // 1ms
    pc.tick(delta1_ns);
    let t1 = (pc.io.read(DEFAULT_PM_TMR_BLK, 4) as u32) & PM_TIMER_MASK_24BIT;
    let expected_t1 = (((delta1_ns as u128) * PM_TIMER_FREQUENCY_HZ) / NS_PER_SEC) as u32;
    assert_eq!(t1, expected_t1 & PM_TIMER_MASK_24BIT);

    // Snapshot deterministically and restore into a fresh platform.
    let acpi_pm_state = {
        let pm = pc.acpi_pm.borrow();
        deterministic_snapshot(&*pm, "ACPI PM")
    };

    let mut pc2 = PcPlatform::new(RAM_SIZE);
    pc2.acpi_pm.borrow_mut().load_state(&acpi_pm_state).unwrap();

    // The timer value at the snapshot moment should survive restore even though the new clock
    // starts at 0; the device models this by restoring the timer base offset.
    let t1_restore = (pc2.io.read(DEFAULT_PM_TMR_BLK, 4) as u32) & PM_TIMER_MASK_24BIT;
    assert_eq!(t1_restore, t1);

    // Further ticking should advance from the restored point. Use absolute expected values to
    // avoid off-by-one issues due to integer division at fractional tick boundaries.
    let delta2_ns: u64 = 500_000; // 0.5ms
    pc2.tick(delta2_ns);
    let t2 = (pc2.io.read(DEFAULT_PM_TMR_BLK, 4) as u32) & PM_TIMER_MASK_24BIT;
    let total_ns = (delta1_ns as u128) + (delta2_ns as u128);
    let expected_t2 = ((total_ns * PM_TIMER_FREQUENCY_HZ) / NS_PER_SEC) as u32;
    assert_eq!(t2, expected_t2 & PM_TIMER_MASK_24BIT);
}
