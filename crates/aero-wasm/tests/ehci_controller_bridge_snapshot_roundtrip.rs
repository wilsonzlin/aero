#![cfg(target_arch = "wasm32")]

use aero_wasm::EhciControllerBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

// EHCI operational register offsets (from the base returned by CAPLENGTH).
const OP_REG_USBCMD: u32 = 0x00;
const OP_REG_FRINDEX: u32 = 0x0C;

const USBCMD_RUN: u32 = 1 << 0;

#[wasm_bindgen_test]
fn ehci_controller_bridge_step_frames_advances_frindex_and_snapshot_roundtrips() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);

    let mut ctrl = EhciControllerBridge::new(guest_base, guest_size).unwrap();

    // CAPLENGTH is an 8-bit value at MMIO offset 0x00; it gives the base offset of the operational
    // register block.
    let caplen = (ctrl.mmio_read(0, 1) & 0xFF) as u32;

    // Bring the controller into the Running state so FRINDEX advances when ticking.
    ctrl.mmio_write(caplen + OP_REG_USBCMD, 4, USBCMD_RUN);

    // With PCI bus mastering disabled (default), the bridge must still advance timers/FRINDEX.
    let before = ctrl.mmio_read(caplen + OP_REG_FRINDEX, 4) & 0x3FFF;
    ctrl.step_frames(1);
    let after = ctrl.mmio_read(caplen + OP_REG_FRINDEX, 4) & 0x3FFF;
    assert_eq!(
        after,
        (before + 8) & 0x3FFF,
        "FRINDEX must advance by 8 per 1ms frame"
    );

    let snapshot = ctrl.save_state();

    let mut ctrl2 = EhciControllerBridge::new(guest_base, guest_size).unwrap();
    ctrl2.load_state(&snapshot).unwrap();

    let caplen2 = (ctrl2.mmio_read(0, 1) & 0xFF) as u32;
    assert_eq!(caplen2, caplen, "CAPLENGTH must be stable across restore");
    assert_eq!(ctrl2.mmio_read(caplen + OP_REG_FRINDEX, 4) & 0x3FFF, after);
}
