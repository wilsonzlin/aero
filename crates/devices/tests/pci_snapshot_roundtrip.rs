use aero_devices::pci::{
    MsiCapability, PciBarDefinition, PciBdf, PciBus, PciBusSnapshot, PciConfigMechanism1,
    PciConfigSpace, PciDevice, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(
    cfg: &mut PciConfigMechanism1,
    bus: &mut PciBus,
    bdf: PciBdf,
    offset: u16,
    size: u8,
) -> u32 {
    cfg.io_write(bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    let port = PCI_CFG_DATA_PORT + (offset & 3);
    cfg.io_read(bus, port, size)
}

fn cfg_write(
    cfg: &mut PciConfigMechanism1,
    bus: &mut PciBus,
    bdf: PciBdf,
    offset: u16,
    size: u8,
    value: u32,
) {
    cfg.io_write(bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    let port = PCI_CFG_DATA_PORT + (offset & 3);
    cfg.io_write(bus, port, size, value);
}

#[derive(Default)]
struct MsiSink {
    messages: Vec<MsiMessage>,
}

impl MsiTrigger for MsiSink {
    fn trigger_msi(&mut self, msg: MsiMessage) {
        self.messages.push(msg);
    }
}

struct TestDevice {
    cfg: PciConfigSpace,
}

impl TestDevice {
    fn new() -> Self {
        let mut cfg = PciConfigSpace::new(0x1234, 0x0001);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        cfg.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });
        cfg.add_capability(Box::new(MsiCapability::new()));
        Self { cfg }
    }
}

impl PciDevice for TestDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

fn make_bus() -> (PciBus, PciBdf) {
    let mut bus = PciBus::new();
    let bdf = PciBdf::new(0, 1, 0);
    bus.add_device(bdf, Box::new(TestDevice::new()));
    (bus, bdf)
}

#[test]
fn pci_snapshot_roundtrip_preserves_config_bars_and_msi_state() {
    let (mut bus, bdf) = make_bus();
    let mut cfg = PciConfigMechanism1::new();

    // BAR0 probe.
    cfg_write(&mut cfg, &mut bus, bdf, 0x10, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut cfg, &mut bus, bdf, 0x10, 4), 0xFFFF_F000);

    // Program BAR0 and BAR1.
    cfg_write(&mut cfg, &mut bus, bdf, 0x10, 4, 0x1234_5000);
    cfg_write(&mut cfg, &mut bus, bdf, 0x14, 4, 0x0000_C200);
    assert_eq!(cfg_read(&mut cfg, &mut bus, bdf, 0x14, 4), 0x0000_C201);

    // Enable memory + IO decoding and ensure BARs are mapped.
    cfg_write(&mut cfg, &mut bus, bdf, 0x04, 2, 0x0003);
    assert_eq!(bus.mapped_mmio_bars().len(), 1);
    assert_eq!(bus.mapped_io_bars().len(), 1);

    // Program MSI via config space writes, then mask and trigger to set the pending bit.
    let cap_offset = bus
        .device_config_mut(bdf)
        .unwrap()
        .find_capability(aero_devices::pci::msi::PCI_CAP_ID_MSI)
        .unwrap() as u16;

    cfg_write(&mut cfg, &mut bus, bdf, cap_offset + 0x04, 4, 0xfee0_0000);
    cfg_write(&mut cfg, &mut bus, bdf, cap_offset + 0x08, 4, 0);
    cfg_write(&mut cfg, &mut bus, bdf, cap_offset + 0x0c, 2, 0x0045);
    let ctrl = cfg_read(&mut cfg, &mut bus, bdf, cap_offset + 0x02, 2) as u16;
    cfg_write(
        &mut cfg,
        &mut bus,
        bdf,
        cap_offset + 0x02,
        2,
        u32::from(ctrl | 0x0001),
    );
    cfg_write(&mut cfg, &mut bus, bdf, cap_offset + 0x10, 4, 1); // mask vector 0

    {
        let mut sink = MsiSink::default();
        let msi = bus
            .device_config_mut(bdf)
            .unwrap()
            .capability_mut::<MsiCapability>()
            .unwrap();
        assert!(!msi.trigger(&mut sink));
        assert!(sink.messages.is_empty());
        assert_eq!(msi.pending_bits() & 1, 1);
    }

    // Leave BAR0 in the probed state: this is guest-visible via config reads, but not via the
    // raw config bytes, so it must be snapshotted explicitly.
    cfg_write(&mut cfg, &mut bus, bdf, 0x10, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut cfg, &mut bus, bdf, 0x10, 4), 0xFFFF_F000);

    // Snapshot both the bus state and the 0xCF8 address latch.
    let bus_snapshot = PciBusSnapshot::save_from(&bus);
    let bus_bytes = bus_snapshot.save_state();
    let bus_bytes2 = PciBusSnapshot::save_from(&bus).save_state();
    assert_eq!(
        bus_bytes, bus_bytes2,
        "snapshot bytes must be deterministic"
    );

    let cfg_bytes = cfg.save_state();

    // Restore into a freshly-constructed bus with the same topology.
    let (mut bus2, bdf2) = make_bus();
    assert_eq!(bdf2, bdf);
    let mut cfg2 = PciConfigMechanism1::new();
    cfg2.load_state(&cfg_bytes).unwrap();

    let mut restored = PciBusSnapshot::default();
    restored.load_state(&bus_bytes).unwrap();
    restored.restore_into(&mut bus2).unwrap();

    // Config reads should match for the full 256-byte space (DWORD granularity is sufficient
    // because the config mechanism is byte-addressable).
    for offset in (0..256u16).step_by(4) {
        let a = bus.read_config(bdf, offset, 4);
        let b = bus2.read_config(bdf, offset, 4);
        assert_eq!(a, b, "config mismatch at offset {offset:#04x}");
    }

    assert_eq!(bus.mapped_bars(), bus2.mapped_bars());

    let msi1 = bus
        .device_config(bdf)
        .unwrap()
        .capability::<MsiCapability>()
        .unwrap();
    let msi2 = bus2
        .device_config(bdf)
        .unwrap()
        .capability::<MsiCapability>()
        .unwrap();
    assert_eq!(msi1.enabled(), msi2.enabled());
    assert_eq!(msi1.message_address(), msi2.message_address());
    assert_eq!(msi1.message_data(), msi2.message_data());
    assert_eq!(msi1.mask_bits(), msi2.mask_bits());
    assert_eq!(msi1.pending_bits(), msi2.pending_bits());

    // Pending bit should also be observable via config-space reads (and must survive restore).
    assert_eq!(
        bus.read_config(bdf, cap_offset + 0x14, 4),
        bus2.read_config(bdf, cap_offset + 0x14, 4)
    );

    // Config address latch should roundtrip as well.
    assert_eq!(
        cfg.io_read(&mut bus, PCI_CFG_ADDR_PORT, 4),
        cfg2.io_read(&mut bus2, PCI_CFG_ADDR_PORT, 4)
    );
}
