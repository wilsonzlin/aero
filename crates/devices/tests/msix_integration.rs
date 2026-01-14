use aero_devices::pci::{msix::PCI_CAP_ID_MSIX, MsixCapability, PciConfigSpace};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};

#[test]
fn msix_table_delivers_vector_to_lapic_and_mask_sets_pba() {
    let mut config = PciConfigSpace::new(0x1234, 0x5678);
    config.add_capability(Box::new(MsixCapability::new(1, 0, 0x1000, 0, 0x2000)));

    let cap_offset = config.find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;

    // Enable MSI-X by setting the Message Control enable bit (bit 15).
    let ctrl = config.read(cap_offset + 0x02, 2) as u16;
    config.write(cap_offset + 0x02, 2, u32::from(ctrl | (1 << 15)));
    assert!(config.capability::<MsixCapability>().unwrap().enabled());

    let table_index: u16 = 0;
    let msi_vector: u8 = 0x45;
    // Use physical destination broadcast to avoid depending on the LAPIC APIC ID value.
    let msi_address: u64 = 0xFEE0_0000u64 | (0xFFu64 << 12);

    // Program the MSI-X table entry: address + data + unmasked.
    {
        let msix = config.capability_mut::<MsixCapability>().unwrap();
        let base = u64::from(table_index) * 16;
        msix.table_write(base, &(msi_address as u32).to_le_bytes());
        msix.table_write(base + 0x4, &((msi_address >> 32) as u32).to_le_bytes());
        msix.table_write(base + 0x8, &(u32::from(msi_vector)).to_le_bytes());
        msix.table_write(base + 0xc, &0u32.to_le_bytes());
    }

    let mut interrupts = PlatformInterrupts::new();
    interrupts.set_mode(PlatformInterruptMode::Apic);

    // Trigger the table entry and validate the vector reaches the LAPIC pending queue.
    {
        let msix = config.capability_mut::<MsixCapability>().unwrap();
        assert!(msix.trigger_into(table_index, &mut interrupts));
    }
    assert_eq!(interrupts.get_pending(), Some(msi_vector));

    // Clear the interrupt so a subsequent trigger isn't satisfied by the previous pending vector.
    interrupts.acknowledge(msi_vector);
    interrupts.eoi(msi_vector);
    assert_eq!(interrupts.get_pending(), None);

    // Mask the table entry and ensure triggers set PBA instead of delivering to the LAPIC.
    {
        let msix = config.capability_mut::<MsixCapability>().unwrap();
        let base = u64::from(table_index) * 16;
        msix.table_write(base + 0xc, &1u32.to_le_bytes());
    }

    {
        let msix = config.capability_mut::<MsixCapability>().unwrap();
        assert!(!msix.trigger_into(table_index, &mut interrupts));
        assert_eq!(interrupts.get_pending(), None);

        let mut pba = [0u8; 8];
        msix.pba_read(0, &mut pba);
        let bits = u64::from_le_bytes(pba);
        assert_eq!(
            bits & (1u64 << u32::from(table_index)),
            1u64 << u32::from(table_index)
        );
    }
}
