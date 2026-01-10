use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::ioapic::{GsiEvent, IoApic};

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

#[test]
fn guest_can_find_hpet_via_acpi_and_program_timer0() {
    let clock = ManualClock::new();
    let mut ioapic = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    // Firmware would normally publish ACPI tables in guest RAM. Build them and
    // extract the HPET table blob.
    let cfg = AcpiConfig::default();
    let tables = AcpiTables::build(&cfg, AcpiPlacement::default());
    let hpet_table = &tables.hpet;

    assert_eq!(&hpet_table[0..4], b"HPET");
    let acpi_hpet_id = read_u32_le(hpet_table, 36);
    let base_address = read_u64_le(hpet_table, 44);

    // Guest OS probes ACPI and discovers the HPET base address.
    assert_eq!(base_address, HPET_MMIO_BASE);

    // Guest reads registers and validates the counter period.
    let caps = hpet.mmio_read(0x000, 8, &mut ioapic);
    assert_eq!((caps & 0xffff_ffff) as u32, acpi_hpet_id);
    let period_fs = (caps >> 32) as u32;
    assert_eq!(period_fs, 100_000_000);

    // Reset the main counter while disabled, then enable HPET.
    hpet.mmio_write(0x0F0, 8, 0, &mut ioapic);
    hpet.mmio_write(0x010, 8, 1, &mut ioapic);

    // Configure Timer 0: enable interrupts (edge-triggered).
    let timer0_cfg = hpet.mmio_read(0x100, 8, &mut ioapic);
    hpet.mmio_write(0x100, 8, timer0_cfg | (1 << 2), &mut ioapic);

    // Fire at main_counter == 3 (300ns with a 100ns period).
    hpet.mmio_write(0x108, 8, 3, &mut ioapic);

    clock.advance_ns(200);
    hpet.poll(&mut ioapic);
    assert!(ioapic.take_events().is_empty());

    clock.advance_ns(100);
    hpet.poll(&mut ioapic);
    assert_eq!(
        ioapic.take_events(),
        vec![GsiEvent::Raise(2), GsiEvent::Lower(2)]
    );

    // Guest acknowledges the interrupt via the General Interrupt Status register.
    assert_ne!(hpet.mmio_read(0x020, 8, &mut ioapic) & 1, 0);
    hpet.mmio_write(0x020, 8, 1, &mut ioapic);
    assert_eq!(hpet.mmio_read(0x020, 8, &mut ioapic) & 1, 0);
}
