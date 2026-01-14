#![cfg(target_arch = "wasm32")]

use aero_wasm::Machine;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn wasm_machine_enables_aerogpu_by_default() {
    let mut machine = Machine::new(16 * 1024 * 1024).expect("Machine::new");

    // Canonical AeroGPU PCI identity contract: `00:07.0` must be `A3A0:0001`.
    let id = machine.pci_config_read_u32(0, 0x07, 0, 0);
    assert_eq!(id & 0xFFFF, 0xA3A0, "unexpected AeroGPU vendor ID");
    assert_eq!((id >> 16) & 0xFFFF, 0x0001, "unexpected AeroGPU device ID");
}
