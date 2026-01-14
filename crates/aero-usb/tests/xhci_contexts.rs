use aero_usb::xhci::context::{
    DeviceContext32, EndpointContext, InputContext32, InputControlContext, SlotContext,
    CONTEXT_SIZE,
};

mod util;

use util::TestMemory;

#[test]
fn input_and_device_context_read_write_roundtrip() {
    let mut mem = TestMemory::new(0x8000);

    let input_base: u64 = 0x1000;
    let device_base: u64 = 0x2000;

    let ic = InputContext32::new(input_base);

    let mut icc = InputControlContext::default();
    icc.set_drop_flags(0x0123_4567);
    icc.set_add_flags(0x89ab_cdef);
    ic.write_input_control(&mut mem, &icc).unwrap();

    let mut slot = SlotContext::default();
    slot.set_route_string(0x54321);
    slot.set_speed(3);
    slot.set_context_entries(1);
    ic.write_slot_context(&mut mem, &slot).unwrap();

    let mut ep0 = EndpointContext::default();
    ep0.set_endpoint_state(5);
    ep0.set_tr_dequeue_pointer(0xdead_beef_cafe_0000, true);
    ic.write_endpoint_context(&mut mem, 1, &ep0).unwrap();

    assert_eq!(ic.input_control(&mut mem), icc);
    assert_eq!(ic.slot_context(&mut mem).unwrap(), slot);
    assert_eq!(ic.endpoint_context(&mut mem, 1).unwrap(), ep0);

    let dc = DeviceContext32::new(device_base);

    let mut dc_slot = SlotContext::default();
    dc_slot.set_route_string(0xabcde);
    dc_slot.set_context_entries(2);
    dc.write_slot_context(&mut mem, &dc_slot).unwrap();

    let mut dc_ep = EndpointContext::default();
    dc_ep.set_endpoint_state(2);
    dc_ep.set_tr_dequeue_pointer(0x1111_2222_3333_4440, false);
    dc.write_endpoint_context(&mut mem, 4, &dc_ep).unwrap();

    assert_eq!(dc.slot_context(&mut mem), dc_slot);
    assert_eq!(dc.endpoint_context(&mut mem, 4).unwrap(), dc_ep);
}

#[test]
fn context_offsets_match_spec_layout() {
    let mut mem = TestMemory::new(0x4000);
    let base: u64 = 0x1000;

    // Write a SlotContext into a Device Context and ensure it lands at the base address.
    let dc = DeviceContext32::new(base);
    let mut slot = SlotContext::default();
    slot.set_dword(0, 0x1122_3344);
    dc.write_slot_context(&mut mem, &slot).unwrap();

    let slot_dw0 = aero_usb::MemoryBus::read_u32(&mut mem, base);
    assert_eq!(slot_dw0, 0x1122_3344);

    // Endpoint context 0 (index 0 in endpoints array, context index 1) should start at +0x20.
    let mut ep0 = EndpointContext::default();
    ep0.set_dword(0, 0xaabb_ccdd);
    ep0.write_to(&mut mem, base + CONTEXT_SIZE as u64);

    // Read back via DeviceContext to ensure consistent indexing.
    assert_eq!(
        dc.endpoint_context(&mut mem, 1).unwrap().dword(0),
        0xaabb_ccdd
    );

    // Input Context layout: SlotContext starts at +0x20 (after ICC).
    let ic = InputContext32::new(base);
    let mut ic_slot = SlotContext::default();
    ic_slot.set_dword(0, 0x5566_7788);
    ic.write_slot_context(&mut mem, &ic_slot).unwrap();

    let slot_dw0 = SlotContext::read_from(&mut mem, base + CONTEXT_SIZE as u64).dword(0);
    assert_eq!(slot_dw0, 0x5566_7788);
}
