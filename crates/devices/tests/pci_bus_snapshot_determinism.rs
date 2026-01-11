use aero_devices::pci::{PciBdf, PciBus, PciBusSnapshot, PciConfigSpace, PciDevice};
use aero_io_snapshot::io::state::IoSnapshot;

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
fn pci_bus_snapshot_is_deterministic_across_device_insertion_order() {
    let bdf0 = PciBdf::new(0, 1, 0);
    let bdf1 = PciBdf::new(0, 2, 0);

    let mut bus_a = PciBus::new();
    bus_a.add_device(bdf0, Box::new(StubDev::new(0x1234, 0x0001)));
    bus_a.add_device(bdf1, Box::new(StubDev::new(0x1234, 0x0002)));

    let mut bus_b = PciBus::new();
    // Reverse insertion order (BTreeMap should canonicalize).
    bus_b.add_device(bdf1, Box::new(StubDev::new(0x1234, 0x0002)));
    bus_b.add_device(bdf0, Box::new(StubDev::new(0x1234, 0x0001)));

    let bytes_a = PciBusSnapshot::save_from(&bus_a).save_state();
    let bytes_b = PciBusSnapshot::save_from(&bus_b).save_state();
    assert_eq!(bytes_a, bytes_b);
}

#[test]
fn pci_bus_snapshot_roundtrips_bytes_through_load_save() {
    let bdf0 = PciBdf::new(0, 1, 0);

    let mut bus = PciBus::new();
    bus.add_device(bdf0, Box::new(StubDev::new(0x1234, 0x0001)));

    let bytes = PciBusSnapshot::save_from(&bus).save_state();

    let mut decoded = PciBusSnapshot::default();
    decoded.load_state(&bytes).unwrap();
    let bytes2 = decoded.save_state();

    assert_eq!(bytes, bytes2);
}

