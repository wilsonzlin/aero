use aero_devices::pci::{
    MsiCapability, PciBarDefinition, PciBdf, PciBus, PciConfigPorts, PciConfigSpace, PciDevice,
    PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
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

fn cfg_read(ports: &mut PciConfigPorts, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    ports.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    ports.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(ports: &mut PciConfigPorts, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    ports.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    ports.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
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
fn pci_config_ports_snapshot_roundtrip_preserves_state() {
    let (bus, bdf) = make_bus();
    let mut ports = PciConfigPorts::with_bus(bus);

    // BAR0 probe and program.
    cfg_write(&mut ports, bdf, 0x10, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut ports, bdf, 0x10, 4), 0xFFFF_F000);
    cfg_write(&mut ports, bdf, 0x10, 4, 0x1234_5000);

    // BAR1 probe and program.
    cfg_write(&mut ports, bdf, 0x14, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut ports, bdf, 0x14, 4), 0xFFFF_FFE1);
    cfg_write(&mut ports, bdf, 0x14, 4, 0x0000_C200);
    assert_eq!(cfg_read(&mut ports, bdf, 0x14, 4), 0x0000_C201);

    // Enable memory + IO decoding and ensure BARs are mapped.
    cfg_write(&mut ports, bdf, 0x04, 2, 0x0003);
    assert_eq!(ports.bus_mut().mapped_mmio_bars().len(), 1);
    assert_eq!(ports.bus_mut().mapped_io_bars().len(), 1);

    // Program MSI and set the pending bit by triggering while masked.
    let cap_offset = ports
        .bus_mut()
        .device_config_mut(bdf)
        .unwrap()
        .find_capability(aero_devices::pci::msi::PCI_CAP_ID_MSI)
        .unwrap() as u16;

    cfg_write(&mut ports, bdf, cap_offset + 0x04, 4, 0xfee0_0000);
    cfg_write(&mut ports, bdf, cap_offset + 0x08, 4, 0);
    cfg_write(&mut ports, bdf, cap_offset + 0x0c, 2, 0x0045);
    let ctrl = cfg_read(&mut ports, bdf, cap_offset + 0x02, 2) as u16;
    cfg_write(
        &mut ports,
        bdf,
        cap_offset + 0x02,
        2,
        u32::from(ctrl | 0x0001),
    );
    cfg_write(&mut ports, bdf, cap_offset + 0x10, 4, 1); // mask vector 0

    {
        let msi = ports
            .bus_mut()
            .device_config_mut(bdf)
            .unwrap()
            .capability_mut::<MsiCapability>()
            .unwrap();
        let mut sink = MsiSink::default();
        assert!(!msi.trigger(&mut sink));
        assert!(sink.messages.is_empty());
        assert_eq!(msi.pending_bits() & 1, 1);
    }

    // Leave BAR0 in the probed state so BAR probe flags are exercised.
    cfg_write(&mut ports, bdf, 0x10, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut ports, bdf, 0x10, 4), 0xFFFF_F000);

    // Ensure the 0xCF8 address latch is restored.
    ports.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, 0x3c));
    let latch = ports.io_read(PCI_CFG_ADDR_PORT, 4);

    let bytes = ports.save_state();
    let bytes2 = ports.save_state();
    assert_eq!(bytes, bytes2, "snapshot bytes must be deterministic");

    let (bus2, _) = make_bus();
    let mut ports2 = PciConfigPorts::with_bus(bus2);
    ports2.load_state(&bytes).unwrap();

    assert_eq!(ports2.io_read(PCI_CFG_ADDR_PORT, 4), latch);

    // Compare guest-visible config reads (DWORD granularity).
    for offset in (0..256u16).step_by(4) {
        let a = ports.bus_mut().read_config(bdf, offset, 4);
        let b = ports2.bus_mut().read_config(bdf, offset, 4);
        assert_eq!(a, b, "config mismatch at offset {offset:#04x}");
    }

    assert_eq!(
        ports.bus_mut().mapped_bars(),
        ports2.bus_mut().mapped_bars()
    );

    let msi1 = ports
        .bus_mut()
        .device_config(bdf)
        .unwrap()
        .capability::<MsiCapability>()
        .unwrap();
    let msi2 = ports2
        .bus_mut()
        .device_config(bdf)
        .unwrap()
        .capability::<MsiCapability>()
        .unwrap();

    assert_eq!(msi1.enabled(), msi2.enabled());
    assert_eq!(msi1.message_address(), msi2.message_address());
    assert_eq!(msi1.message_data(), msi2.message_data());
    assert_eq!(msi1.mask_bits(), msi2.mask_bits());
    assert_eq!(msi1.pending_bits(), msi2.pending_bits());
}
