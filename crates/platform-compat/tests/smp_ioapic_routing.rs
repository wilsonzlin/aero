use platform::interrupts::{InterruptInput, PlatformInterruptMode, PlatformInterrupts};

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn ioapic_redirection_destination_routes_to_non_bsp_lapic() {
    let mut ints = PlatformInterrupts::new_with_cpu_count(2);
    ints.set_mode(PlatformInterruptMode::Apic);

    let gsi = 5u32;
    let vector = 0x50u32;
    program_ioapic_entry(&mut ints, gsi, vector, 1u32 << 24);

    ints.raise_irq(InterruptInput::Gsi(gsi));

    assert_eq!(ints.lapic(1).get_pending_vector(), Some(vector as u8));
    assert_eq!(ints.lapic(0).get_pending_vector(), None);
}

#[test]
fn ioapic_redirection_destination_ff_broadcasts_to_all_lapics() {
    let mut ints = PlatformInterrupts::new_with_cpu_count(2);
    ints.set_mode(PlatformInterruptMode::Apic);

    let gsi = 6u32;
    let vector = 0x52u32;
    program_ioapic_entry(&mut ints, gsi, vector, 0xFFu32 << 24);

    ints.raise_irq(InterruptInput::Gsi(gsi));

    assert_eq!(ints.lapic(0).get_pending_vector(), Some(vector as u8));
    assert_eq!(ints.lapic(1).get_pending_vector(), Some(vector as u8));
}

