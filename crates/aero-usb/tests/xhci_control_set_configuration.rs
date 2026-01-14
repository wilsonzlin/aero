use std::boxed::Box;

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::xhci::transfer::Ep0TransferEngine;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType};

mod util;

use util::{Alloc, TestMemory};

const TRB_CYCLE: u32 = 1 << 0;
const TRB_IOC: u32 = 1 << 5;
const TRB_TRB_TYPE_SHIFT: u32 = Trb::CONTROL_TRB_TYPE_SHIFT;
const TRB_DIR_IN: u32 = 1 << 16;

#[test]
fn xhci_control_set_configuration_configures_keyboard() {
    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate and configure an event ring with space for 8 event TRBs.
    let event_ring_base = alloc.alloc(8 * 16, 16);

    // Allocate a transfer ring with 3 TRBs: Setup/Status/Link.
    let tr_ring_base = alloc.alloc(3 * 16, 16);
    let setup_trb_addr = tr_ring_base;
    let status_trb_addr = tr_ring_base + 16;
    let link_trb_addr = tr_ring_base + 32;

    // SET_CONFIGURATION(1) has no DATA stage. Status stage is an IN ZLP.
    let setup_bytes = [0x00u8, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
    Trb::new(u64::from_le_bytes(setup_bytes), 8, 0).write_to(&mut mem, setup_trb_addr as u64);

    // Patch in control fields.
    let mut setup_trb = Trb::read_from(&mut mem, setup_trb_addr as u64);
    setup_trb.control = TRB_CYCLE | ((u32::from(TrbType::SetupStage.raw())) << TRB_TRB_TYPE_SHIFT);
    setup_trb.write_to(&mut mem, setup_trb_addr as u64);

    let mut status_trb = Trb::new(0, 0, 0);
    status_trb.set_cycle(true);
    status_trb.set_trb_type(TrbType::StatusStage);
    // Status stage direction: IN for control-out requests.
    status_trb.control |= TRB_DIR_IN | TRB_IOC;
    status_trb.write_to(&mut mem, status_trb_addr as u64);

    // Link TRB back to the start of the ring with toggle-cycle.
    let mut link = Trb::new(tr_ring_base as u64, 0, 0);
    link.set_cycle(true);
    link.set_trb_type(TrbType::Link);
    link.set_link_toggle_cycle(true);
    link.write_to(&mut mem, link_trb_addr as u64);

    let keyboard = UsbHidKeyboardHandle::new();
    assert!(!keyboard.configured(), "device should start unconfigured");

    let mut xhci = Ep0TransferEngine::new_with_ports(1);
    xhci.set_event_ring(event_ring_base as u64, 8);
    xhci.hub_mut().attach(0, Box::new(keyboard.clone()));

    let slot_id = xhci.enable_slot(0).expect("slot must be allocated");
    assert!(xhci.configure_ep0(slot_id, tr_ring_base as u64, true, 64));

    // Ring the endpoint-0 doorbell (endpoint ID 1).
    xhci.ring_doorbell(&mut mem, slot_id, 1);

    assert!(
        keyboard.configured(),
        "control transfer should apply configuration"
    );

    let evt = Trb::read_from(&mut mem, event_ring_base as u64);
    assert_eq!(evt.trb_type(), TrbType::TransferEvent);
    assert_eq!(evt.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt.status & 0x00ff_ffff, 0);
    assert_eq!(evt.parameter, status_trb_addr as u64);
}
