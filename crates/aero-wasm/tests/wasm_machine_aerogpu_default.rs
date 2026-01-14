#![cfg(target_arch = "wasm32")]

use aero_wasm::Machine;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn wasm_machine_enables_aerogpu_by_default() {
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

    // Canonical AeroGPU PCI identity contract: `00:07.0` must be `A3A0:0001`.
    let id = machine.pci_config_read_u32(0, 0x07, 0, 0);
    assert_eq!(id & 0xFFFF, 0xA3A0, "unexpected AeroGPU vendor ID");
    assert_eq!((id >> 16) & 0xFFFF, 0x0001, "unexpected AeroGPU device ID");
}
