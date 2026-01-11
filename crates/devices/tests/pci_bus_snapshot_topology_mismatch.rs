use aero_devices::pci::{PciBdf, PciBus, PciBusSnapshot, PciConfigSpace, PciDevice};

struct StubDev {
    cfg: PciConfigSpace,
}

impl StubDev {
    fn new(vendor_id: u16, device_id: u16) -> Self {
        Self {
            cfg: PciConfigSpace::new(vendor_id, device_id),
        }
    }
}

impl PciDevice for StubDev {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

#[test]
fn pci_bus_snapshot_skips_restore_when_bdf_device_type_mismatches() {
    let bdf = PciBdf::new(0, 1, 0);

    let mut bus = PciBus::new();
    bus.add_device(bdf, Box::new(StubDev::new(0x1234, 0x0001)));

    let snapshot = PciBusSnapshot::save_from(&bus);

    let mut bus2 = PciBus::new();
    // Same BDF, but different device_id: restore should be skipped.
    bus2.add_device(bdf, Box::new(StubDev::new(0x1234, 0x0002)));

    snapshot.restore_into(&mut bus2).unwrap();

    // Vendor/device ID read should remain the new device.
    assert_eq!(bus2.read_config(bdf, 0x00, 4), 0x0002_1234);
}
