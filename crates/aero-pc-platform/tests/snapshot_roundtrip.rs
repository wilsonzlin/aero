use aero_devices::acpi_pm::{
    DEFAULT_ACPI_ENABLE, DEFAULT_PM1A_EVT_BLK, DEFAULT_PM_TMR_BLK, DEFAULT_SMI_CMD_PORT,
    PM1_STS_PWRBTN,
};
use aero_devices::i8042::{I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_devices::pci::{profile, PciBdf, PciCoreSnapshot, PciInterruptPin, PCI_CFG_ADDR_PORT};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD, PIT_HZ};
use aero_devices_storage::ata::{AtaDrive, ATA_CMD_READ_DMA_EXT, ATA_CMD_WRITE_DMA_EXT};
use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
use aero_devices_storage::pci_ide::{PRIMARY_PORTS, SECONDARY_PORTS};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_pc_platform::PcPlatform;
use aero_platform::interrupts::{
    InterruptController, InterruptInput, PlatformInterruptMode, PlatformInterrupts, IMCR_DATA_PORT,
    IMCR_INDEX, IMCR_SELECT_PORT,
};
use aero_storage::{DiskError, Result as DiskResult, VirtualDisk, SECTOR_SIZE};
use memory::MemoryBus as _;
use std::io;
use std::sync::{Arc, Mutex};

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
    // Switch the platform into APIC mode via IMCR (0x22/0x23) to match how guests enable the
    // IOAPIC/LAPIC interrupt path.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    {
        let mut ints = pc.interrupts.borrow_mut();

        // Program SCI (GSI9) to a known vector. ACPI tables specify active-low + level-triggered.
        let low = u32::from(SCI_VECTOR) | (1 << 13) | (1 << 15); // polarity_low + level-triggered
        program_ioapic_entry(&mut ints, SCI_GSI, low, 0);

        // Route PIT IRQ0 (GSI2 in our ACPI/IOAPIC setup) to a known vector.
        program_ioapic_entry(&mut ints, PIT_GSI, u32::from(PIT_VECTOR), 0);
    }

    // Put the i8042 controller into a non-default state (output buffer filled) so snapshot/restore
    // exercises the controller state machine.
    pc.io
        .write_u8(I8042_STATUS_PORT, 0xAA); // i8042 self-test -> returns 0x55 in output buffer.

    // Put the PCI config ports into a non-default state (address latch set) so PCI core snapshot is
    // meaningfully exercised.
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        0x8000_0000 | (7 << 11) | 0x3C,
    );
    let expected_pci_addr_latch = pc.io.read(PCI_CFG_ADDR_PORT, 4);

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
    pc.io
        .write_u8(DEFAULT_SMI_CMD_PORT, DEFAULT_ACPI_ENABLE); // ACPI enable handshake.
    pc.io.write(
        DEFAULT_PM1A_EVT_BLK + 2,
        2,
        u32::from(PM1_STS_PWRBTN),
    ); // PM1_EN.PWRBTN_EN
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
        let mut ints = pc2.interrupts.borrow_mut();
        let ints: &mut PlatformInterrupts = &mut ints;
        core.sync_intx_levels_to_sink(ints);
    }

    // HPET snapshot does not serialize active IRQ assertions; re-drive after restore.
    {
        let hpet = pc2.hpet();
        let mut hpet = hpet.borrow_mut();
        let mut ints = pc2.interrupts.borrow_mut();
        hpet.sync_levels_to_sink(&mut *ints);
    }

    // --- Assertions after restore.
    assert_eq!(
        pc2.io.read(PCI_CFG_ADDR_PORT, 4),
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
    pc2.io
        .write(DEFAULT_PM1A_EVT_BLK, 2, u32::from(PM1_STS_PWRBTN)); // PM1_STS write-1-to-clear
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
        pc2.io.read_u8(I8042_DATA_PORT),
        0x55,
        "i8042 output buffer should survive snapshot/restore"
    );
}

