use std::boxed::Box;

use aero_usb::xhci::context::{InputControlContext, SlotContext, CONTEXT_SIZE};
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, CommandCompletionCode, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult};

mod util;

use util::{Alloc, TestMemory};

#[derive(Clone, Debug)]
struct InterruptInDevice;

impl UsbDeviceModel for InterruptInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        assert_eq!(ep_addr, 0x81);
        UsbInResult::Data(vec![0x5a; max_len.min(8)])
    }
}

fn configure_dcbaa(mem: &mut TestMemory, dcbaa: u64, slot_id: u8, dev_ctx_ptr: u64) {
    MemoryBus::write_u64(mem, dcbaa + u64::from(slot_id) * 8, dev_ctx_ptr);
}

fn endpoint_ctx_addr(dev_ctx_base: u64, endpoint_id: u8) -> u64 {
    dev_ctx_base + u64::from(endpoint_id) * (CONTEXT_SIZE as u64)
}

fn write_interrupt_in_endpoint_context(
    mem: &mut TestMemory,
    dev_ctx_base: u64,
    endpoint_id: u8,
    ring_base: u64,
) {
    let base = endpoint_ctx_addr(dev_ctx_base, endpoint_id);
    // Endpoint state: Running (1).
    MemoryBus::write_u32(mem, base, 1);
    // Endpoint type (Interrupt IN = 7) + max packet size.
    let dw1 = (7u32 << 3) | (8u32 << 16);
    MemoryBus::write_u32(mem, base + 4, dw1);

    let tr_dequeue_raw = (ring_base & !0x0f) | 1;
    MemoryBus::write_u32(mem, base + 8, tr_dequeue_raw as u32);
    MemoryBus::write_u32(mem, base + 12, (tr_dequeue_raw >> 32) as u32);
}

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, ioc: bool) -> Trb {
    let mut trb = Trb::new(buf_ptr, len & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb.set_trb_type(TrbType::Normal);
    trb.set_cycle(cycle);
    if ioc {
        trb.control |= Trb::CONTROL_IOC_BIT;
    }
    trb
}

fn run_drop_or_deconfigure(
    ctrl: &mut XhciController,
    mem: &mut TestMemory,
    slot_id: u8,
    cmd_ring: u64,
    input_ctx: u64,
    configure: impl FnOnce(&mut Trb),
) {
    ctrl.set_command_ring(cmd_ring, true);

    let mut cmd = Trb::new(input_ctx, 0, 0);
    cmd.set_trb_type(TrbType::ConfigureEndpointCommand);
    cmd.set_cycle(true);
    cmd.set_slot_id(slot_id);
    configure(&mut cmd);
    cmd.write_to(mem, cmd_ring);

    ctrl.process_command_ring(mem, 1);
    let ev = ctrl
        .pop_pending_event()
        .expect("Configure Endpoint completion");
    assert_eq!(ev.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev.completion_code_raw(), CompletionCode::Success.as_u8());
    assert_eq!(ev.slot_id(), slot_id);
}

#[test]
fn xhci_configure_endpoint_drop_clears_pending_doorbells() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x40) as u64;
    let input_ctx = alloc.alloc(0x200, 0x40) as u64;
    let ring_base = alloc.alloc((TRB_LEN as u32) * 2, 0x10) as u64;
    let buf_ptr = alloc.alloc(8, 0x10) as u64;

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;

    write_interrupt_in_endpoint_context(&mut mem, dev_ctx, EP_ID, ring_base);
    make_normal_trb(buf_ptr, 8, true, true).write_to(&mut mem, ring_base);

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(InterruptInDevice));
    while ctrl.pop_pending_event().is_some() {}
    // Transfers only execute while the controller is running (USBCMD.RUN=1).
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Transfers only execute while the controller is running.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    ctrl.set_dcbaap(dcbaa);
    // Transfer execution only runs while USBCMD.RUN is set.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    let enable = ctrl.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    configure_dcbaa(&mut mem, dcbaa, slot_id, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Transfers only execute while the controller is running.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Queue an endpoint doorbell but do not tick: the coalescing bitmap should mark it pending.
    ctrl.ring_doorbell(slot_id, EP_ID);

    // Drop the endpoint context via Configure Endpoint (drop flag). This should remove the endpoint
    // from the active queue *and* clear its pending bit so future doorbells can be serviced.
    let mut icc = InputControlContext::default();
    icc.set_drop_flags(1u32 << EP_ID);
    icc.set_add_flags(0);
    icc.write_to(&mut mem, input_ctx);

    run_drop_or_deconfigure(&mut ctrl, &mut mem, slot_id, cmd_ring, input_ctx, |_| {});

    // Re-populate the endpoint context (guest would re-run Configure Endpoint) and ring the doorbell
    // again. If the pending bit was not cleared, this doorbell would be ignored and no DMA would
    // occur.
    write_interrupt_in_endpoint_context(&mut mem, dev_ctx, EP_ID, ring_base);

    ctrl.ring_doorbell(slot_id, EP_ID);
    ctrl.tick(&mut mem);

    let mut buf = [0u8; 8];
    mem.read_physical(buf_ptr, &mut buf);
    assert_eq!(buf, [0x5a; 8]);
    assert_eq!(ctrl.pending_event_count(), 1);
}

