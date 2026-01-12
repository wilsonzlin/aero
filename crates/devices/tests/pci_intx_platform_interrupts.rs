use aero_devices::pci::{PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn pci_intx_delivers_via_ioapic_when_platform_interrupts_in_apic_mode() {
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

    let bdf = PciBdf::new(0, 0, 0);
    let pin = PciInterruptPin::IntA;
    let gsi = router.gsi_for_intx(bdf, pin);

    // Route the routed GSI to vector 0x45.
    // PCI INTx lines are active-low + level-triggered.
    let vector = 0x45u32;
    let low = vector | (1 << 13) | (1 << 15);
    program_ioapic_entry(&mut interrupts, gsi, low, 0);

    router.assert_intx(bdf, pin, &mut interrupts);
    assert_eq!(interrupts.get_pending(), Some(0x45));
}

#[test]
fn pci_intx_can_be_delivered_via_pic_in_legacy_mode_through_platform_interrupts() {
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut interrupts = PlatformInterrupts::new();
    interrupts.pic_mut().set_offsets(0x20, 0x28);
    interrupts.set_mode(PlatformInterruptMode::LegacyPic);

    let bdf = PciBdf::new(0, 0, 0);
    let pin = PciInterruptPin::IntA;
    let gsi = router.gsi_for_intx(bdf, pin);
    assert!(
        gsi < 16,
        "expected PCI INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let irq = u8::try_from(gsi).unwrap();
    if irq >= 8 {
        interrupts.pic_mut().set_masked(2, false); // cascade
    }
    interrupts.pic_mut().set_masked(irq, false);

    router.assert_intx(bdf, pin, &mut interrupts);

    let expected = if irq < 8 { 0x20 + irq } else { 0x28 + (irq - 8) };
    assert_eq!(interrupts.get_pending(), Some(expected));
}
