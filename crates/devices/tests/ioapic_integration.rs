use aero_devices::apic::{IoApic, IoApicId, LocalApic};
use std::sync::Arc;

#[test]
fn ioapic_routes_configured_vector_to_lapic() {
    let lapic = Arc::new(LocalApic::new(0));
    let mut ioapic = IoApic::new(IoApicId(0), lapic.clone());

    // Configure GSI 5 -> vector 0x45, unmasked, edge-triggered.
    let gsi = 5u32;
    let vector = 0x45u8;
    let redtbl_low = 0x10u32 + (gsi * 2);
    let redtbl_high = redtbl_low + 1;

    ioapic.mmio_write(0x00, 4, u64::from(redtbl_low));
    ioapic.mmio_write(0x10, 4, u64::from(vector)); // Fixed + physical + unmasked.

    ioapic.mmio_write(0x00, 4, u64::from(redtbl_high));
    ioapic.mmio_write(0x10, 4, 0u64); // Route to LAPIC 0.

    ioapic.set_irq_level(gsi, true);
    assert_eq!(lapic.pop_pending(), Some(vector));
}
