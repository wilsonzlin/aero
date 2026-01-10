use aero_devices::apic::{IoApic, IoApicId, LocalApic};
use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use std::sync::Arc;

#[test]
fn hpet_timer0_interrupt_is_routed_via_ioapic_to_lapic() {
    let clock = ManualClock::new();
    let lapic = Arc::new(LocalApic::new(0));
    lapic.mmio_write(0xF0, &(1u32 << 8).to_le_bytes());
    let mut ioapic = IoApic::new(IoApicId(0), lapic.clone());

    // Route HPET Timer 0 (default GSI 2) to vector 0x42.
    let gsi = 2u32;
    let vector = 0x42u8;
    let redtbl_low = 0x10u32 + (gsi * 2);
    let redtbl_high = redtbl_low + 1;

    ioapic.mmio_write(0x00, 4, u64::from(redtbl_low));
    ioapic.mmio_write(0x10, 4, u64::from(vector)); // Fixed + physical + unmasked, edge-triggered.

    ioapic.mmio_write(0x00, 4, u64::from(redtbl_high));
    ioapic.mmio_write(0x10, 4, 0u64); // Route to LAPIC 0.

    let mut hpet = Hpet::new_default(clock.clone());

    // Enable HPET + timer 0 interrupt.
    hpet.mmio_write(0x010, 8, 1, &mut ioapic);
    let timer0_cfg = hpet.mmio_read(0x100, 8, &mut ioapic);
    hpet.mmio_write(0x100, 8, timer0_cfg | (1 << 2), &mut ioapic);
    hpet.mmio_write(0x108, 8, 3, &mut ioapic);

    clock.advance_ns(300);
    hpet.poll(&mut ioapic);

    assert_eq!(lapic.get_pending_vector(), Some(vector));
    assert_ne!(hpet.mmio_read(0x020, 8, &mut ioapic) & 1, 0);
}
