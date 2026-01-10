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
    let mut ioapic = IoApic::new(IoApicId(0), lapic.clone());

    // Configure GSI 10 -> vector 0x45, unmasked, edge-triggered.
    let gsi = 10u32;
    let vector = 0x45u8;
    let redtbl_low = 0x10u32 + (gsi * 2);
    let redtbl_high = redtbl_low + 1;

    ioapic.mmio_write(0x00, 4, u64::from(redtbl_low));
    ioapic.mmio_write(0x10, 4, u64::from(vector));

    ioapic.mmio_write(0x00, 4, u64::from(redtbl_high));
    ioapic.mmio_write(0x10, 4, 0u64);

    let mut pic = DualPic8259::new();
    init_legacy_pc(&mut pic);

    let mut sink = IoApicPicMirrorSink::new(&mut ioapic, &mut pic);

    // Device 0 INTA# routes to PIRQ A -> GSI/IRQ 10 in the default mapping.
    let bdf = PciBdf::new(0, 0, 0);
    router.assert_intx(bdf, PciInterruptPin::IntA, &mut sink);

    assert_eq!(lapic.pop_pending(), Some(vector));
    assert_eq!(pic.get_pending_vector(), Some(0x2A)); // IRQ10 -> slave IRQ2 -> vector 0x28+2
}

