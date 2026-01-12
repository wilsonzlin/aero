use aero_devices::acpi_pm::PM1_STS_PWRBTN;
use aero_devices::pci::PciCoreSnapshot;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_pc_platform::PcPlatform;
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};

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

    // --- Setup: build a PcPlatform and route SCI through the IOAPIC.
    let mut pc = PcPlatform::new(RAM_SIZE);
    {
        let mut ints = pc.interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // Program SCI (GSI9) to a known vector. ACPI tables specify active-low + level-triggered.
        let low = u32::from(SCI_VECTOR) | (1 << 13) | (1 << 15); // polarity_low + level-triggered
        program_ioapic_entry(&mut ints, SCI_GSI, low, 0);
    }

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
        let core = PciCoreSnapshot::new(&mut *cfg_ports, &mut pc.pci_intx);
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
        let mut core = PciCoreSnapshot::new(&mut *cfg_ports, &mut pc2.pci_intx);
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
}
