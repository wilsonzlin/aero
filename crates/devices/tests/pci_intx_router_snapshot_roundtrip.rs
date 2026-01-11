use aero_devices::pci::{
    GsiLevelSink, PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};
use aero_io_snapshot::io::state::IoSnapshot;

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
fn intx_router_snapshot_roundtrip_preserves_assert_counts() {
    let mut router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut sink = MockSink::default();

    let dev0 = PciBdf::new(0, 0, 0);
    let dev4 = PciBdf::new(0, 4, 0); // Same PIRQ swizzle as dev0.

    router.assert_intx(dev0, PciInterruptPin::IntA, &mut sink);
    assert_eq!(sink.events, vec![(10, true)]);

    // Save deterministically.
    let bytes = router.save_state();
    assert_eq!(bytes, router.save_state());

    let mut router2 = PciIntxRouter::new(PciIntxRouterConfig::default());
    router2.load_state(&bytes).unwrap();

    // Re-asserting an already-asserted source should be a no-op after restore.
    let mut sink2 = MockSink::default();
    router2.sync_levels_to_sink(&mut sink2);
    assert_eq!(sink2.events, vec![(10, true), (11, false), (12, false), (13, false)]);

    router2.assert_intx(dev0, PciInterruptPin::IntA, &mut sink2);
    assert_eq!(sink2.events, vec![(10, true), (11, false), (12, false), (13, false)]);

    // Asserting another source that shares the same GSI should *not* toggle the line because the
    // restored assert count is already non-zero.
    router2.assert_intx(dev4, PciInterruptPin::IntA, &mut sink2);
    assert_eq!(sink2.events, vec![(10, true), (11, false), (12, false), (13, false)]);
}
