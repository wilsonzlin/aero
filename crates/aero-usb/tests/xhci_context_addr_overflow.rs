use std::panic::{catch_unwind, AssertUnwindSafe};

use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::MemoryBus;

mod util;

use util::{xhci_set_run, TestMemory};

#[test]
fn xhci_configure_endpoint_dev_ctx_ptr_overflow_does_not_panic_or_write_low_memory() {
    let mut mem = TestMemory::new(0x10_000);
    mem.data[0..32].fill(0xAA);

    let dcbaa = 0x1000u64;
    let cmd_ring = 0x2000u64;
    let input_ctx = 0x3000u64; // 64-byte aligned.

    // Choose a device context base such that `base + (2 * CONTEXT_SIZE)` would overflow.
    // `CONTEXT_SIZE` is 32 bytes for 32-byte contexts, so endpoint_id=2 implies +64.
    let dev_ctx_ptr = !0x3fu64; // 0xffff...ffc0

    let mut xhci = XhciController::new();
    xhci.set_dcbaap(dcbaa);
    xhci_set_run(&mut xhci);
    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;

    // Install the overflowing device context pointer in DCBAA.
    MemoryBus::write_u64(&mut mem, dcbaa + u64::from(slot_id) * 8, dev_ctx_ptr);

    // Configure Endpoint command with Deconfigure=1.
    let mut cmd = Trb::new(input_ctx, 0, 0);
    cmd.set_cycle(true);
    cmd.set_trb_type(TrbType::ConfigureEndpointCommand);
    cmd.set_slot_id(slot_id);
    cmd.set_configure_endpoint_deconfigure(true);
    // Command ring contains a single TRB.
    cmd.write_to(&mut mem, cmd_ring);

    xhci.set_command_ring(cmd_ring, true);

    let res = catch_unwind(AssertUnwindSafe(|| {
        xhci.process_command_ring(&mut mem, 1);
    }));
    assert!(
        res.is_ok(),
        "controller must not panic on device context pointer address overflow"
    );

    let ev = xhci.pop_pending_event().expect("expected completion event");
    assert_eq!(ev.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(
        ev.completion_code_raw(),
        CompletionCode::ContextStateError.as_u8()
    );
    assert_eq!(ev.parameter, cmd_ring);

    // If address arithmetic wrapped, the deconfigure loop would have DMA-written endpoint contexts
    // into low memory (e.g. address 0). Ensure our sentinel pattern remains intact.
    assert_eq!(mem.data[0..32], [0xAA; 32]);
}