#[test]
fn pc_platform_snapshot_roundtrip_redrives_hpet_and_pci_intx_levels_after_restore() {
    // This test focuses on the post-restore "re-drive" steps:
    // - HPET: `irq_asserted` is intentionally not serialized, so restore must explicitly re-drive
    //   any pending level-triggered interrupts into the interrupt sink.
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
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    {
        let mut ints = pc.interrupts.borrow_mut();

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
    // is deasserted in the interrupt controller snapshot (to ensure explicit re-drive is required
    // after restore).
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
        let core = PciCoreSnapshot::new(&mut cfg_ports, &mut pc.pci_intx);
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
        let mut core = PciCoreSnapshot::new(&mut cfg_ports, &mut pc2.pci_intx);
        core.load_state(&pci_core_state).unwrap();
    }

    // Re-drive PCI first (via router), then HPET (via `sync_levels_to_sink()`).
    pc2.sync_pci_intx_levels_to_interrupts();
    {
        let hpet = pc2.hpet();
        let mut hpet = hpet.borrow_mut();
        let mut ints = pc2.interrupts.borrow_mut();
        hpet.sync_levels_to_sink(&mut *ints);
    }

    // Both vectors should now be pending; HPET has the higher priority class.
    assert_eq!(
        pc2.interrupts.borrow().get_pending(),
        Some(HPET_VECTOR),
        "HPET IRQ sync should re-drive pending level interrupt after restore"
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
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    {
        let mut ints = pc.interrupts.borrow_mut();
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
    let t0 = pc.io.read(DEFAULT_PM_TMR_BLK, 4) & PM_TIMER_MASK_24BIT;
    let t0b = pc.io.read(DEFAULT_PM_TMR_BLK, 4) & PM_TIMER_MASK_24BIT;
    assert_eq!(t0, t0b);

    // Advance deterministic time and ensure the timer increments as expected.
    let delta1_ns: u64 = 1_000_000; // 1ms
    pc.tick(delta1_ns);
    let t1 = pc.io.read(DEFAULT_PM_TMR_BLK, 4) & PM_TIMER_MASK_24BIT;
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
    let t1_restore = pc2.io.read(DEFAULT_PM_TMR_BLK, 4) & PM_TIMER_MASK_24BIT;
    assert_eq!(t1_restore, t1);

    // Further ticking should advance from the restored point. Use absolute expected values to
    // avoid off-by-one issues due to integer division at fractional tick boundaries.
    let delta2_ns: u64 = 500_000; // 0.5ms
    pc2.tick(delta2_ns);
    let t2 = pc2.io.read(DEFAULT_PM_TMR_BLK, 4) & PM_TIMER_MASK_24BIT;
    let total_ns = (delta1_ns as u128) + (delta2_ns as u128);
    let expected_t2 = ((total_ns * PM_TIMER_FREQUENCY_HZ) / NS_PER_SEC) as u32;
    assert_eq!(t2, expected_t2 & PM_TIMER_MASK_24BIT);
}

// AHCI register offsets (HBA + port 0).
const HBA_GHC: u64 = 0x04;
const PORT_BASE: u64 = 0x100;
const PORT_REG_CLB: u64 = 0x00;
const PORT_REG_CLBU: u64 = 0x04;
const PORT_REG_FB: u64 = 0x08;
const PORT_REG_FBU: u64 = 0x0C;
const PORT_REG_IS: u64 = 0x10;
const PORT_REG_IE: u64 = 0x14;
const PORT_REG_CMD: u64 = 0x18;
const PORT_REG_CI: u64 = 0x38;

const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;

const PORT_IS_DHRS: u32 = 1 << 0;

#[derive(Clone)]
struct SharedDisk {
    data: Arc<Mutex<Vec<u8>>>,
    capacity: u64,
}

impl SharedDisk {
    fn new(sectors: usize) -> Self {
        let capacity = sectors
            .checked_mul(SECTOR_SIZE)
            .expect("disk capacity overflow");
        Self {
            data: Arc::new(Mutex::new(vec![0u8; capacity])),
            capacity: capacity as u64,
        }
    }
}

impl VirtualDisk for SharedDisk {
    fn capacity_bytes(&self) -> u64 {
        self.capacity
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.capacity {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.capacity,
            });
        }
        let guard = self.data.lock().unwrap();
        buf.copy_from_slice(&guard[offset as usize..end as usize]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> DiskResult<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.capacity {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.capacity,
            });
        }
        let mut guard = self.data.lock().unwrap();
        guard[offset as usize..end as usize].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> DiskResult<()> {
        Ok(())
    }
}

