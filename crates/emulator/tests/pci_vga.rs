use emulator::devices::pci::{PciBus, CONFIG_ADDRESS_PORT, CONFIG_DATA_PORT};
use emulator::devices::vga::pci::{
    VgaPciFunction, DEFAULT_LFB_BASE, DEFAULT_LFB_SIZE, VGA_PCI_CLASS_CODE, VGA_PCI_DEVICE_ID,
    VGA_PCI_SUBCLASS, VGA_PCI_VENDOR_ID,
};

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000u32
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

#[test]
fn pci_vga_device_enumerates_with_lfb_bar() {
    let mut bus = PciBus::default();
    // Mirror the canonical machine's transitional VGA/VBE PCI stub BDF (`00:0c.0`) rather than
    // using an arbitrary low device number that may be reserved by other integration topologies.
    bus.insert_function(0, 0x0c, 0, VgaPciFunction::new());

    bus.io_write(CONFIG_ADDRESS_PORT, 4, cfg_addr(0, 0x0c, 0, 0x00));
    let id = bus.io_read(CONFIG_DATA_PORT, 4);
    assert_eq!(id & 0xFFFF, VGA_PCI_VENDOR_ID as u32);
    assert_eq!(id >> 16, VGA_PCI_DEVICE_ID as u32);

    bus.io_write(CONFIG_ADDRESS_PORT, 4, cfg_addr(0, 0x0c, 0, 0x08));
    let class_reg = bus.io_read(CONFIG_DATA_PORT, 4);
    let class_code = ((class_reg >> 24) & 0xFF) as u8;
    let subclass = ((class_reg >> 16) & 0xFF) as u8;
    assert_eq!(class_code, VGA_PCI_CLASS_CODE);
    assert_eq!(subclass, VGA_PCI_SUBCLASS);

    bus.io_write(CONFIG_ADDRESS_PORT, 4, cfg_addr(0, 0x0c, 0, 0x0C));
    let hdr = bus.io_read(CONFIG_DATA_PORT, 4);
    let header_type = ((hdr >> 16) & 0xFF) as u8;
    assert_eq!(header_type, 0x00);

    bus.io_write(CONFIG_ADDRESS_PORT, 4, cfg_addr(0, 0x0c, 0, 0x10));
    let bar0 = bus.io_read(CONFIG_DATA_PORT, 4);
    assert_eq!(bar0 & 0xFFFF_FFF0, DEFAULT_LFB_BASE);
    assert_eq!(bar0 & 0x8, 0x8);

    // BAR sizing probe: write all 1s and confirm the size mask is returned.
    bus.io_write(CONFIG_DATA_PORT, 4, 0xFFFF_FFFF);
    let mask = bus.io_read(CONFIG_DATA_PORT, 4);
    assert_eq!(mask & 0x8, 0x8);
    assert_eq!(mask & 0xFFFF_FFF0, (!(DEFAULT_LFB_SIZE - 1)) & 0xFFFF_FFF0);

    // Program a new base and ensure VBE's PhysBasePtr follows the BAR.
    let new_base = 0xD000_0000u32;
    bus.io_write(CONFIG_DATA_PORT, 4, new_base);
    let bar0_after = bus.io_read(CONFIG_DATA_PORT, 4);
    assert_eq!(bar0_after & 0xFFFF_FFF0, new_base);

    let vga = bus
        .function_mut_typed::<VgaPciFunction>(0, 0x0c, 0)
        .expect("vga function present");
    assert_eq!(vga.vbe_phys_base_ptr(), new_base);
}
