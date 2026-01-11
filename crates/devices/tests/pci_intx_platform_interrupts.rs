use aero_devices::pci::{PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, PlatformInterrupts,
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
fn pci_intx_delivers_via_ioapic_when_platform_interrupts_in_apic_mode() {
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

    // Route GSI10 to vector 0x45.
    //
    // PCI INTx lines are active-low + level-triggered.
    let vector = 0x45u32;
    let low = vector | (1 << 13) | (1 << 15);
    program_ioapic_entry(&mut interrupts, 10, low, 0);

    // Device 0 INTA# routes to PIRQ A -> GSI 10.
    router.assert_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntA, &mut interrupts);
    assert_eq!(interrupts.get_pending(), Some(0x45));
}

#[test]
fn pci_intx_can_be_delivered_via_pic_in_legacy_mode_through_platform_interrupts() {
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut interrupts = PlatformInterrupts::new();
    interrupts.pic_mut().set_offsets(0x20, 0x28);
    interrupts.set_mode(PlatformInterruptMode::LegacyPic);

    router.assert_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntA, &mut interrupts);

    // IRQ10 is on the slave PIC (IRQ2 on the slave) -> vector 0x28 + 2.
    assert_eq!(interrupts.get_pending(), Some(0x2A));
}
