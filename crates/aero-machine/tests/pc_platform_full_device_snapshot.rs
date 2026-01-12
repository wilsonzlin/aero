use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::acpi_pm::{DEFAULT_ACPI_ENABLE, DEFAULT_PM1A_CNT_BLK, DEFAULT_SMI_CMD_PORT};
use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::i8042::{I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_devices::pci::{PciBdf, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};
use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController, InterruptInput, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX,
    IMCR_SELECT_PORT,
};
use pretty_assertions::assert_eq;

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn ioapic_write(m: &mut Machine, reg: u32, value: u32) {
    // IOREGSEL at +0, IOWIN at +0x10.
    m.write_physical_u32(IOAPIC_MMIO_BASE, reg);
    m.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, value);
}

fn ioapic_read(m: &mut Machine, reg: u32) -> u32 {
    m.write_physical_u32(IOAPIC_MMIO_BASE, reg);
    m.read_physical_u32(IOAPIC_MMIO_BASE + 0x10)
}

fn program_ioapic_entry(m: &mut Machine, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ioapic_write(m, redtbl_low, low);
    ioapic_write(m, redtbl_high, high);
}

fn read_ioapic_entry(m: &mut Machine, gsi: u32) -> (u32, u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    (ioapic_read(m, redtbl_low), ioapic_read(m, redtbl_high))
}