fn write_cmd_header(
    mem: &mut dyn memory::MemoryBus,
    clb: u64,
    slot: usize,
    ctba: u64,
    prdtl: u16,
    write: bool,
) {
    let cfl = 5u32;
    let w = if write { 1u32 << 6 } else { 0 };
    let flags = cfl | w | ((prdtl as u32) << 16);
    let addr = clb + (slot as u64) * 32;
    mem.write_u32(addr, flags);
    mem.write_u32(addr + 4, 0); // PRDBC
    mem.write_u32(addr + 8, ctba as u32);
    mem.write_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(mem: &mut dyn memory::MemoryBus, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    mem.write_u32(addr, dba as u32);
    mem.write_u32(addr + 4, (dba >> 32) as u32);
    mem.write_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    mem.write_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn write_cfis(mem: &mut dyn memory::MemoryBus, ctba: u64, command: u8, lba: u64, count: u16) {
    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = command;
    cfis[7] = 0x40; // LBA mode

    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;

    cfis[12] = (count & 0xFF) as u8;
    cfis[13] = (count >> 8) as u8;

    mem.write_physical(ctba, &cfis);
}

fn send_atapi_packet(
    io: &mut aero_platform::io::IoPortBus,
    base: u16,
    features: u8,
    pkt: &[u8; 12],
    byte_count: u16,
) {
    io.write(base + 1, 1, features as u32);
    io.write(base + 4, 1, (byte_count & 0xFF) as u32);
    io.write(base + 5, 1, (byte_count >> 8) as u32);
    io.write(base + 7, 1, 0xA0); // PACKET
    for i in 0..6 {
        let w = u16::from_le_bytes([pkt[i * 2], pkt[i * 2 + 1]]);
        io.write(base, 2, w as u32);
    }
}

#[derive(Debug)]
struct MemIso {
    sector_count: u32,
    data: Vec<u8>,
}

impl MemIso {
    fn new(sectors: u32) -> Self {
        Self {
            sector_count: sectors,
            data: vec![0u8; sectors as usize * 2048],
        }
    }
}

impl IsoBackend for MemIso {
    fn sector_count(&self) -> u32 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        if !buf.len().is_multiple_of(2048) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unaligned buffer length",
            ));
        }

        let start = lba as usize * 2048;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "overflow"))?;
        if end > self.data.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "OOB"));
        }

        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
}