#[test]
fn xhci_configure_endpoint_deconfigure_clears_pending_doorbells() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    let cmd_ring = alloc.alloc(0x100, 0x40) as u64;
    let input_ctx = alloc.alloc(0x200, 0x40) as u64;
    let ring_base = alloc.alloc((TRB_LEN as u32) * 2, 0x10) as u64;
    let buf_ptr = alloc.alloc(8, 0x10) as u64;

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;

    write_interrupt_in_endpoint_context(&mut mem, dev_ctx, EP_ID, ring_base);
    make_normal_trb(buf_ptr, 8, true, true).write_to(&mut mem, ring_base);

    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(InterruptInDevice));
    while ctrl.pop_pending_event().is_some() {}
    // Transfers only execute while the controller is running (USBCMD.RUN=1).
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Transfers only execute while the controller is running.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    ctrl.set_dcbaap(dcbaa);
    // Transfer execution only runs while USBCMD.RUN is set.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    let enable = ctrl.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    configure_dcbaa(&mut mem, dcbaa, slot_id, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = ctrl.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Transfers only execute while the controller is running.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Ensure the output Slot Context in guest memory is populated. Deconfigure mode reads the Slot
    // Context from the output Device Context and mirrors it back into controller-local state; if we
    // leave it zeroed, the slot would no longer resolve to an attached device after deconfigure.
    let mut out_slot_ctx = SlotContext::default();
    out_slot_ctx.set_root_hub_port_number(1);
    out_slot_ctx.write_to(&mut mem, dev_ctx);

    // Queue an endpoint doorbell but do not tick.
    ctrl.ring_doorbell(slot_id, EP_ID);

    // Deconfigure mode disables all non-EP0 endpoints. Like the drop path, it must clear the
    // pending-doorbell coalescing bitmap for those endpoints so they can be re-doorbelled later.
    InputControlContext::default().write_to(&mut mem, input_ctx);
    run_drop_or_deconfigure(&mut ctrl, &mut mem, slot_id, cmd_ring, input_ctx, |cmd| {
        cmd.set_configure_endpoint_deconfigure(true)
    });

    // Re-populate the endpoint context and ring the doorbell again.
    write_interrupt_in_endpoint_context(&mut mem, dev_ctx, EP_ID, ring_base);

    ctrl.ring_doorbell(slot_id, EP_ID);
    ctrl.tick(&mut mem);

    let mut buf = [0u8; 8];
    mem.read_physical(buf_ptr, &mut buf);
    assert_eq!(buf, [0x5a; 8]);
    assert_eq!(ctrl.pending_event_count(), 1);
}
