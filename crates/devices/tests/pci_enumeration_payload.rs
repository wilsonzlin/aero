use aero_devices::pci::{
    PciBdf, PciBus, PciConfigMechanism1, PciPlatform, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(bus) << 16)
        | (u32::from(device) << 11)
        | (u32::from(function) << 8)
        | u32::from(offset)
}

/// A tiny "payload" that enumerates bus 0 using PCI configuration mechanism #1.
fn enumerate_bus0(cfg: &mut PciConfigMechanism1, bus: &mut PciBus) -> Vec<(PciBdf, u16, u16)> {
    let mut found = Vec::new();
    for device in 0u8..32 {
        for function in 0u8..8 {
            let addr = cfg_addr(0, device, function, 0);

            cfg.io_write(bus, PCI_CFG_ADDR_PORT, 4, addr);
            let id = cfg.io_read(bus, PCI_CFG_DATA_PORT, 4);
            let vendor = (id & 0xFFFF) as u16;
            if vendor == 0xFFFF {
                // If function 0 is absent, functions 1..7 are guaranteed absent too.
                if function == 0 {
                    break;
                }
                continue;
            }
            let device_id = ((id >> 16) & 0xFFFF) as u16;
            found.push((PciBdf::new(0, device, function), vendor, device_id));
        }
    }
    found
}

#[test]
fn payload_sees_root_chipset_devices() {
    let mut bus = PciPlatform::build_bus();
    let mut cfg = PciConfigMechanism1::new();

    let devices = enumerate_bus0(&mut cfg, &mut bus);

    assert!(
        devices
            .iter()
            .any(|(bdf, vendor, _)| *bdf == PciBdf::new(0, 0, 0) && *vendor == 0x8086),
        "expected to find PCI host bridge at 00:00.0"
    );
    assert!(
        devices
            .iter()
            .any(|(bdf, vendor, _)| *bdf == PciBdf::new(0, 0x1f, 0) && *vendor == 0x8086),
        "expected to find ISA/LPC bridge at 00:1f.0"
    );
}
