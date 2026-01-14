use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::passthrough::{
    UsbHostAction, UsbHostCompletion, UsbHostCompletionIn, UsbHostCompletionOut,
};
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::regs;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{
    ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult,
    UsbWebUsbPassthroughDevice,
};

mod util;

use util::{Alloc, TestMemory};

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, ioc: bool) -> Trb {
    let mut trb = Trb::new(buf_ptr, len & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb.set_trb_type(TrbType::Normal);
    trb.set_cycle(cycle);
    if ioc {
        trb.control |= Trb::CONTROL_IOC_BIT;
    }
    trb
}

fn make_chained_normal_trb(buf_ptr: u64, len: u32, cycle: bool) -> Trb {
    let mut trb = make_normal_trb(buf_ptr, len, cycle, false);
    trb.control |= Trb::CONTROL_CHAIN_BIT;
    trb
}

fn make_event_data_trb(event_data: u64, cycle: bool, ioc: bool) -> Trb {
    let mut trb = Trb::new(event_data, 0, 0);
    trb.set_trb_type(TrbType::EventData);
    trb.set_cycle(cycle);
    if ioc {
        trb.control |= Trb::CONTROL_IOC_BIT;
    }
    trb
}

fn read_u64(mem: &TestMemory, addr: u64) -> u64 {
    let lo = mem.read_u32(addr as u32) as u64;
    let hi = mem.read_u32((addr + 4) as u32) as u64;
    (hi << 32) | lo
}

fn endpoint_ctx_addr(dev_ctx_base: u64, endpoint_id: u8) -> u64 {
    dev_ctx_base + (endpoint_id as u64) * 0x20
}

fn write_endpoint_context(
    mem: &mut TestMemory,
    dev_ctx_base: u64,
    endpoint_id: u8,
    ep_type_raw: u8,
    max_packet_size: u16,
    ring_base: u64,
    dcs: bool,
) {
    let base = endpoint_ctx_addr(dev_ctx_base, endpoint_id);
    // Endpoint state: running (1).
    mem.write_u32(base as u32, 1);
    // Endpoint type + max packet size.
    let dw1 = ((ep_type_raw as u32) << 3) | (u32::from(max_packet_size) << 16);
    mem.write_u32((base + 4) as u32, dw1);

    let tr_dequeue_raw = (ring_base & !0x0f) | u64::from(dcs as u8);
    mem.write_u32((base + 8) as u32, tr_dequeue_raw as u32);
    mem.write_u32((base + 12) as u32, (tr_dequeue_raw >> 32) as u32);
}

fn configure_dcbaa(mem: &mut TestMemory, dcbaa: u64, slot_id: u8, dev_ctx_ptr: u64) {
    let entry = dcbaa + u64::from(slot_id) * 8;
    mem.write_u32(entry as u32, dev_ctx_ptr as u32);
    mem.write_u32((entry + 4) as u32, (dev_ctx_ptr >> 32) as u32);
}

fn set_dcbaap(ctrl: &mut XhciController, mem: &mut TestMemory, dcbaa: u64) {
    ctrl.mmio_write(mem, regs::REG_DCBAAP_LO, 4, dcbaa as u32);
    ctrl.mmio_write(mem, regs::REG_DCBAAP_HI, 4, (dcbaa >> 32) as u32);
}

fn ring_endpoint_doorbell(
    ctrl: &mut XhciController,
    mem: &mut TestMemory,
    slot_id: u8,
    endpoint_id: u8,
) {
    let dboff = ctrl.mmio_read_u32(mem, regs::REG_DBOFF) as u64;
    let doorbell = dboff + u64::from(slot_id) * 4;
    ctrl.mmio_write(mem, doorbell, 4, endpoint_id as u32);
}

fn configure_event_ring(
    ctrl: &mut XhciController,
    mem: &mut TestMemory,
    erstba: u64,
    ring_base: u64,
    ring_size_trbs: u32,
) {
    // Single ERST entry.
    MemoryBus::write_u64(mem, erstba, ring_base);
    MemoryBus::write_u32(mem, erstba + 8, ring_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);

    ctrl.mmio_write(mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    ctrl.mmio_write(mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    ctrl.mmio_write(mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    ctrl.mmio_write(mem, regs::REG_INTR0_ERDP_LO, 4, ring_base as u32);
    ctrl.mmio_write(mem, regs::REG_INTR0_ERDP_HI, 4, (ring_base >> 32) as u32);
    ctrl.mmio_write(mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);
}

#[derive(Clone, Debug)]
struct FixedInterruptInDevice;

impl UsbDeviceModel for FixedInterruptInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        assert_eq!(ep_addr, 0x81);
        let mut data = vec![0xde, 0xad, 0xbe, 0xef];
        if data.len() > max_len {
            data.truncate(max_len);
        }
        UsbInResult::Data(data)
    }
}

#[test]
fn xhci_controller_interrupt_in_dmas_and_emits_transfer_event_trb() {
    let keyboard = UsbHidKeyboardHandle::new();

    // Configure the HID device so it is allowed to emit interrupt reports.
    let mut kb_cfg = keyboard.clone();
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        kb_cfg.handle_control_request(setup, None),
        ControlResponse::Ack
    );

    let mut xhci = XhciController::new();
    xhci.attach_device(0, Box::new(keyboard.clone()));
    // Drop root hub attach events so the event ring begins empty.
    while xhci.pop_pending_event().is_some() {}

    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    set_dcbaap(&mut xhci, &mut mem, dcbaa);

    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    assert_ne!(slot_id, 0);
    configure_dcbaa(&mut mem, dcbaa, slot_id, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    let erstba = alloc.alloc(0x40, 0x10) as u64;
    let event_ring = alloc.alloc((TRB_LEN * 16) as u32, 0x10) as u64;
    configure_event_ring(&mut xhci, &mut mem, erstba, event_ring, 16);

    let ring_base = alloc.alloc(TRB_LEN as u32, 0x10) as u64;
    let buf = alloc.alloc(8, 0x10) as u64;

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;
    write_endpoint_context(&mut mem, dev_ctx, EP_ID, 7, 8, ring_base, true);

    Trb::write_to(&make_normal_trb(buf, 8, true, true), &mut mem, ring_base);

    // No report available yet: NAK leaves TRB pending and no event is emitted.
    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, EP_ID);
    xhci.service_event_ring(&mut mem);
    let ev0 = Trb::read_from(&mut mem, event_ring);
    assert_ne!(ev0.trb_type(), TrbType::TransferEvent);
    assert_eq!(
        read_u64(&mem, endpoint_ctx_addr(dev_ctx, EP_ID) + 8),
        (ring_base & !0x0f) | 1,
        "NAK must not advance the Endpoint Context dequeue pointer"
    );

    // Produce a keypress and retry.
    keyboard.key_event(0x04, true);
    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, EP_ID);
    xhci.service_event_ring(&mut mem);

    let mut got = [0u8; 8];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, [0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]);

    // Dequeue pointer should have advanced by one TRB.
    assert_eq!(
        read_u64(&mem, endpoint_ctx_addr(dev_ctx, EP_ID) + 8),
        ((ring_base + TRB_LEN as u64) & !0x0f) | 1,
        "completion should advance the Endpoint Context dequeue pointer"
    );

    // Transfer Event TRB should be present in the event ring.
    let ev = Trb::read_from(&mut mem, event_ring);
    assert_eq!(ev.trb_type(), TrbType::TransferEvent);
    assert!(ev.cycle());
    assert_eq!(ev.slot_id(), slot_id);
    assert_eq!(ev.endpoint_id(), EP_ID);
    assert_eq!(ev.pointer(), ring_base);
    assert_eq!(ev.completion_code_raw(), 1); // Success
    assert_eq!(ev.status & 0x00ff_ffff, 0); // residual
}

