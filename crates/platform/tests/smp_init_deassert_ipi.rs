use aero_platform::interrupts::PlatformInterrupts;

fn lapic_read_u32(ints: &PlatformInterrupts, apic_id: u8, offset: u64) -> u32 {
    let mut buf = [0u8; 4];
    ints.lapic_mmio_read_for_apic(apic_id, offset, &mut buf);
    u32::from_le_bytes(buf)
}

fn lapic_write_u32(ints: &PlatformInterrupts, apic_id: u8, offset: u64, value: u32) {
    ints.lapic_mmio_write_for_apic(apic_id, offset, &value.to_le_bytes());
}

#[test]
fn init_deassert_ipi_is_ignored_and_does_not_reset_target_lapic() {
    // Create a 2-vCPU interrupt fabric (LAPIC0 + LAPIC1).
    let ints = PlatformInterrupts::new_with_cpu_count(2);

    // Pending INIT flags start clear.
    assert!(!ints.take_pending_init(0));
    assert!(!ints.take_pending_init(1));

    // Set some LAPIC1 state that would be cleared on reset.
    lapic_write_u32(&ints, 1, 0x80, 0x70); // TPR
    assert_eq!(lapic_read_u32(&ints, 1, 0x80), 0x70);

    // Send INIT IPI with level=deassert from CPU0 to CPU1.
    //
    // ICR_HIGH[63:56] is the destination APIC ID for xAPIC mode.
    lapic_write_u32(&ints, 0, 0x310, 1u32 << 24);
    // ICR_LOW[10:8] delivery mode = INIT (0b101), ICR_LOW[14] level = deassert (0).
    lapic_write_u32(&ints, 0, 0x300, 5u32 << 8);

    // Deassert should be a no-op: no pending INIT and no LAPIC reset.
    assert!(!ints.take_pending_init(1));
    assert_eq!(lapic_read_u32(&ints, 1, 0x80), 0x70);
}

#[test]
fn init_assert_ipi_sets_pending_init_and_resets_target_lapic() {
    let ints = PlatformInterrupts::new_with_cpu_count(2);

    // Pending INIT flags start clear.
    assert!(!ints.take_pending_init(0));
    assert!(!ints.take_pending_init(1));

    // Set some LAPIC1 state that should be cleared by INIT reset semantics.
    lapic_write_u32(&ints, 1, 0x80, 0x70); // TPR
    assert_eq!(lapic_read_u32(&ints, 1, 0x80), 0x70);

    // Send INIT IPI with level=assert from CPU0 to CPU1.
    lapic_write_u32(&ints, 0, 0x310, 1u32 << 24);
    lapic_write_u32(&ints, 0, 0x300, (5u32 << 8) | (1u32 << 14));

    assert!(ints.take_pending_init(1));
    assert_eq!(lapic_read_u32(&ints, 1, 0x80), 0);
    // Flag is consumed on take.
    assert!(!ints.take_pending_init(1));
}
