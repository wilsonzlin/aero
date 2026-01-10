use aero_devices::pci::{PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};
use aero_platform::interrupts::{
    InterruptController, IoApicRedirectionEntry, PlatformInterruptMode, PlatformInterrupts,
    TriggerMode,
};

#[test]
fn pci_intx_delivers_via_ioapic_when_platform_interrupts_in_apic_mode() {
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

    // Route GSI10 to vector 0x45.
    let mut entry = IoApicRedirectionEntry::fixed(0x45, 0);
    entry.masked = false;
    entry.trigger = TriggerMode::Edge;
    interrupts.ioapic_mut().set_entry(10, entry);

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

