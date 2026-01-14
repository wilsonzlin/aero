use aero_devices::pci::{MsixCapability, PciConfigSpace};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};

#[test]
fn msix_pending_bit_redelivered_on_vector_unmask() {
    let mut config = PciConfigSpace::new(0x1234, 0x5678);
    config.add_capability(Box::new(MsixCapability::new(1, 0, 0x1000, 0, 0x2000)));
    let cap_offset = config
        .find_capability(aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .unwrap() as u16;

    // Enable MSI-X.
    let ctrl = config.read(cap_offset + 0x02, 2) as u16;
    config.write(cap_offset + 0x02, 2, u32::from(ctrl | (1 << 15)));

    // Program entry 0 with a valid MSI message, but keep it masked.
    {
        let msix = config.capability_mut::<MsixCapability>().unwrap();
        msix.table_write(0x0, &0xfee0_0000u32.to_le_bytes());
        msix.table_write(0x4, &0u32.to_le_bytes());
        msix.table_write(0x8, &0x0045u32.to_le_bytes());
        msix.table_write(0xc, &1u32.to_le_bytes()); // masked
    }

    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

    let msix = config.capability_mut::<MsixCapability>().unwrap();

    // Trigger while masked: no delivery, but PBA pending bit should be set.
    assert!(!msix.trigger_into(0, &mut interrupts));
    assert_eq!(interrupts.get_pending(), None);

    let mut pba = [0u8; 8];
    msix.pba_read(0, &mut pba);
    let bits = u64::from_le_bytes(pba);
    assert_eq!(bits & 1, 1);

    // Unmask the entry and drain pending; the interrupt should be delivered now.
    msix.table_write(0xc, &0u32.to_le_bytes());
    assert_eq!(msix.deliver_pending_into(&mut interrupts), 1);
    assert_eq!(interrupts.get_pending(), Some(0x45));

    msix.pba_read(0, &mut pba);
    let bits = u64::from_le_bytes(pba);
    assert_eq!(bits & 1, 0);
}
