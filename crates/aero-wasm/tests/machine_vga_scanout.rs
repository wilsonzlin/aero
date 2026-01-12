#[test]
fn machine_vga_scanout_exports_non_empty_rgba8888_framebuffer() {
    // Keep the RAM size small-ish for a fast smoke test while still being large enough for the
    // canonical PC machine configuration.
    let mut m = aero_wasm::Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

    // Ensure the front buffer is up to date (no-op if nothing is dirty).
    m.vga_present();

    let width = m.vga_width();
    let height = m.vga_height();
    assert!(width > 0, "vga_width must be non-zero when VGA is present");
    assert!(height > 0, "vga_height must be non-zero when VGA is present");

    assert_eq!(
        m.vga_stride_bytes(),
        width.saturating_mul(4),
        "stride must be width * 4 for RGBA8888"
    );

    let len_bytes = m.vga_framebuffer_len_bytes();
    let expected_len_bytes = (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(4);
    assert!(
        expected_len_bytes <= u64::from(u32::MAX),
        "framebuffer byte length should fit in u32 for test mode"
    );
    assert_eq!(
        len_bytes as u64, expected_len_bytes,
        "len_bytes must equal width * height * 4"
    );

    let copy = m.vga_framebuffer_copy_rgba8888();
    assert!(!copy.is_empty(), "copied framebuffer should be non-empty");
    assert_eq!(copy.len() as u32, len_bytes, "copy length should match len_bytes");

    // The raw pointer view is only meaningful for wasm32 builds.
    if cfg!(target_arch = "wasm32") {
        assert_ne!(m.vga_framebuffer_ptr(), 0);
    } else {
        assert_eq!(m.vga_framebuffer_ptr(), 0);
    }
}

