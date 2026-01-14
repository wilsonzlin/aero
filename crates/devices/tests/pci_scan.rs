use aero_devices::pci::{
    PciBdf, PciBus, PciConfigMechanism1, PciConfigSpace, PciDevice, PCI_CFG_ADDR_PORT,
    PCI_CFG_DATA_PORT,
};

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn read_dword(
    cfg: &mut PciConfigMechanism1,
    bus: &mut PciBus,
    b: u8,
    d: u8,
    f: u8,
    offset: u8,
) -> u32 {
    cfg.io_write(bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(b, d, f, offset));
    cfg.io_read(bus, PCI_CFG_DATA_PORT, 4)
}

#[test]
fn pci_scan_enumerates_registered_functions() {
    struct Stub {
        cfg: PciConfigSpace,
    }

    impl Stub {
        fn new(vendor_id: u16, device_id: u16) -> Self {
            Self {
                cfg: PciConfigSpace::new(vendor_id, device_id),
            }
        }
    }

    impl PciDevice for Stub {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    let mut bus = PciBus::new();
    let mut cfg = PciConfigMechanism1::new();

    // Host bridge at 00:00.0
    let mut host_bridge = Stub::new(0x8086, 0x1237);
    host_bridge.cfg.set_class_code(0x06, 0x00, 0x00, 0x00);
    bus.add_device(PciBdf::new(0, 0, 0), Box::new(host_bridge));

    // Single-function device at 00:01.0
    let mut dev = Stub::new(0x1234, 0x5678);
    dev.cfg.set_class_code(0x01, 0x06, 0x01, 0x01);
    bus.add_device(PciBdf::new(0, 1, 0), Box::new(dev));

    // Multifunction device at 00:02.{0,1}
    let mut fn0 = Stub::new(0x1111, 0x0001);
    fn0.cfg.set_header_type(0x80); // multifunction bit
    bus.add_device(PciBdf::new(0, 2, 0), Box::new(fn0));
    let fn1 = Stub::new(0x2222, 0x0002);
    bus.add_device(PciBdf::new(0, 2, 1), Box::new(fn1));

    let mut found = Vec::new();
    for device in 0..32u8 {
        let id = read_dword(&mut cfg, &mut bus, 0, device, 0, 0x00);
        let vendor = (id & 0xFFFF) as u16;
        if vendor == 0xFFFF {
            continue;
        }
        found.push((device, 0u8));

        let hdr = read_dword(&mut cfg, &mut bus, 0, device, 0, 0x0C);
        let header_type = ((hdr >> 16) & 0xFF) as u8;
        let functions = if header_type & 0x80 != 0 { 8 } else { 1 };
        for function in 1..functions {
            let id = read_dword(&mut cfg, &mut bus, 0, device, function, 0x00);
            let vendor = (id & 0xFFFF) as u16;
            if vendor == 0xFFFF {
                continue;
            }
            found.push((device, function));
        }
    }

    found.sort();
    assert_eq!(found, vec![(0, 0), (1, 0), (2, 0), (2, 1)]);
}
