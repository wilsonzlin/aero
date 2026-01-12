use aero_io_snapshot::io::state::IoSnapshot;
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

#[test]
fn snapshot_determinism_is_byte_identical() {
    let mut ints = PlatformInterrupts::new();
    ints.set_mode(PlatformInterruptMode::Apic);

    program_ioapic_entry(&mut ints, 1, 0x31, 0);

    ints.raise_irq(InterruptInput::Gsi(1));

    let a = ints.save_state();
    let b = ints.save_state();
    assert_eq!(a, b);
}

#[test]
fn snapshot_round_trip_legacy_pic_preserves_pending_and_eoi_flow() {
    let mut ints = PlatformInterrupts::new();
    ints.pic_mut().set_offsets(0x20, 0x28);

    ints.raise_irq(InterruptInput::IsaIrq(1));
    assert_eq!(ints.get_pending(), Some(0x21));

    let bytes = ints.save_state();

    let mut restored = PlatformInterrupts::new();
    restored.load_state(&bytes).unwrap();
    restored.finalize_restore();

    assert_eq!(restored.get_pending(), Some(0x21));

    restored.acknowledge(0x21);
    assert_eq!(restored.get_pending(), None);
    restored.eoi(0x21);

    restored.lower_irq(InterruptInput::IsaIrq(1));
    restored.raise_irq(InterruptInput::IsaIrq(1));
    assert_eq!(restored.get_pending(), Some(0x21));
}

#[test]
fn snapshot_round_trip_apic_edge_preserves_pending_vector() {
    let mut ints = PlatformInterrupts::new();
    ints.set_mode(PlatformInterruptMode::Apic);

    let vector = 0x31u8;
    program_ioapic_entry(&mut ints, 1, u32::from(vector), 0);

    ints.raise_irq(InterruptInput::Gsi(1));
    ints.lower_irq(InterruptInput::Gsi(1));
    assert_eq!(ints.get_pending(), Some(vector));

    let bytes = ints.save_state();

    let mut restored = PlatformInterrupts::new();
    restored.load_state(&bytes).unwrap();
    restored.finalize_restore();

    assert_eq!(restored.get_pending(), Some(vector));

    restored.acknowledge(vector);
    assert_eq!(restored.get_pending(), None);
    restored.eoi(vector);

    restored.raise_irq(InterruptInput::Gsi(1));
    assert_eq!(restored.get_pending(), Some(vector));
}

#[test]
fn snapshot_round_trip_apic_level_preserves_remote_irr_semantics() {
    let mut ints = PlatformInterrupts::new();
    ints.set_mode(PlatformInterruptMode::Apic);

    let vector = 0x40u8;
    program_ioapic_entry(&mut ints, 1, u32::from(vector) | (1 << 15), 0);

    // Raise and ACK the interrupt but keep the line asserted so Remote-IRR remains set.
    ints.raise_irq(InterruptInput::Gsi(1));
    assert_eq!(ints.get_pending(), Some(vector));
    ints.acknowledge(vector);
    assert_eq!(ints.get_pending(), None);

    let expected = ints.clone();
    let bytes = ints.save_state();

    let mut restored = PlatformInterrupts::new();
    restored.load_state(&bytes).unwrap();
    restored.finalize_restore();

    // EOI while still asserted should re-deliver (Remote-IRR gating survives snapshot).
    let mut expected = expected;
    expected.eoi(vector);
    restored.eoi(vector);
    assert_eq!(expected.get_pending(), Some(vector));
    assert_eq!(restored.get_pending(), Some(vector));

    // Clearing the line before EOI should stop re-delivery.
    expected.acknowledge(vector);
    restored.acknowledge(vector);
    expected.lower_irq(InterruptInput::Gsi(1));
    restored.lower_irq(InterruptInput::Gsi(1));
    expected.eoi(vector);
    restored.eoi(vector);
    assert_eq!(expected.get_pending(), None);
    assert_eq!(restored.get_pending(), None);
}
