use aero_platform::interrupts::{
    InterruptController, InterruptInput, PlatformInterruptMode, PlatformInterrupts,
};

#[test]
fn guest_programs_ioapic_redirection_entry_and_receives_vector() {
    let mut ints = PlatformInterrupts::new();
    ints.set_mode(PlatformInterruptMode::Apic);

    let gsi = 1u32;
    let vector = 0x41u8;
    let redir_low_index = 0x10u8 + (2 * gsi as u8);
    let redir_high_index = redir_low_index + 1;

    ints.ioapic_mmio_write(0x00, redir_low_index as u32);
    ints.ioapic_mmio_write(0x10, vector as u32);

    ints.ioapic_mmio_write(0x00, redir_high_index as u32);
    ints.ioapic_mmio_write(0x10, 0);

    ints.raise_irq(InterruptInput::Gsi(gsi));
    assert_eq!(ints.get_pending(), Some(vector));

    ints.acknowledge(vector);
    ints.lower_irq(InterruptInput::Gsi(gsi));
    ints.eoi(vector);
}