#[test]
fn xhci_controller_transfer_event_sets_ed_bit_and_copies_event_data_parameter() {
    let mut xhci = XhciController::new();
    xhci.attach_device(0, Box::new(FixedInterruptInDevice));
    while xhci.pop_pending_event().is_some() {}

    let mut mem = TestMemory::new(0x20_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;
    set_dcbaap(&mut xhci, &mut mem, dcbaa);

    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    assert_ne!(slot_id, 0);
    configure_dcbaa(&mut mem, dcbaa, slot_id, dev_ctx);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    let erstba = alloc.alloc(0x40, 0x10) as u64;
    let event_ring = alloc.alloc((TRB_LEN * 16) as u32, 0x10) as u64;
    configure_event_ring(&mut xhci, &mut mem, erstba, event_ring, 16);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let buf = alloc.alloc(4, 0x10) as u64;

    // Endpoint 1 IN => endpoint id 3.
    const EP_ID: u8 = 3;
    write_endpoint_context(&mut mem, dev_ctx, EP_ID, 7, 8, ring_base, true);

    // TD: Normal (CH=1) then Event Data (IOC=1). The Transfer Event TRB should set ED=1 and copy
    // the Event Data TRB `parameter` payload.
    Trb::write_to(&make_chained_normal_trb(buf, 4, true), &mut mem, ring_base);
    Trb::write_to(
        &make_event_data_trb(0xfeed_beef, true, true),
        &mut mem,
        ring_base + TRB_LEN as u64,
    );

    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, EP_ID);
    xhci.service_event_ring(&mut mem);

    let mut got = [0u8; 4];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, [0xde, 0xad, 0xbe, 0xef]);

    let ev = Trb::read_from(&mut mem, event_ring);
    assert_eq!(ev.trb_type(), TrbType::TransferEvent);
    assert!(ev.control & Trb::CONTROL_EVENT_DATA_BIT != 0);
    assert_eq!(ev.parameter, 0xfeed_beef);
    assert_eq!(ev.slot_id(), slot_id);
    assert_eq!(ev.endpoint_id(), EP_ID);
    assert_eq!(ev.completion_code_raw(), 1); // Success
    assert_eq!(ev.status & 0x00ff_ffff, 0); // residual
}

