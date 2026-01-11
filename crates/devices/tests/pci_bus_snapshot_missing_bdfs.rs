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
fn pci_bus_snapshot_ignores_bdfs_missing_in_target_bus() {
    let bdf_a = PciBdf::new(0, 1, 0);
    let bdf_b = PciBdf::new(0, 2, 0);

    let mut bus = PciBus::new();
    bus.add_device(bdf_a, Box::new(StubDev::new(0x1234, 0x0001)));
    bus.add_device(bdf_b, Box::new(StubDev::new(0x1234, 0x0002)));

    // Mutate state so restore is observable.
    bus.write_config(bdf_a, 0x04, 2, 0x0002);
    bus.write_config(bdf_b, 0x04, 2, 0x0001);

    let snapshot = PciBusSnapshot::save_from(&bus);

    let mut bus2 = PciBus::new();
    bus2.add_device(bdf_a, Box::new(StubDev::new(0x1234, 0x0001)));

    snapshot.restore_into(&mut bus2).unwrap();

    assert_eq!(bus2.read_config(bdf_a, 0x04, 2) as u16, 0x0002);

    // BDF 00:02.0 is absent; config reads should still float high.
    assert_eq!(bus2.read_config(bdf_b, 0x00, 4), 0xFFFF_FFFF);
}

#[test]
fn pci_bus_snapshot_leaves_devices_absent_in_snapshot_unchanged() {
    let bdf_a = PciBdf::new(0, 1, 0);
    let bdf_c = PciBdf::new(0, 3, 0);

    let mut bus = PciBus::new();
    bus.add_device(bdf_a, Box::new(StubDev::new(0x1234, 0x0001)));
    bus.write_config(bdf_a, 0x04, 2, 0x0002);

    let snapshot = PciBusSnapshot::save_from(&bus);

    let mut bus2 = PciBus::new();
    bus2.add_device(bdf_a, Box::new(StubDev::new(0x1234, 0x0001)));
    bus2.add_device(bdf_c, Box::new(StubDev::new(0x1234, 0x0003)));

    snapshot.restore_into(&mut bus2).unwrap();

    // Restored device matches snapshot.
    assert_eq!(bus2.read_config(bdf_a, 0x04, 2) as u16, 0x0002);

    // Device C was not in the snapshot; it should remain at power-on defaults.
    assert_eq!(bus2.read_config(bdf_c, 0x00, 4), 0x0003_1234);
    assert_eq!(bus2.read_config(bdf_c, 0x04, 2) as u16, 0x0000);
}
