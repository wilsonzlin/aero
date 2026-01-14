#![cfg(target_arch = "wasm32")]

use aero_wasm::Machine;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn wasm_machine_display_present_exposes_framebuffer_cache() {
    let mut machine = Machine::new(16 * 1024 * 1024).expect("Machine::new");
    machine.reset();

    machine.display_present();

    let width = machine.display_width();
    let height = machine.display_height();
    assert!(width > 0, "expected non-zero display_width");
    assert!(height > 0, "expected non-zero display_height");
    assert_eq!(machine.display_stride_bytes(), width * 4);

    let len = machine.display_framebuffer_len_bytes();
    let expected_len = u64::from(width) * u64::from(height) * 4;
    assert!(
        expected_len <= u64::from(u32::MAX),
        "framebuffer too large for u32"
    );
    assert_eq!(len, expected_len as u32);

    let ptr = machine.display_framebuffer_ptr();
    assert!(ptr != 0, "expected non-zero display_framebuffer_ptr");

    // Safety: ptr/len is a view into the module's own linear memory.
    let fb = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };

    let copied = machine.display_framebuffer_copy_rgba8888();
    assert_eq!(copied.len(), len as usize);
    assert_eq!(copied.as_slice(), fb);
}