#[test]
fn xhci_controller_bulk_in_out_webusb_actions_complete_and_emit_events() {
    let dev = UsbWebUsbPassthroughDevice::new();

    let mut xhci = XhciController::new();
    xhci.attach_device(0, Box::new(dev.clone()));
    while xhci.pop_pending_event().is_some() {}

    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x800, 0x40) as u64;
    set_dcbaap(&mut xhci, &mut mem, dcbaa);

    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;
    configure_dcbaa(&mut mem, dcbaa, slot_id, dev_ctx);
    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    let erstba = alloc.alloc(0x40, 0x10) as u64;
    let event_ring = alloc.alloc((TRB_LEN * 16) as u32, 0x10) as u64;
    configure_event_ring(&mut xhci, &mut mem, erstba, event_ring, 16);

    // --- Bulk OUT (endpoint 1 OUT, endpoint id 2) ---
    const BULK_OUT_ID: u8 = 2;
    let out_ring = alloc.alloc(TRB_LEN as u32, 0x10) as u64;
    let out_buf = alloc.alloc(4, 0x10) as u64;
    let out_payload = [0xAAu8, 0xBB, 0xCC, 0xDD];
    mem.write(out_buf as u32, &out_payload);

    write_endpoint_context(&mut mem, dev_ctx, BULK_OUT_ID, 2, 512, out_ring, true);
    Trb::write_to(
        &make_normal_trb(out_buf, out_payload.len() as u32, true, true),
        &mut mem,
        out_ring,
    );

    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, BULK_OUT_ID);
    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let (bulk_out_id, bulk_out_ep) = match actions.pop().unwrap() {
        UsbHostAction::BulkOut { id, endpoint, data } => {
            assert_eq!(endpoint, 0x01);
            assert_eq!(data, out_payload);
            (id, endpoint)
        }
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(bulk_out_ep, 0x01);
    assert_eq!(
        read_u64(&mem, endpoint_ctx_addr(dev_ctx, BULK_OUT_ID) + 8),
        (out_ring & !0x0f) | 1,
        "bulk OUT NAK must keep TD pending"
    );

    // Retry without completion must not duplicate the host action.
    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, BULK_OUT_ID);
    assert!(dev.drain_actions().is_empty());

    dev.push_completion(UsbHostCompletion::BulkOut {
        id: bulk_out_id,
        result: UsbHostCompletionOut::Success {
            bytes_written: out_payload.len() as u32,
        },
    });
    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, BULK_OUT_ID);
    assert_eq!(
        read_u64(&mem, endpoint_ctx_addr(dev_ctx, BULK_OUT_ID) + 8),
        ((out_ring + TRB_LEN as u64) & !0x0f) | 1,
        "bulk OUT completion should advance the Endpoint Context dequeue pointer"
    );

    // --- Bulk IN (endpoint 1 IN, endpoint id 3) ---
    const BULK_IN_ID: u8 = 3;
    let in_ring = alloc.alloc(TRB_LEN as u32, 0x10) as u64;
    let in_buf = alloc.alloc(5, 0x10) as u64;
    let in_payload = [1u8, 2, 3, 4, 5];

    write_endpoint_context(&mut mem, dev_ctx, BULK_IN_ID, 6, 512, in_ring, true);
    Trb::write_to(
        &make_normal_trb(in_buf, in_payload.len() as u32, true, true),
        &mut mem,
        in_ring,
    );

    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, BULK_IN_ID);
    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let bulk_in_id = match actions.pop().unwrap() {
        UsbHostAction::BulkIn {
            id,
            endpoint,
            length,
        } => {
            assert_eq!(endpoint, 0x81);
            assert_eq!(length as usize, in_payload.len());
            id
        }
        other => panic!("unexpected action: {other:?}"),
    };

    // Retry without completion must not duplicate the host action.
    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, BULK_IN_ID);
    assert!(dev.drain_actions().is_empty());

    dev.push_completion(UsbHostCompletion::BulkIn {
        id: bulk_in_id,
        result: UsbHostCompletionIn::Success {
            data: in_payload.to_vec(),
        },
    });
    ring_endpoint_doorbell(&mut xhci, &mut mem, slot_id, BULK_IN_ID);
    // Flush both transfer events into the guest event ring.
    xhci.service_event_ring(&mut mem);
    assert_eq!(
        read_u64(&mem, endpoint_ctx_addr(dev_ctx, BULK_IN_ID) + 8),
        ((in_ring + TRB_LEN as u64) & !0x0f) | 1,
        "bulk IN completion should advance the Endpoint Context dequeue pointer"
    );

    let mut got = vec![0u8; in_payload.len()];
    mem.read(in_buf as u32, &mut got);
    assert_eq!(got, in_payload);

    // Event ring should contain two Transfer Event TRBs (bulk OUT then bulk IN).
    let ev0 = Trb::read_from(&mut mem, event_ring);
    let ev1 = Trb::read_from(&mut mem, event_ring + TRB_LEN as u64);

    assert_eq!(ev0.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev1.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev0.endpoint_id(), BULK_OUT_ID);
    assert_eq!(ev1.endpoint_id(), BULK_IN_ID);
    assert_eq!(ev0.slot_id(), slot_id);
    assert_eq!(ev1.slot_id(), slot_id);
    assert_eq!(ev0.completion_code_raw(), 1);
    assert_eq!(ev1.completion_code_raw(), 1);
}
