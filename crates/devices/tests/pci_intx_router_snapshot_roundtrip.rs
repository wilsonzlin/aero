use aero_devices::pci::{
    GsiLevelSink, PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};
use aero_io_snapshot::io::state::IoSnapshot;
use std::collections::BTreeSet;

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
    let gsi_dev0 = router.gsi_for_intx(dev0, PciInterruptPin::IntA);

    router.assert_intx(dev0, PciInterruptPin::IntA, &mut sink);
    assert_eq!(sink.events, vec![(gsi_dev0, true)]);

    // Save deterministically.
    let bytes = router.save_state();
    assert_eq!(bytes, router.save_state());

    let mut router2 = PciIntxRouter::new(PciIntxRouterConfig::default());
    router2.load_state(&bytes).unwrap();

    // Re-asserting an already-asserted source should be a no-op after restore.
    let mut sink2 = MockSink::default();
    router2.sync_levels_to_sink(&mut sink2);

    // Derive the router's configured PIRQ->GSI mapping in the same order `sync_levels_to_sink`
    // uses (PIRQ[A-D]), but avoid hard-coding the legacy 10-13 mapping.
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
        expected_sync.push((gsi, gsi == gsi_dev0));
    }
    assert_eq!(
        sink2.events,
        expected_sync
    );

    router2.assert_intx(dev0, PciInterruptPin::IntA, &mut sink2);
    assert_eq!(
        sink2.events,
        expected_sync
    );

    // Asserting another source that shares the same GSI should *not* toggle the line because the
    // restored assert count is already non-zero.
    router2.assert_intx(dev4, PciInterruptPin::IntA, &mut sink2);
    assert_eq!(
        sink2.events,
        expected_sync
    );
}
