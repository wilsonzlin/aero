#![cfg(target_arch = "wasm32")]

use aero_wasm::Machine;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn wasm_machine_display_present_exposes_framebuffer_cache() {
    let mut machine = Machine::new(16 * 1024 * 1024).expect("Machine::new");

    // Threaded/shared-memory builds must expose embedded scanout + cursor state headers so the JS
    // runtime can coordinate presentation across workers.
    #[cfg(feature = "wasm-threaded")]
    {
        // Keep constants in sync with:
        // - `crates/aero-shared/src/scanout_state.rs`
        // - `crates/aero-shared/src/cursor_state.rs`
        const SCANOUT_STATE_BYTE_LEN: u32 = 8 * 4;
        const CURSOR_STATE_BYTE_LEN: u32 = 12 * 4;

        assert_ne!(
            machine.scanout_state_ptr(),
            0,
            "scanout_state_ptr must be non-zero in wasm-threaded builds"
        );
        assert_eq!(
            machine.scanout_state_len_bytes(),
            SCANOUT_STATE_BYTE_LEN,
            "unexpected scanout_state_len_bytes in wasm-threaded build"
        );

        assert_ne!(
            machine.cursor_state_ptr(),
            0,
            "cursor_state_ptr must be non-zero in wasm-threaded builds"
        );
        assert_eq!(
            machine.cursor_state_len_bytes(),
            CURSOR_STATE_BYTE_LEN,
            "unexpected cursor_state_len_bytes in wasm-threaded build"
        );
    }

    machine.reset();

    machine.display_present();

    // `display_width`/`display_height`/`display_framebuffer_len_bytes` reflect the last
    // `display_present` call.
    let width1 = machine.display_width();
    let height1 = machine.display_height();
    assert!(width1 > 0, "expected non-zero display_width");
    assert!(height1 > 0, "expected non-zero display_height");
    assert_eq!(machine.display_stride_bytes(), width1 * 4);

    let len1 = machine.display_framebuffer_len_bytes();
    let expected_len1 = u64::from(width1) * u64::from(height1) * 4;
    assert!(
        expected_len1 <= u64::from(u32::MAX),
        "framebuffer too large for u32"
    );
    assert_eq!(len1, expected_len1 as u32);

    // `display_framebuffer_copy_rgba8888` calls `display_present` internally, so it can reallocate
    // the underlying framebuffer cache. Re-query the ptr/len after calling it.
    let copied = machine.display_framebuffer_copy_rgba8888();
    let width = machine.display_width();
    let height = machine.display_height();
    let len = machine.display_framebuffer_len_bytes();
    assert_eq!(copied.len(), len as usize);

    let expected_len = u64::from(width) * u64::from(height) * 4;
    assert!(
        expected_len <= u64::from(u32::MAX),
        "framebuffer too large for u32"
    );
    assert_eq!(len, expected_len as u32);

    let ptr = machine.display_framebuffer_ptr();
    assert!(ptr != 0, "expected non-zero display_framebuffer_ptr");

    let mut fb = vec![0u8; len as usize];
    // Safety: ptr/len is a view into the module's own linear memory.
    unsafe {
        core::ptr::copy_nonoverlapping(
            core::ptr::with_exposed_provenance(ptr as usize),
            fb.as_mut_ptr(),
            fb.len(),
        );
    }
    assert_eq!(copied, fb);
}
