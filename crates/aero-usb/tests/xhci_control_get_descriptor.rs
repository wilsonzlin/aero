use std::boxed::Box;

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::xhci::transfer::{CompletionCode, Ep0TransferEngine};
use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::MemoryBus;

mod util;

use util::{Alloc, TestMemory};

const TRB_CYCLE: u32 = 1 << 0;
const TRB_IOC: u32 = 1 << 5;
const TRB_TRB_TYPE_SHIFT: u32 = Trb::CONTROL_TRB_TYPE_SHIFT;
const TRB_DIR_IN: u32 = 1 << 16;

// Setup Stage TRB TRT field (bits 16..=17).
const SETUP_TRT_IN: u32 = 3 << 16;

fn write_trb(mem: &mut TestMemory, addr: u32, trb: Trb) {
    trb.write_to(mem, addr as u64);
}

#[test]
fn xhci_control_get_descriptor_device() {
    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x1000);

    // Allocate and configure an event ring with space for 16 event TRBs.
    let event_ring_base = alloc.alloc(16 * 16, 16);

    // Allocate a transfer ring with 4 TRBs: Setup/Data/Status/Link.
    let tr_ring_base = alloc.alloc(4 * 16, 16);
    let setup_trb_addr = tr_ring_base;
    let data_trb_addr = tr_ring_base + 16;
    let status_trb_addr = tr_ring_base + 32;
    let link_trb_addr = tr_ring_base + 48;

    // Data buffer for GET_DESCRIPTOR(Device) response.
    let data_buf = alloc.alloc(18, 16);

    // Build the control transfer:
    // - SETUP: GET_DESCRIPTOR(Device), wLength=18
    // - DATA: IN, 18 bytes
    // - STATUS: OUT ZLP with IOC
    let setup_bytes = [0x80u8, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00];
    write_trb(
        &mut mem,
        setup_trb_addr,
        Trb {
            parameter: u64::from_le_bytes(setup_bytes),
            status: 8, // Transfer length for SETUP is always 8.
            control: TRB_CYCLE
                | ((TrbType::SetupStage.raw() as u32) << TRB_TRB_TYPE_SHIFT)
                | SETUP_TRT_IN,
        },
    );
    write_trb(
        &mut mem,
        data_trb_addr,
        Trb {
            parameter: data_buf as u64,
            status: 18, // Transfer length.
            control: TRB_CYCLE
                | ((TrbType::DataStage.raw() as u32) << TRB_TRB_TYPE_SHIFT)
                | TRB_DIR_IN,
        },
    );
    write_trb(
        &mut mem,
        status_trb_addr,
        Trb {
            parameter: 0,
            status: 0,
            control: TRB_CYCLE
                | ((TrbType::StatusStage.raw() as u32) << TRB_TRB_TYPE_SHIFT)
                | TRB_IOC,
        },
    );
    // Link TRB back to the start of the ring with toggle-cycle.
    write_trb(
        &mut mem,
        link_trb_addr,
        Trb {
            parameter: tr_ring_base as u64,
            status: 0,
            control: TRB_CYCLE
                | ((TrbType::Link.raw() as u32) << TRB_TRB_TYPE_SHIFT)
                | Trb::CONTROL_LINK_TOGGLE_CYCLE,
        },
    );

    let mut xhci = Ep0TransferEngine::new_with_ports(1);
    xhci.set_event_ring(event_ring_base as u64, 16);

    let keyboard = UsbHidKeyboardHandle::new();
    xhci.hub_mut().attach(0, Box::new(keyboard.clone()));

    let slot_id = xhci.enable_slot(0).expect("slot must be allocated");
    assert!(xhci.configure_ep0(slot_id, tr_ring_base as u64, true, 64));

    // Ring the endpoint-0 doorbell (endpoint ID 1).
    xhci.ring_doorbell(&mut mem, slot_id, 1);

    let mut got = [0u8; 18];
    mem.read_physical(data_buf as u64, &mut got);
    let expected = [
        0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x01, 0x00, 0x00, 0x01, 0x01,
        0x02, 0x00, 0x01,
    ];
    assert_eq!(got, expected);

    let evt = Trb::read_from(&mut mem, event_ring_base as u64);
    assert_eq!(evt.trb_type(), TrbType::TransferEvent);
    assert_eq!(evt.completion_code_raw(), CompletionCode::Success as u8);
    assert_eq!(evt.status & 0x00ff_ffff, 0);
    assert_eq!(evt.parameter, status_trb_addr as u64);
}
