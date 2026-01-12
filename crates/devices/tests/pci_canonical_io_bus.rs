use aero_devices::pci::profile::{
    build_canonical_io_bus, IDE_PIIX3, ISA_PIIX3, USB_UHCI_PIIX3, VIRTIO_INPUT_KEYBOARD,
    VIRTIO_INPUT_MOUSE,
};
use aero_devices::pci::{PciBdf, PciBus, PciConfigMechanism1, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn read_dword(cfg: &mut PciConfigMechanism1, bus: &mut PciBus, bdf: PciBdf, offset: u16) -> u32 {
    cfg.io_write(bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    cfg.io_read(bus, PCI_CFG_DATA_PORT, 4)
}

fn read_vendor_device(
    cfg: &mut PciConfigMechanism1,
    bus: &mut PciBus,
    bdf: PciBdf,
) -> Option<(u16, u16)> {
    let id = read_dword(cfg, bus, bdf, 0x00);
    let vendor = (id & 0xFFFF) as u16;
    if vendor == 0xFFFF {
        return None;
    }
    let device = (id >> 16) as u16;
    Some((vendor, device))
}

fn read_header_type(cfg: &mut PciConfigMechanism1, bus: &mut PciBus, bdf: PciBdf) -> u8 {
    let hdr = read_dword(cfg, bus, bdf, 0x0C);
    ((hdr >> 16) & 0xFF) as u8
}

fn enumerate_bus0(cfg: &mut PciConfigMechanism1, bus: &mut PciBus) -> Vec<PciBdf> {
    let mut found = Vec::new();

    for device in 0u8..32 {
        let fn0 = PciBdf::new(0, device, 0);
        let Some((_vendor, _device)) = read_vendor_device(cfg, bus, fn0) else {
            continue;
        };
        found.push(fn0);

        let header_type = read_header_type(cfg, bus, fn0);
        let functions = if header_type & 0x80 != 0 { 8 } else { 1 };
        for function in 1u8..functions {
            let bdf = PciBdf::new(0, device, function);
            if read_vendor_device(cfg, bus, bdf).is_some() {
                found.push(bdf);
            }
        }
    }

    found
}

#[test]
fn canonical_io_bus_enumerates_multifunction_devices() {
    let mut bus = build_canonical_io_bus();
    let mut cfg = PciConfigMechanism1::new();

    let mut found = enumerate_bus0(&mut cfg, &mut bus);
    found.sort();

    // PIIX3 must present a function 0 with the multifunction bit set so that OSes discover the
    // IDE + UHCI functions at 00:01.1 and 00:01.2.
    assert!(found.contains(&ISA_PIIX3.bdf), "missing {}", ISA_PIIX3.name);
    assert!(found.contains(&IDE_PIIX3.bdf), "missing {}", IDE_PIIX3.name);
    assert!(
        found.contains(&USB_UHCI_PIIX3.bdf),
        "missing {}",
        USB_UHCI_PIIX3.name
    );
    assert_eq!(
        read_header_type(&mut cfg, &mut bus, ISA_PIIX3.bdf) & 0x80,
        0x80
    );

    // virtio-input must enumerate as a multi-function device (keyboard fn0, mouse fn1).
    assert!(
        found.contains(&VIRTIO_INPUT_KEYBOARD.bdf),
        "missing virtio-input keyboard"
    );
    assert!(
        found.contains(&VIRTIO_INPUT_MOUSE.bdf),
        "missing virtio-input mouse"
    );
    assert_eq!(
        read_header_type(&mut cfg, &mut bus, VIRTIO_INPUT_KEYBOARD.bdf) & 0x80,
        0x80
    );
}
