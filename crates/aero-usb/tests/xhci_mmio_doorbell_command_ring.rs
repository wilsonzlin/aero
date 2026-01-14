mod util;

use aero_usb::xhci::context::{EndpointContext, InputControlContext, SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

use util::{Alloc, TestMemory};

struct AckDevice;

impl UsbDeviceModel for AckDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }
}

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[test]
fn doorbell0_processes_command_ring_and_emits_completion_events() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    // Device Context Base Address Array (DCBAA) is 64-byte aligned.
    let dcbaa = alloc.alloc(0x200, 0x40);
    // Command ring base (CRCR bits 63:6), also 64-byte aligned.
    let cmd_ring = alloc.alloc(0x100, 0x40);
    // Input Context + Device Context pointers.
    let input_ctx = alloc.alloc(0x200, 0x40);
    let dev_ctx = alloc.alloc(0x400, 0x40);

    // Guest event ring (single segment).
    let erstba = alloc.alloc(0x20, 0x40);
    let event_ring = alloc.alloc(16 * (TRB_LEN as u32), 0x10);
    write_erst_entry(&mut mem, erstba as u64, event_ring as u64, 16);

    let mut xhci = XhciController::new();
    xhci.attach_device(0, Box::new(AckDevice));
    // Drain the Port Status Change Event emitted by `attach_device` so the test only observes
    // command completion events.
    while xhci.pop_pending_event().is_some() {}

    // Configure event ring on interrupter 0 so command completion events are written to guest RAM.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, 0);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, event_ring);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_HI, 4, 0);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    // Program DCBAAP and CRCR.
    xhci.mmio_write(&mut mem, regs::REG_DCBAAP_LO, 4, dcbaa);
    xhci.mmio_write(&mut mem, regs::REG_DCBAAP_HI, 4, 0);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, cmd_ring | 1);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, 0);
    // Command processing is only active while USBCMD.RUN is set.
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    // Enable Slot command TRB.
    {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::EnableSlotCommand);
        trb.set_cycle(true);
        trb.write_to(&mut mem, cmd_ring as u64);
    }
    // Stop marker: cycle mismatch => ring appears empty after TRB0.
    {
        let mut trb = Trb::new(0, 0, 0);
        trb.set_trb_type(TrbType::NoOpCommand);
        trb.set_cycle(false);
        trb.write_to(&mut mem, (cmd_ring as u64) + TRB_LEN as u64);
    }

    // Ring doorbell 0 (Command Ring).
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);
    xhci.service_event_ring(&mut mem);

    let evt0 = Trb::read_from(&mut mem, event_ring as u64);
    assert_eq!(evt0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt0.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt0.parameter & !0x0f, cmd_ring as u64);

    let slot_id = evt0.slot_id();
    assert_ne!(slot_id, 0);

    // Install device context pointer in DCBAA entry for the newly enabled slot (as the guest driver
    // would do between Enable Slot and Address Device).
    let dcbaa_entry = (dcbaa as u64) + (slot_id as u64) * 8;
    MemoryBus::write_u64(&mut mem, dcbaa_entry, dev_ctx as u64);

    // Build Input Context: ICC + Slot + EP0.
    let mut icc = InputControlContext::default();
    icc.set_add_flags(0b11); // slot + EP0
    icc.write_to(&mut mem, input_ctx as u64);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_route_string(0);
    slot_ctx.set_speed(regs::PSIV_FULL_SPEED);
    slot_ctx.set_context_entries(1);
    slot_ctx.set_root_hub_port_number(1);
    slot_ctx.write_to(&mut mem, input_ctx as u64 + CONTEXT_SIZE as u64);

    let mut ep0_ctx = EndpointContext::default();
    ep0_ctx.set_dword(1, 64u32 << 16); // Max Packet Size
    ep0_ctx.set_tr_dequeue_pointer((cmd_ring as u64) + 0x80, true);
    ep0_ctx.write_to(&mut mem, input_ctx as u64 + (2 * CONTEXT_SIZE) as u64);

    // Address Device command TRB for the enabled slot.
    {
        let mut trb = Trb::new(input_ctx as u64, 0, 0);
        trb.set_trb_type(TrbType::AddressDeviceCommand);
        trb.set_cycle(true);
        trb.set_slot_id(slot_id);
        trb.write_to(&mut mem, (cmd_ring as u64) + TRB_LEN as u64);
    }

    // Ring doorbell 0 again to process Address Device.
    xhci.mmio_write(&mut mem, u64::from(regs::DBOFF_VALUE), 4, 0);
    xhci.service_event_ring(&mut mem);

    let evt1 = Trb::read_from(&mut mem, (event_ring as u64) + TRB_LEN as u64);
    assert_eq!(evt1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(evt1.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(evt1.slot_id(), slot_id);
    assert_eq!(evt1.parameter & !0x0f, (cmd_ring as u64) + TRB_LEN as u64);

    assert_eq!(xhci.slot_state(slot_id).and_then(|s| s.port_id()), Some(1));

    let out_slot = SlotContext::read_from(&mut mem, dev_ctx as u64);
    assert_eq!(out_slot.root_hub_port_number(), 1);
    assert_eq!(out_slot.speed(), regs::PSIV_FULL_SPEED);
    assert_eq!(out_slot.route_string(), 0);

    let out_ep0 = EndpointContext::read_from(&mut mem, (dev_ctx as u64) + CONTEXT_SIZE as u64);
    assert_eq!(out_ep0.max_packet_size(), 64);
    assert_eq!(out_ep0.tr_dequeue_pointer(), (cmd_ring as u64) + 0x80);
}
