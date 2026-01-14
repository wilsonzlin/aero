use aero_devices::pci::{
    PciBarDefinition, PciBdf, PciBus, PciBusSnapshot, PciConfigMechanism1, PciConfigSpace,
    PciDevice, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_io_snapshot::io::state::IoSnapshot;

const BAR0_SIZE: u64 = 0x4000;
const BAR0_FLAGS: u32 = 0x4; // 64-bit MMIO BAR type bits (bits 2:1 = 0b10).

fn mmio64_probe_masks(size: u64) -> (u32, u32) {
    assert!(size.is_power_of_two());
    let mask = !(size - 1);
    ((mask as u32) | BAR0_FLAGS, (mask >> 32) as u32)
}

fn mmio64_programmed_lo(size: u64, requested: u32) -> u32 {
    let size = u32::try_from(size).expect("BAR0 size should fit in u32 for this test");
    (requested & !(size - 1)) | BAR0_FLAGS
}

fn mmio64_base(programmed_lo: u32, programmed_hi: u32) -> u64 {
    (u64::from(programmed_hi) << 32) | u64::from(programmed_lo & 0xffff_fff0)
}

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
    cfg.io_read(bus, PCI_CFG_DATA_PORT + (offset & 3), size)
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
    cfg.io_write(bus, PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

struct Mmio64Device {
    cfg: PciConfigSpace,
}

impl Mmio64Device {
    fn new() -> Self {
        let mut cfg = PciConfigSpace::new(0x1234, 0x0001);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: BAR0_SIZE,
                prefetchable: false,
            },
        );
        // BAR1 is the high dword of BAR0 (implicit).
        Self { cfg }
    }
}

impl PciDevice for Mmio64Device {
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
    bus.add_device(bdf, Box::new(Mmio64Device::new()));
    (bus, bdf)
}

#[test]
fn pci_snapshot_roundtrip_preserves_mmio64_bar_programming() {
    let (mut bus, bdf) = make_bus();
    let mut cfg = PciConfigMechanism1::new();

    // Probe BAR0 (64-bit).
    cfg_write(&mut cfg, &mut bus, bdf, 0x10, 4, 0xFFFF_FFFF);
    cfg_write(&mut cfg, &mut bus, bdf, 0x14, 4, 0xFFFF_FFFF);
    let (expected_probe_lo, expected_probe_hi) = mmio64_probe_masks(BAR0_SIZE);
    assert_eq!(
        cfg_read(&mut cfg, &mut bus, bdf, 0x10, 4),
        expected_probe_lo
    );
    assert_eq!(
        cfg_read(&mut cfg, &mut bus, bdf, 0x14, 4),
        expected_probe_hi
    );

    // Program BAR0 above 4GiB.
    let requested_lo = 0x2345_6000;
    let requested_hi = 0x0000_0001;
    let expected_lo = mmio64_programmed_lo(BAR0_SIZE, requested_lo);
    cfg_write(&mut cfg, &mut bus, bdf, 0x10, 4, requested_lo);
    cfg_write(&mut cfg, &mut bus, bdf, 0x14, 4, requested_hi);
    assert_eq!(cfg_read(&mut cfg, &mut bus, bdf, 0x10, 4), expected_lo);
    assert_eq!(cfg_read(&mut cfg, &mut bus, bdf, 0x14, 4), requested_hi);

    // Enable memory decoding and verify the BAR is mapped.
    cfg_write(&mut cfg, &mut bus, bdf, 0x04, 2, 0x0002);
    let mapped = bus.mapped_mmio_bars();
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].bdf, bdf);
    assert_eq!(mapped[0].bar, 0);
    assert_eq!(mapped[0].range.base, mmio64_base(expected_lo, requested_hi));
    assert_eq!(mapped[0].range.size, BAR0_SIZE);

    // Snapshot and restore.
    let bus_snapshot = PciBusSnapshot::save_from(&bus);
    let bus_bytes = bus_snapshot.save_state();
    let cfg_bytes = cfg.save_state();

    let (mut bus2, _) = make_bus();
    let mut cfg2 = PciConfigMechanism1::new();
    cfg2.load_state(&cfg_bytes).unwrap();

    let mut restored = PciBusSnapshot::default();
    restored.load_state(&bus_bytes).unwrap();
    restored.restore_into(&mut bus2).unwrap();

    // Verify BAR reads and mapping survived restore.
    assert_eq!(cfg_read(&mut cfg2, &mut bus2, bdf, 0x10, 4), expected_lo);
    assert_eq!(cfg_read(&mut cfg2, &mut bus2, bdf, 0x14, 4), requested_hi);
    assert_eq!(bus.mapped_bars(), bus2.mapped_bars());
}

#[test]
fn pci_snapshot_roundtrip_preserves_mmio64_bar_probe_state() {
    let (mut bus, bdf) = make_bus();
    let mut cfg = PciConfigMechanism1::new();

    // Leave the BAR in probed state (write all 1s but do not program a base yet).
    cfg_write(&mut cfg, &mut bus, bdf, 0x10, 4, 0xFFFF_FFFF);
    cfg_write(&mut cfg, &mut bus, bdf, 0x14, 4, 0xFFFF_FFFF);
    let (expected_probe_lo, expected_probe_hi) = mmio64_probe_masks(BAR0_SIZE);
    assert_eq!(
        cfg_read(&mut cfg, &mut bus, bdf, 0x10, 4),
        expected_probe_lo
    );
    assert_eq!(
        cfg_read(&mut cfg, &mut bus, bdf, 0x14, 4),
        expected_probe_hi
    );

    let bus_snapshot = PciBusSnapshot::save_from(&bus);
    let bus_bytes = bus_snapshot.save_state();
    let cfg_bytes = cfg.save_state();

    let (mut bus2, _) = make_bus();
    let mut cfg2 = PciConfigMechanism1::new();
    cfg2.load_state(&cfg_bytes).unwrap();

    let mut restored = PciBusSnapshot::default();
    restored.load_state(&bus_bytes).unwrap();
    restored.restore_into(&mut bus2).unwrap();

    // Probe state should survive restore.
    assert_eq!(
        cfg_read(&mut cfg2, &mut bus2, bdf, 0x10, 4),
        expected_probe_lo
    );
    assert_eq!(
        cfg_read(&mut cfg2, &mut bus2, bdf, 0x14, 4),
        expected_probe_hi
    );
    assert!(bus2.mapped_bars().is_empty());

    // And programming the BAR should clear probe state and behave normally after restore.
    let requested_lo = 0x2345_6000;
    let requested_hi = 0x0000_0001;
    let expected_lo = mmio64_programmed_lo(BAR0_SIZE, requested_lo);
    cfg_write(&mut cfg2, &mut bus2, bdf, 0x10, 4, requested_lo);
    cfg_write(&mut cfg2, &mut bus2, bdf, 0x14, 4, requested_hi);
    assert_eq!(cfg_read(&mut cfg2, &mut bus2, bdf, 0x10, 4), expected_lo);
    assert_eq!(cfg_read(&mut cfg2, &mut bus2, bdf, 0x14, 4), requested_hi);

    cfg_write(&mut cfg2, &mut bus2, bdf, 0x04, 2, 0x0002);
    let mapped = bus2.mapped_mmio_bars();
    assert_eq!(mapped.len(), 1);
    assert_eq!(mapped[0].range.base, mmio64_base(expected_lo, requested_hi));
}
