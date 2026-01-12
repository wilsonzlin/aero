use aero_devices::pci::{
    GsiLevelSink, PciBarDefinition, PciBdf, PciBus, PciConfigPorts, PciConfigSpace,
    PciCoreSnapshot, PciDevice, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
    PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_io_snapshot::io::state::IoSnapshot;
use std::collections::BTreeSet;

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

struct TestDev {
    cfg: PciConfigSpace,
}

impl PciDevice for TestDev {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

fn make_cfg_ports() -> (PciConfigPorts, PciBdf) {
    let mut bus = PciBus::new();
    let bdf = PciBdf::new(0, 1, 0);

    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    cfg.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });

    bus.add_device(bdf, Box::new(TestDev { cfg }));
    (PciConfigPorts::with_bus(bus), bdf)
}

#[derive(Default)]
struct MockSink {
    events: Vec<(u32, bool)>,
}

impl GsiLevelSink for MockSink {
    fn set_gsi_level(&mut self, gsi: u32, level: bool) {
        self.events.push((gsi, level));
    }
}

#[test]
fn pci_core_snapshot_roundtrip_restores_cfg_and_intx_state() {
    let (mut ports, bdf) = make_cfg_ports();

    // Read vendor/device ID.
    let id = cfg_read(&mut ports, bdf, 0x00, 4);
    assert_eq!(id & 0xFFFF, 0x1234);
    assert_eq!(id >> 16, 0x5678);

    // BAR0 probe/program.
    cfg_write(&mut ports, bdf, 0x10, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut ports, bdf, 0x10, 4), 0xFFFF_F000);
    cfg_write(&mut ports, bdf, 0x10, 4, 0x1234_5000);
    assert_eq!(cfg_read(&mut ports, bdf, 0x10, 4), 0x1234_5000);

    // BAR1 IO probe/program.
    cfg_write(&mut ports, bdf, 0x14, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut ports, bdf, 0x14, 4), 0xFFFF_FFE1);
    cfg_write(&mut ports, bdf, 0x14, 4, 0x0000_C200);
    assert_eq!(cfg_read(&mut ports, bdf, 0x14, 4), 0x0000_C201);

    // Leave BAR0 in the probed state so probe flags are exercised, but keep the programmed base.
    cfg_write(&mut ports, bdf, 0x10, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut ports, bdf, 0x10, 4), 0xFFFF_F000);

    // Leave the config address latch at a recognizable value.
    ports.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, 0x3c));
    let expected_latch = ports.io_read(PCI_CFG_ADDR_PORT, 4);

    // INTx: assert multiple sources (including a shared GSI) so refcounts are non-trivial.
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut sink = MockSink::default();
    let dev0 = PciBdf::new(0, 0, 0);
    let dev4 = PciBdf::new(0, 4, 0); // Same PIRQ/GSI as dev0 for INTA#.
    let gsi_dev0 = router.gsi_for_intx(dev0, PciInterruptPin::IntA);
    let gsi_bdf = router.gsi_for_intx(bdf, PciInterruptPin::IntA);

    router.assert_intx(dev0, PciInterruptPin::IntA, &mut sink);
    router.assert_intx(dev4, PciInterruptPin::IntA, &mut sink);
    router.assert_intx(bdf, PciInterruptPin::IntA, &mut sink);
    let mut expected_events = vec![(gsi_dev0, true)];
    if gsi_bdf != gsi_dev0 {
        expected_events.push((gsi_bdf, true));
    }
    assert_eq!(sink.events, expected_events);

    // Snapshot deterministically.
    let bytes = {
        let core = PciCoreSnapshot::new(&mut ports, &mut router);
        let a = core.save_state();
        let b = core.save_state();
        assert_eq!(a, b, "snapshot bytes must be deterministic");
        a
    };

    // Restore into fresh devices with the same PCI topology/config.
    let (mut ports2, bdf2) = make_cfg_ports();
    assert_eq!(bdf2, bdf);
    let mut router2 = PciIntxRouter::new(PciIntxRouterConfig::default());

    {
        let mut core2 = PciCoreSnapshot::new(&mut ports2, &mut router2);
        core2.load_state(&bytes).unwrap();
    }

    // CF8 latch should roundtrip.
    assert_eq!(ports2.io_read(PCI_CFG_ADDR_PORT, 4), expected_latch);

    // Vendor/device should be readable and match.
    let id2 = cfg_read(&mut ports2, bdf, 0x00, 4);
    assert_eq!(id2, id);

    // BAR probe mask should be restored (BAR0 is left probed).
    assert_eq!(cfg_read(&mut ports2, bdf, 0x10, 4), 0xFFFF_F000);

    // BAR base should also be restored even when the BAR is in probe mode.
    let bar0_base = ports2
        .bus_mut()
        .device_config(bdf)
        .unwrap()
        .bar_range(0)
        .unwrap()
        .base;
    assert_eq!(bar0_base, 0x1234_5000);

    // BAR1 should read back the programmed IO address.
    assert_eq!(cfg_read(&mut ports2, bdf, 0x14, 4), 0x0000_C201);

    // INTx levels must be re-driven via `sync_levels_to_sink()` after restore.
    let mut sink2 = MockSink::default();
    router2.sync_levels_to_sink(&mut sink2);

    // `sync_levels_to_sink` iterates PIRQ[A-D] in order and skips duplicate GSIs.
    // Derive that mapping via gsi_for_intx without hard-coding the legacy 10-13 IRQ scheme.
    let pirq_gsis = [
        router2.gsi_for_intx(dev0, PciInterruptPin::IntA),
        router2.gsi_for_intx(dev0, PciInterruptPin::IntB),
        router2.gsi_for_intx(dev0, PciInterruptPin::IntC),
        router2.gsi_for_intx(dev0, PciInterruptPin::IntD),
    ];
    let mut seen = BTreeSet::new();
    let mut expected_sync = Vec::new();
    for gsi in pirq_gsis {
        if !seen.insert(gsi) {
            continue;
        }
        let asserted = gsi == gsi_dev0 || gsi == gsi_bdf;
        expected_sync.push((gsi, asserted));
    }
    assert_eq!(sink2.events, expected_sync);

    // Restored refcounts should keep the shared line asserted until all sources deassert.
    router2.deassert_intx(dev0, PciInterruptPin::IntA, &mut sink2);
    assert_eq!(sink2.events, expected_sync);

    router2.deassert_intx(dev4, PciInterruptPin::IntA, &mut sink2);
    router2.deassert_intx(bdf, PciInterruptPin::IntA, &mut sink2);
    let mut expected_after_deassert = expected_sync.clone();
    if gsi_bdf != gsi_dev0 {
        expected_after_deassert.push((gsi_dev0, false));
        expected_after_deassert.push((gsi_bdf, false));
    } else {
        expected_after_deassert.push((gsi_dev0, false));
    }
    assert_eq!(sink2.events, expected_after_deassert);
}
