use aero_usb::ehci::EhciController;

#[test]
fn ehci_mmio_read_does_not_wrap_on_offset_overflow() {
    let ctrl = EhciController::new();

    // Regression test: `mmio_read` used `wrapping_add` when iterating bytes, so an overflowing
    // offset would wrap back into low MMIO addresses and alias real registers.
    let v = ctrl.mmio_read(u64::MAX - 1, 4);
    assert_eq!(
        v, 0xffff_ffff,
        "expected out-of-range MMIO reads to return open bus instead of wrapping"
    );
}

