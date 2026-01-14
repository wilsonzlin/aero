use aero_usb::uhci::UhciController;

#[test]
fn uhci_io_read_does_not_wrap_on_offset_overflow() {
    let ctrl = UhciController::new();

    // Regression test: `io_read` used `wrapping_add` when iterating bytes, so an overflowing offset
    // would wrap back into low I/O offsets and alias real registers.
    let v = ctrl.io_read(u16::MAX - 1, 4);
    assert_eq!(
        v, 0xffff_ffff,
        "expected out-of-range I/O reads to return open bus instead of wrapping"
    );
}