#[test]
fn pc_platform_snapshot_roundtrip_preserves_storage_controllers_and_allows_backend_reattach() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    const AHCI_VECTOR: u8 = 0x70;

    let ahci_disk = SharedDisk::new(64);
    let ide_disk = SharedDisk::new(16);

    // Seed AHCI disk with a known pattern.
    let mut ahci_seed = vec![0u8; SECTOR_SIZE];
    ahci_seed[0..4].copy_from_slice(&[9, 8, 7, 6]);
    ahci_disk.clone().write_sectors(4, &ahci_seed).unwrap();

    // Seed IDE disk sector 0 so a PIO read has a visible prefix.
    let mut ide_seed = vec![0u8; SECTOR_SIZE];
    ide_seed[0..4].copy_from_slice(b"BOOT");
    ide_disk.clone().write_sectors(0, &ide_seed).unwrap();

    let mut iso = MemIso::new(2);
    iso.data[2048..2053].copy_from_slice(b"WORLD");

    // --- Setup: build a PcPlatform with AHCI + IDE (Win7 topology), attach disks, and enable
    // device decoding/DMA via PCI command registers.
    let mut pc = PcPlatform::new_with_win7_storage(RAM_SIZE);

    pc.attach_ahci_drive_port0(AtaDrive::new(Box::new(ahci_disk.clone())).unwrap());
    pc.attach_ide_primary_master_drive(AtaDrive::new(Box::new(ide_disk.clone())).unwrap());
    pc.attach_ide_secondary_master_atapi(AtapiCdrom::new(Some(Box::new(iso))));

    {
        let mut cfg_ports = pc.pci_cfg.borrow_mut();
        let bus = cfg_ports.bus_mut();
        bus.write_config(profile::SATA_AHCI_ICH9.bdf, 0x04, 2, 0x0006); // MEM + BUSMASTER
        bus.write_config(profile::IDE_PIIX3.bdf, 0x04, 2, 0x0005); // IO + BUSMASTER
    }

    let ahci_gsi = pc
        .pci_intx
        .gsi_for_intx(profile::SATA_AHCI_ICH9.bdf, PciInterruptPin::IntA);

    // Switch to APIC mode via IMCR so the IOAPIC path is active (matches how guests enable APIC).
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    {
        let mut ints = pc.interrupts.borrow_mut();

        // PCI INTx is active-low + level-triggered.
        let low = u32::from(AHCI_VECTOR) | (1 << 13) | (1 << 15);
        program_ioapic_entry(&mut ints, ahci_gsi, low, 0);
    }

    let ahci_abar = {
        let mut cfg_ports = pc.pci_cfg.borrow_mut();
        cfg_ports
            .bus_mut()
            .mapped_bar_range(profile::SATA_AHCI_ICH9.bdf, 5)
            .expect("AHCI BAR5 should be mapped after enabling MEM decode")
            .base
    };

    // --- AHCI: issue a READ DMA EXT to leave the controller in a non-default state (pending INTx).
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let data_buf = 0x4000u64;

    pc.memory
        .write_u32(ahci_abar + PORT_BASE + PORT_REG_CLB, clb as u32);
    pc.memory
        .write_u32(ahci_abar + PORT_BASE + PORT_REG_CLBU, (clb >> 32) as u32);
    pc.memory
        .write_u32(ahci_abar + PORT_BASE + PORT_REG_FB, fb as u32);
    pc.memory
        .write_u32(ahci_abar + PORT_BASE + PORT_REG_FBU, (fb >> 32) as u32);
    pc.memory
        .write_u32(ahci_abar + HBA_GHC, GHC_IE | GHC_AE);
    pc.memory
        .write_u32(ahci_abar + PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
    pc.memory.write_u32(
        ahci_abar + PORT_BASE + PORT_REG_CMD,
        PORT_CMD_ST | PORT_CMD_FRE,
    );

    write_cmd_header(&mut pc.memory, clb, 0, ctba, 1, false);
    write_cfis(&mut pc.memory, ctba, ATA_CMD_READ_DMA_EXT, 4, 1);
    write_prdt(&mut pc.memory, ctba, 0, data_buf, SECTOR_SIZE as u32);
    pc.memory
        .write_u32(ahci_abar + PORT_BASE + PORT_REG_CI, 1);

    pc.process_ahci();
    pc.poll_pci_intx_lines();

    assert!(pc.ahci.as_ref().unwrap().borrow().intx_level());
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(AHCI_VECTOR));
    {
        let mut ints = pc.interrupts.borrow_mut();
        assert!(
            ioapic_remote_irr_set(&mut ints, ahci_gsi),
            "level-triggered AHCI INTx should set IOAPIC Remote-IRR once delivered"
        );
    }

    let mut out = [0u8; 4];
    pc.memory.read_physical(data_buf, &mut out);
    assert_eq!(out, [9, 8, 7, 6]);

    // --- IDE: issue a PIO READ and leave the transfer mid-sector.
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x20);

    // Consume the first 4 bytes ("BOOT") but leave the transfer in progress.
    let w0 = pc.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let w1 = pc.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
    let mut first4 = [0u8; 4];
    first4[0..2].copy_from_slice(&w0.to_le_bytes());
    first4[2..4].copy_from_slice(&w1.to_le_bytes());
    assert_eq!(&first4, b"BOOT");

    // Trigger ATAPI UNIT ATTENTION and leave sense state pending.
    pc.io.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    let tur = [0u8; 12];
    send_atapi_packet(&mut pc.io, SECONDARY_PORTS.cmd_base, 0, &tur, 0);
    let _ = pc.io.read(SECONDARY_PORTS.cmd_base + 7, 1);

    // --- Snapshot deterministically: interrupts, PCI core, and both storage controllers.
    let interrupts_state = {
        let ints = pc.interrupts.borrow();
        deterministic_snapshot(&*ints, "PlatformInterrupts")
    };
    let pci_core_state = {
        let mut cfg_ports = pc.pci_cfg.borrow_mut();
        let core = PciCoreSnapshot::new(&mut cfg_ports, &mut pc.pci_intx);
        deterministic_snapshot(&core, "PCI core")
    };
    let ahci_state = {
        let ahci = pc.ahci.as_ref().unwrap().borrow();
        deterministic_snapshot(&*ahci, "AHCI")
    };
    let ide_state = {
        let ide = pc.ide.as_ref().unwrap().borrow();
        deterministic_snapshot(&*ide, "IDE")
    };

    // --- Restore into a fresh PcPlatform instance.
    let mut pc2 = PcPlatform::new_with_win7_storage(RAM_SIZE);

    pc2.interrupts
        .borrow_mut()
        .load_state(&interrupts_state)
        .unwrap();

    {
        let mut cfg_ports = pc2.pci_cfg.borrow_mut();
        let mut core = PciCoreSnapshot::new(&mut cfg_ports, &mut pc2.pci_intx);
        core.load_state(&pci_core_state).unwrap();
        let mut ints = pc2.interrupts.borrow_mut();
        let ints: &mut PlatformInterrupts = &mut ints;
        core.sync_intx_levels_to_sink(ints);
    }

    pc2.ahci
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&ahci_state)
        .unwrap();
    pc2.ide
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&ide_state)
        .unwrap();

    let ahci_abar2 = {
        let mut cfg_ports = pc2.pci_cfg.borrow_mut();
        cfg_ports
            .bus_mut()
            .mapped_bar_range(profile::SATA_AHCI_ICH9.bdf, 5)
            .expect("AHCI BAR5 should still be mapped after restore")
            .base
    };

    // Verify key AHCI register state and that the pending INTx survived restore.
    assert_eq!(
        pc2.memory.read_u32(ahci_abar2 + HBA_GHC),
        GHC_IE | GHC_AE,
        "AHCI GHC should survive snapshot/restore"
    );
    assert_eq!(
        pc2.memory.read_u32(ahci_abar2 + PORT_BASE + PORT_REG_CLB),
        clb as u32,
        "AHCI PxCLB should survive snapshot/restore"
    );
    assert!(pc2.ahci.as_ref().unwrap().borrow().intx_level());
    assert_eq!(pc2.interrupts.borrow().get_pending(), Some(AHCI_VECTOR));

    // Reattach storage backends.
    pc2.attach_ahci_drive_port0(AtaDrive::new(Box::new(ahci_disk.clone())).unwrap());
    pc2.attach_ide_primary_master_drive(AtaDrive::new(Box::new(ide_disk.clone())).unwrap());
    pc2.ide
        .as_ref()
        .unwrap()
        .borrow_mut()
        .controller
        .attach_secondary_master_atapi_backend_for_restore(Box::new(MemIso::new(2)));

    // Acknowledge the restored AHCI interrupt, clear it in the device, and ensure Remote-IRR clears
    // on EOI without re-delivery.
    pc2.interrupts.borrow_mut().acknowledge(AHCI_VECTOR);
    assert_eq!(pc2.interrupts.borrow().get_pending(), None);

    pc2.memory
        .write_u32(ahci_abar2 + PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
    pc2.poll_pci_intx_lines();

    {
        let mut ints = pc2.interrupts.borrow_mut();
        assert!(
            ioapic_remote_irr_set(&mut ints, ahci_gsi),
            "Remote-IRR should remain set until EOI"
        );
        ints.eoi(AHCI_VECTOR);
        assert!(
            !ioapic_remote_irr_set(&mut ints, ahci_gsi),
            "EOI should clear Remote-IRR once INTx is deasserted"
        );
    }
    assert_eq!(
        pc2.interrupts.borrow().get_pending(),
        None,
        "AHCI interrupt should not be re-delivered after EOI once deasserted"
    );

    // --- Continue with AHCI: perform a WRITE DMA EXT after restore.
    let write_buf = 0x5000u64;
    let mut sector = vec![0u8; SECTOR_SIZE];
    sector[0..4].copy_from_slice(&[1, 2, 3, 4]);
    pc2.memory.write_physical(write_buf, &sector);

    write_cmd_header(&mut pc2.memory, clb, 0, ctba, 1, true);
    write_cfis(&mut pc2.memory, ctba, ATA_CMD_WRITE_DMA_EXT, 5, 1);
    write_prdt(&mut pc2.memory, ctba, 0, write_buf, SECTOR_SIZE as u32);
    pc2.memory
        .write_u32(ahci_abar2 + PORT_BASE + PORT_REG_CI, 1);

    pc2.process_ahci();
    pc2.poll_pci_intx_lines();
    assert!(pc2.ahci.as_ref().unwrap().borrow().intx_level());

    let mut verify = vec![0u8; SECTOR_SIZE];
    ahci_disk.clone().read_sectors(5, &mut verify).unwrap();
    assert_eq!(&verify[..4], &[1, 2, 3, 4]);

    // --- Continue the restored IDE PIO read: read the rest of the sector.
    let mut buf = vec![0u8; SECTOR_SIZE];
    buf[0..4].copy_from_slice(b"BOOT");
    for i in 2..(SECTOR_SIZE / 2) {
        let w = pc2.io.read(PRIMARY_PORTS.cmd_base, 2) as u16;
        buf[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(&buf[0..4], b"BOOT");

    // Reading status clears the pending IRQ.
    let _ = pc2.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    assert!(!pc2
        .ide
        .as_ref()
        .unwrap()
        .borrow()
        .controller
        .primary_irq_pending());

    // --- Verify ATAPI sense state still reports UNIT ATTENTION / medium changed.
    pc2.io.write(SECONDARY_PORTS.cmd_base + 6, 1, 0xA0);
    let mut req_sense = [0u8; 12];
    req_sense[0] = 0x03;
    req_sense[4] = 18;
    send_atapi_packet(&mut pc2.io, SECONDARY_PORTS.cmd_base, 0, &req_sense, 18);

    let mut sense = [0u8; 18];
    for i in 0..(18 / 2) {
        let w = pc2.io.read(SECONDARY_PORTS.cmd_base, 2) as u16;
        sense[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    assert_eq!(sense[2] & 0x0F, 0x06); // UNIT ATTENTION
    assert_eq!(sense[12], 0x28); // MEDIUM CHANGED

    // --- Perform an IDE PIO write after restore to ensure the reattached disk backend is used.
    pc2.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc2.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc2.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 1);
    pc2.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc2.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc2.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0x30); // WRITE SECTORS

    pc2.io.write(
        PRIMARY_PORTS.cmd_base,
        2,
        u32::from(u16::from_le_bytes([5, 6])),
    );
    pc2.io.write(
        PRIMARY_PORTS.cmd_base,
        2,
        u32::from(u16::from_le_bytes([7, 8])),
    );
    for _ in 0..((SECTOR_SIZE / 2) - 2) {
        pc2.io.write(PRIMARY_PORTS.cmd_base, 2, 0);
    }

    let mut verify = vec![0u8; SECTOR_SIZE];
    ide_disk.clone().read_sectors(1, &mut verify).unwrap();
    assert_eq!(&verify[..4], &[5, 6, 7, 8]);
}
