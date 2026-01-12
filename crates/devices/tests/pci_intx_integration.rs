use aero_devices::apic::{IoApic, IoApicId, LocalApic};
use aero_devices::pci::{
    IoApicPicMirrorSink, PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};
use aero_devices::pic8259::{DualPic8259, MASTER_CMD, MASTER_DATA, SLAVE_CMD, SLAVE_DATA};
use std::sync::Arc;

fn init_legacy_pc(pic: &mut DualPic8259) {
    // Initialize master PIC: base 0x20, slave on IRQ2, 8086 mode.
    pic.port_write_u8(MASTER_CMD, 0x11);
    pic.port_write_u8(MASTER_DATA, 0x20);
    pic.port_write_u8(MASTER_DATA, 0x04);
    pic.port_write_u8(MASTER_DATA, 0x01);

    // Initialize slave PIC: base 0x28, cascade identity 2, 8086 mode.
    pic.port_write_u8(SLAVE_CMD, 0x11);
    pic.port_write_u8(SLAVE_DATA, 0x28);
    pic.port_write_u8(SLAVE_DATA, 0x02);
    pic.port_write_u8(SLAVE_DATA, 0x01);
}

#[test]
fn pci_intx_can_drive_ioapic_and_be_mirrored_to_pic() {
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());

    let lapic = Arc::new(LocalApic::new(0));
    // Enable LAPIC with spurious vector 0xFF so injected interrupts are accepted.
    lapic.mmio_write(0xF0, &(0x1FFu32).to_le_bytes());
    let mut ioapic = IoApic::new(IoApicId(0), lapic.clone());

    // Configure the routed GSI -> vector 0x45, unmasked, active-low, level-triggered.
    let bdf = PciBdf::new(0, 0, 0);
    let pin = PciInterruptPin::IntA;
    let gsi = router.gsi_for_intx(bdf, pin);
    assert!(
        gsi < 16,
        "expected PCI INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let vector = 0x45u8;
    let redtbl_low = 0x10u32 + (gsi * 2);
    let redtbl_high = redtbl_low + 1;

    ioapic.mmio_write(0x00, 4, u64::from(redtbl_low));
    // PCI INTx is active-low, level-triggered.
    let low = u32::from(vector) | (1 << 13) | (1 << 15);
    ioapic.mmio_write(0x10, 4, u64::from(low));

    ioapic.mmio_write(0x00, 4, u64::from(redtbl_high));
    ioapic.mmio_write(0x10, 4, 0u64);

    let mut pic = DualPic8259::new();
    init_legacy_pc(&mut pic);

    let mut sink = IoApicPicMirrorSink::new(&mut ioapic, &mut pic);

    router.assert_intx(bdf, pin, &mut sink);

    assert_eq!(lapic.get_pending_vector(), Some(vector));
    let irq = u8::try_from(gsi).unwrap();
    let expected_pic = if irq < 8 { 0x20 + irq } else { 0x28 + (irq - 8) };
    assert_eq!(pic.get_pending_vector(), Some(expected_pic));
}
