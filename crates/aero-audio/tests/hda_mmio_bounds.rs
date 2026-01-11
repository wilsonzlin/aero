use aero_audio::hda::{HdaController, HDA_MMIO_SIZE};

#[test]
fn hda_mmio_high_offsets_are_ignored_without_panicking() {
    assert_eq!(HDA_MMIO_SIZE, 0x4000);

    let mut hda = HdaController::new();
    let bar_end = HDA_MMIO_SIZE as u64;

    // Touch the end of the BAR with a few access sizes. These offsets are within the declared BAR
    // size, but outside the subset of registers currently implemented by this model.
    for (offset, size) in [
        (bar_end - 1, 1),
        (bar_end - 2, 2),
        (bar_end - 4, 4),
        (bar_end - 8, 8),
    ] {
        assert_eq!(hda.mmio_read(offset, size), 0);
        hda.mmio_write(offset, size, 0xdead_beef);
    }

    // A write to an unimplemented register must not perturb real state (GCTL is 0x08).
    assert_eq!(hda.mmio_read(0x08, 4), 0);
}

