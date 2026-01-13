mod util;

use aero_usb::xhci::command_ring::{CommandRing, CommandRingProcessor, EventRing};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType};
use aero_usb::{ControlResponse, UsbDeviceModel};

use util::{Alloc, TestMemory};

struct AckDevice;

impl UsbDeviceModel for AckDevice {
    fn handle_control_request(
        &mut self,
        _setup: aero_usb::SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }
}

#[test]
fn enable_slot_then_address_device_emits_completion_event_and_sets_address() {
    let mut mem = TestMemory::new(0x20000);
    let mut alloc = Alloc::new(0x1000);

    // Device Context Base Address Array (DCBAA) is 64-byte aligned.
    let dcbaa = alloc.alloc(0x100, 0x40);

    // Command ring: [Enable Slot][Address Device][Link->base, TC=1]
    let cmd_ring = alloc.alloc(0x40, 0x40);

    // Input contexts are 64-byte aligned.
    let input_ctx = alloc.alloc(0x40, 0x40);
    // Slot context is the second context entry in an input context (after the Input Control
    // Context). Root hub port number is encoded in Slot Context dword1 bits 7:0 (per our context
    // helpers).
    mem.write_u32(input_ctx + 0x20 + 4, 1);

    let event_ring = alloc.alloc(16 * 8, 0x10);

    let mut processor = CommandRingProcessor::new(
        mem.data.len() as u64,
        8,
        dcbaa as u64,
        CommandRing::from_crcr((cmd_ring as u64) | 1),
        EventRing::new(event_ring as u64, 8),
    );
    processor.attach_root_port(1, Box::new(AckDevice));

    // Enable Slot command TRB.
    {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::EnableSlotCommand);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring as u64);
    }

    // Address Device command TRB for slot 1.
    let input_ctx_u64 = input_ctx as u64;
    {
        let mut trb = Trb::new(input_ctx_u64, 0, 0);
        trb.set_trb_type(TrbType::AddressDeviceCommand);
        trb.set_cycle(true);
        trb.set_slot_id(1);
        trb.write_to(&mut mem, (cmd_ring + 16) as u64);
    }

    // Link TRB back to ring base (toggle cycle when wrapping).
    let cmd_ring_u64 = cmd_ring as u64;
    {
        let mut trb = Trb::new(cmd_ring_u64, 0, 0);
        trb.set_trb_type(TrbType::Link);
        trb.set_cycle(true);
        trb.set_link_toggle_cycle(true);
        trb.write_to(&mut mem, (cmd_ring + 32) as u64);
    }

    // Process commands (equivalent to ringing doorbell 0 in a full controller model).
    processor.process(&mut mem, 256);

    // Command Completion Event for Enable Slot.
    let evt0 = Trb::read_from(&mut mem, event_ring as u64);
    assert_eq!(evt0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt0.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt0.slot_id(), 1);
    assert_eq!(evt0.parameter & !0x0Fu64, cmd_ring as u64);

    // Command Completion Event for Address Device.
    let evt1 = Trb::read_from(&mut mem, (event_ring + 16) as u64);
    assert_eq!(evt1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt1.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt1.slot_id(), 1);
    assert_eq!(evt1.parameter & !0x0Fu64, (cmd_ring + 16) as u64);

    // The attached device should now have a non-zero address.
    let dev = processor
        .port_device(1)
        .expect("device should still be attached");
    assert_ne!(dev.address(), 0);
}
