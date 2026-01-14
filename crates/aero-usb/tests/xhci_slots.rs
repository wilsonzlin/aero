use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::MemoryBus;

mod util;

use util::TestMemory;

#[test]
fn enable_slot_fails_without_dcbaap() {
    let mut ctrl = XhciController::new();
    let mut mem = TestMemory::new(0x1000);

    let result = ctrl.enable_slot(&mut mem);
    assert_eq!(
        result.completion_code,
        CommandCompletionCode::ContextStateError
    );
    assert_eq!(result.slot_id, 0);
    assert!(ctrl.slot_state(1).is_none());
}

#[test]
fn enable_slot_allocates_slot_and_uses_aligned_dcbaap() {
    let mut ctrl = XhciController::new();
    let mut mem = TestMemory::new(0x1000);

    // DCBAAP is 64-byte aligned; the controller should mask low bits.
    let dcbaap_aligned: u64 = 0x200;
    let dcbaap_unaligned: u64 = dcbaap_aligned | 0x1f;

    // Place sentinel values at both the aligned and unaligned addresses. If the controller fails to
    // mask the register value, it will overwrite the unaligned location instead.
    mem.write_u64(dcbaap_aligned + 8, 0x1111_2222_3333_4444);
    mem.write_u64(dcbaap_unaligned + 8, 0x5555_6666_7777_8888);

    ctrl.set_dcbaap(dcbaap_unaligned);
    assert_eq!(ctrl.dcbaap(), Some(dcbaap_aligned));

    let result = ctrl.enable_slot(&mut mem);
    assert_eq!(result.completion_code, CommandCompletionCode::Success);
    assert_eq!(result.slot_id, 1);
    assert!(ctrl.slot_state(1).is_some());

    // DCBAAP[1] should be initialised to 0 by the controller.
    assert_eq!(mem.read_u64(dcbaap_aligned + 8), 0);
    // Unaligned location must remain unchanged.
    assert_eq!(mem.read_u64(dcbaap_unaligned + 8), 0x5555_6666_7777_8888);
}