#[test]
fn snapshot_restore_preserves_full_pc_platform_device_state() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: true,
        enable_a20_gate: true,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Enable A20 so MMIO regions (IOAPIC/HPET) are reachable without address line 20 wrapping.
    enable_a20(&mut src);

    // Switch IMCR to APIC mode (and keep selector latched).
    src.io_write(IMCR_SELECT_PORT, 1, u32::from(IMCR_INDEX));
    src.io_write(IMCR_DATA_PORT, 1, 0x01);

    // Configure IOAPIC redirection entries:
    // - PIT (ISA IRQ0 is mapped to GSI2 by default via ACPI ISO) => edge-triggered, unmasked.
    // - PCI INTx (PIRQ[A-D] defaults to GSIs 10-13; use device 0 INTA# => GSI10) => active-low,
    //   level-triggered, masked.
    // - HPET timer0 => route to GSI4 => active-high, level-triggered, masked.
    let pit_vector = 0x40u8;
    let intx_vector = 0x41u8;
    let hpet_vector = 0x42u8;

    let gsi_pit = 2u32;
    let gsi_intx = 10u32;
    let gsi_hpet = 4u32;

    program_ioapic_entry(&mut src, gsi_pit, u32::from(pit_vector), 0);
    // active-low (bit 13), level-triggered (bit 15), masked (bit 16).
    program_ioapic_entry(
        &mut src,
        gsi_intx,
        u32::from(intx_vector) | (1 << 13) | (1 << 15) | (1 << 16),
        0,
    );
    // level-triggered (bit 15), masked (bit 16).
    program_ioapic_entry(
        &mut src,
        gsi_hpet,
        u32::from(hpet_vector) | (1 << 15) | (1 << 16),
        0,
    );

    let expected_ioapic_pit = read_ioapic_entry(&mut src, gsi_pit);
    let expected_ioapic_intx = read_ioapic_entry(&mut src, gsi_intx);
    let expected_ioapic_hpet = read_ioapic_entry(&mut src, gsi_hpet);

    // Program PIT channel0 periodic mode (mode 2) with a small reload divisor.
    src.io_write(PIT_CMD, 1, 0x34);
    src.io_write(PIT_CH0, 1, 0x20);
    src.io_write(PIT_CH0, 1, 0x00);

    // Read back the current channel0 count via a latch command so we can compare after restore.
    src.io_write(PIT_CMD, 1, 0x00);
    let pit_count_lo = src.io_read(PIT_CH0, 1) as u8;
    let pit_count_hi = src.io_read(PIT_CH0, 1) as u8;
    let expected_pit_count = u16::from_le_bytes([pit_count_lo, pit_count_hi]);

    // Program HPET timer0 for a level-triggered interrupt on GSI4 that becomes pending immediately.
    // Use MMIO access through the machine bus to ensure the MMIO wiring itself is covered.
    let timer0_cfg = (u64::from(gsi_hpet) << 9) | (1 << 1) | (1 << 2);
    src.write_physical_u64(HPET_MMIO_BASE + 0x100, timer0_cfg);
    src.write_physical_u64(HPET_MMIO_BASE + 0x108, 0);
    src.write_physical_u64(HPET_MMIO_BASE + 0x010, 1);

    let expected_hpet_general_cfg = src.read_physical_u64(HPET_MMIO_BASE + 0x010);
    let expected_hpet_timer0_cfg = src.read_physical_u64(HPET_MMIO_BASE + 0x100);
    let expected_hpet_timer0_cmp = src.read_physical_u64(HPET_MMIO_BASE + 0x108);
    let expected_hpet_int_status = src.read_physical_u64(HPET_MMIO_BASE + 0x020);
    assert_ne!(
        expected_hpet_int_status & 1,
        0,
        "expected HPET timer0 interrupt to be pending"
    );

    // Enable ACPI via the standard SMI_CMD handshake.
    src.io_write(DEFAULT_SMI_CMD_PORT, 1, u32::from(DEFAULT_ACPI_ENABLE));
    let expected_pm1_cnt = src.io_read(DEFAULT_PM1A_CNT_BLK, 2) as u16;

    // Mutate PCI config space through the standard 0xCF8/0xCFC config ports. Use the host bridge
    // at 00:00.0 and change the COMMAND register (offset 0x04).
    let cfg_addr = 0x8000_0000u32 | (0x04u32 & 0xFC);
    let command: u16 = 0x0007;
    src.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr);
    src.io_write(PCI_CFG_DATA_PORT, 2, u32::from(command));

    let expected_cf8 = src.io_read(PCI_CFG_ADDR_PORT, 4);
    let expected_cfc = src.io_read(PCI_CFG_DATA_PORT, 2) as u16;
    assert_eq!(expected_cf8, cfg_addr);
    assert_eq!(expected_cfc, command);

    // Assert a PCI INTx line via the router (00:00.0 INTA# => GSI10).
    let interrupts = src.platform_interrupts().expect("pc platform enabled");
    let pci_intx = src.pci_intx_router().expect("pc platform enabled");
    {
        let mut ints = interrupts.borrow_mut();
        pci_intx
            .borrow_mut()
            .assert_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntA, &mut *ints);
    }

    // Corrupt the interrupt controller's sink level state so restore must re-drive:
    // - PCI INTx levels via `PciIntxRouter::sync_levels_to_sink()`
    // - HPET level lines via `Hpet::sync_levels_to_sink()`.
    interrupts
        .borrow_mut()
        .lower_irq(InterruptInput::Gsi(gsi_intx));
    interrupts
        .borrow_mut()
        .lower_irq(InterruptInput::Gsi(gsi_hpet));

    // Queue some i8042 controller output bytes (one in the output buffer, the rest in the pending
    // queue). Use the i8042 command 0xD2 ("write output buffer as keyboard").
    for byte in [0xAAu8, 0xBB, 0xCC] {
        src.io_write(I8042_STATUS_PORT, 1, 0xD2);
        src.io_write(I8042_DATA_PORT, 1, u32::from(byte));
    }
    let expected_i8042_status = src.io_read(I8042_STATUS_PORT, 1) as u8;

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Verify key register/port images round-trip.
    assert_eq!(restored.io_read(IMCR_SELECT_PORT, 1) as u8, IMCR_INDEX);
    assert_eq!(restored.io_read(IMCR_DATA_PORT, 1) as u8, 0x01);

    assert_eq!(restored.io_read(PCI_CFG_ADDR_PORT, 4), expected_cf8);
    assert_eq!(restored.io_read(PCI_CFG_DATA_PORT, 2) as u16, expected_cfc);

    assert_eq!(
        read_ioapic_entry(&mut restored, gsi_pit),
        expected_ioapic_pit
    );
    assert_eq!(
        read_ioapic_entry(&mut restored, gsi_intx),
        expected_ioapic_intx
    );
    assert_eq!(
        read_ioapic_entry(&mut restored, gsi_hpet),
        expected_ioapic_hpet
    );

    assert_eq!(
        restored.read_physical_u64(HPET_MMIO_BASE + 0x010),
        expected_hpet_general_cfg
    );
    assert_eq!(
        restored.read_physical_u64(HPET_MMIO_BASE + 0x100),
        expected_hpet_timer0_cfg
    );
    assert_eq!(
        restored.read_physical_u64(HPET_MMIO_BASE + 0x108),
        expected_hpet_timer0_cmp
    );
    assert_eq!(
        restored.read_physical_u64(HPET_MMIO_BASE + 0x020),
        expected_hpet_int_status
    );

    assert_eq!(
        restored.io_read(DEFAULT_PM1A_CNT_BLK, 2) as u16,
        expected_pm1_cnt
    );

    // PIT count should be identical immediately after restore (no platform time has advanced yet).
    restored.io_write(PIT_CMD, 1, 0x00);
    let restored_pit_count_lo = restored.io_read(PIT_CH0, 1) as u8;
    let restored_pit_count_hi = restored.io_read(PIT_CH0, 1) as u8;
    let restored_pit_count = u16::from_le_bytes([restored_pit_count_lo, restored_pit_count_hi]);
    assert_eq!(restored_pit_count, expected_pit_count);

    // i8042 status image preserved.
    assert_eq!(
        restored.io_read(I8042_STATUS_PORT, 1) as u8,
        expected_i8042_status
    );

    // i8042 output bytes preserved.
    let mut out = Vec::new();
    for _ in 0..3 {
        out.push(restored.io_read(I8042_DATA_PORT, 1) as u8);
    }
    assert_eq!(out, vec![0xAA, 0xBB, 0xCC]);

    // At least one timer interrupt (PIT) should still be delivered after restore using only
    // deterministic platform ticking.
    let interrupts = restored.platform_interrupts().expect("pc platform enabled");
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().get_pending(), None);
    restored.tick_platform(1_000_000);
    assert_eq!(interrupts.borrow().get_pending(), Some(pit_vector));
}
