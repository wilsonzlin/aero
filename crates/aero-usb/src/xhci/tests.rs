use super::command_ring::{CommandRing, CommandRingProcessor, EventRing};
use super::context;
use super::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use super::{
    regs, RingCursor, XhciController, PORTSC_CCS, PORTSC_CSC, PORTSC_PEC, PORTSC_PED, PORTSC_PLC,
    PORTSC_PR, PORTSC_PRC,
};
use crate::hub::UsbHubDevice;
use crate::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};
use aero_io_snapshot::io::state::IoSnapshot;

struct DummyUsbDevice;

impl UsbDeviceModel for DummyUsbDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }
}

struct TestMem {
    data: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        let addr = addr as usize;
        self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        let addr = addr as usize;
        self.data[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn read_trb(&mut self, addr: u64) -> Trb {
        let mut bytes = [0u8; TRB_LEN];
        self.read_physical(addr, &mut bytes);
        Trb::from_bytes(bytes)
    }

    fn read_u64(&mut self, addr: u64) -> u64 {
        let mut bytes = [0u8; 8];
        self.read_physical(addr, &mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn write_trb(&mut self, addr: u64, trb: Trb) {
        self.write_physical(addr, &trb.to_bytes());
    }
}

impl MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            // Out-of-bounds reads return all-zeros.
            buf.fill(0);
            return;
        }
        buf.copy_from_slice(&self.data[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            // Ignore out-of-bounds writes.
            return;
        }
        self.data[start..end].copy_from_slice(buf);
    }
}

fn event_completion_code(ev: Trb) -> u8 {
    (ev.status >> 24) as u8
}

fn read_u32(mem: &mut TestMem, addr: u64) -> u32 {
    let mut buf = [0u8; 4];
    mem.read_physical(addr, &mut buf);
    u32::from_le_bytes(buf)
}

fn control_no_data(dev: &mut crate::device::AttachedUsbDevice, setup: crate::SetupPacket) {
    assert_eq!(dev.handle_setup(setup), crate::UsbOutResult::Ack);
    // Status stage is an IN ZLP. Poll in case the device model NAKs while pending.
    loop {
        match dev.handle_in(0, 0) {
            crate::UsbInResult::Data(data) => {
                assert!(data.is_empty());
                break;
            }
            crate::UsbInResult::Nak => continue,
            other => panic!("unexpected status stage response: {other:?}"),
        }
    }
}

#[test]
fn xhci_context_helpers_ignore_slot0_scratchpad_entry() {
    let mut mem = TestMem::new(0x10_000);
    let dcbaa = 0x1000u64;
    let scratchpad = 0x2000u64;

    let mut ctrl = XhciController::new();
    ctrl.set_dcbaap(dcbaa);

    // DCBAA entry 0 is reserved for the scratchpad buffer array pointer (not a Device Context).
    // Guard against any accidental use of slot 0 in context helpers by ensuring they do not read
    // or write through this pointer.
    mem.write_u64(dcbaa, scratchpad);

    // Fill the scratchpad region with a sentinel pattern so accidental writes are observable.
    mem.data[scratchpad as usize..scratchpad as usize + 0x100].fill(0xAA);

    assert!(
        ctrl.read_endpoint_state_from_context(&mut mem, 0, 1)
            .is_none(),
        "slot 0 should never resolve to a valid Endpoint Context state"
    );
    assert!(
        ctrl.read_endpoint_dequeue_from_context(&mut mem, 0, 1)
            .is_none(),
        "slot 0 should never resolve to a valid TR Dequeue Pointer"
    );

    ctrl.write_endpoint_dequeue_to_context(&mut mem, 0, 1, 0x1234_5678, true);
    assert!(
        !ctrl.write_endpoint_state_to_context(
            &mut mem,
            0,
            1,
            super::context::EndpointState::Halted
        ),
        "writing endpoint state for slot 0 should be rejected"
    );

    assert!(
        mem.data[scratchpad as usize..scratchpad as usize + 0x100]
            .iter()
            .all(|&b| b == 0xAA),
        "context helpers must not modify the scratchpad region when slot_id == 0"
    );
}

#[test]
fn controller_endpoint_commands_update_device_context_and_ring_state() {
    let mut mem = TestMem::new(0x20_000);
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;

    let slot_id = 1u8;
    let endpoint_id = 2u8; // EP1 OUT
    let ep_ctx = dev_ctx + u64::from(endpoint_id) * 32;

    let mut ctrl = super::XhciController::new();
    ctrl.set_dcbaap(dcbaa);

    // Enable slot 1, then populate its DCBAA entry with a device context pointer.
    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    assert_eq!(completion.slot_id, slot_id);
    mem.write_u64(dcbaa + 8, dev_ctx);

    // Seed endpoint context state + dequeue pointer.
    mem.write_u32(ep_ctx, 1); // Running
    mem.write_u32(ep_ctx + 8, 0x1110 | 1); // TR Dequeue Pointer low (DCS=1)
    mem.write_u32(ep_ctx + 12, 0);

    // Stop Endpoint -> Stopped (3).
    let completion = ctrl.stop_endpoint(&mut mem, slot_id, endpoint_id);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    assert_eq!(read_u32(&mut mem, ep_ctx) & 0x7, 3);

    // Set TR Dequeue Pointer updates the context + internal ring cursor state.
    let new_trdp = 0x6000u64;
    let completion = ctrl.set_tr_dequeue_pointer(&mut mem, slot_id, endpoint_id, new_trdp, false);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    let dw2 = read_u32(&mut mem, ep_ctx + 8);
    let dw3 = read_u32(&mut mem, ep_ctx + 12);
    let raw = (u64::from(dw3) << 32) | u64::from(dw2);
    assert_eq!(raw & !0x0f, new_trdp);
    assert_eq!(raw & 0x01, 0);

    let ring = ctrl
        .slot_state(slot_id)
        .unwrap()
        .transfer_ring(endpoint_id)
        .expect("transfer ring cursor should have been created");
    assert_eq!(ring.dequeue_ptr(), new_trdp);
    assert!(!ring.cycle_state());

    // Simulate a transfer error halting the endpoint, then Reset Endpoint -> Running (1).
    let halted_dw0 = (read_u32(&mut mem, ep_ctx) & !0x7) | 2;
    mem.write_u32(ep_ctx, halted_dw0);
    let completion = ctrl.reset_endpoint(&mut mem, slot_id, endpoint_id);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    assert_eq!(read_u32(&mut mem, ep_ctx) & 0x7, 1);
}

#[test]
fn write_endpoint_state_to_context_updates_controller_shadow_context() {
    let mut mem = TestMem::new(0x20_000);
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;

    let slot_id = 1u8;
    let endpoint_id = 2u8; // EP1 OUT
    let ep_ctx_addr = dev_ctx + u64::from(endpoint_id) * 32;

    let mut ctrl = super::XhciController::new();
    ctrl.set_dcbaap(dcbaa);

    // Enable slot 1, then populate its DCBAA entry with a device context pointer.
    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    assert_eq!(completion.slot_id, slot_id);
    mem.write_u64(dcbaa + 8, dev_ctx);

    // Seed endpoint context state.
    mem.write_u32(ep_ctx_addr, context::EndpointState::Running.raw().into());

    assert!(
        ctrl.write_endpoint_state_to_context(
            &mut mem,
            slot_id,
            endpoint_id,
            context::EndpointState::Halted
        ),
        "expected endpoint context state write to succeed"
    );
    assert_eq!(
        read_u32(&mut mem, ep_ctx_addr) & 0x7,
        u32::from(context::EndpointState::Halted.raw())
    );

    let slot = ctrl
        .slot_state(slot_id)
        .expect("slot must exist after enable_slot");
    assert_eq!(slot.device_context_ptr(), dev_ctx);
    let shadow = slot
        .endpoint_context(usize::from(endpoint_id - 1))
        .expect("shadow endpoint context must exist");
    assert_eq!(shadow.endpoint_state_enum(), context::EndpointState::Halted);
}

#[test]
fn address_device_copies_slot_routing_and_ep0_tr_dequeue_pointer() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    // All xHCI context pointers are 64-byte aligned in our model.
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let input_ctx = 0x3000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    // Input Control Context: Drop=0, Add = Slot + EP0.
    mem.write_u32(input_ctx, 0);
    mem.write_u32(input_ctx + 0x04, (1 << 0) | (1 << 1));

    // Slot Context: Route String + Root Hub Port Number.
    // Route String lives in DW0 bits 0..19. Encode ports [2, 5] from root => raw 0x25.
    let route_string = 0x25u32;
    let speed_id = 3u32; // arbitrary
    let context_entries = 1u32;
    mem.write_u32(
        input_ctx + 0x20,
        route_string | (speed_id << 20) | (context_entries << 27),
    );
    mem.write_u32(input_ctx + 0x20 + 4, 7u32 << 16); // RootHubPortNumber = 7 (bits 23:16)

    // EP0 Endpoint Context: type=Control, MPS=64, TR Dequeue Pointer.
    let mps = 64u32;
    let ep_type_control = 4u32;
    mem.write_u32(input_ctx + 0x40 + 4, (ep_type_control << 3) | (mps << 16));
    let tr_dequeue_ptr = 0x9000u64;
    let tr_raw = tr_dequeue_ptr | 1; // set DCS
    mem.write_u32(input_ctx + 0x40 + 8, tr_raw as u32);
    mem.write_u32(input_ctx + 0x40 + 12, (tr_raw >> 32) as u32);

    // Command ring:
    //  - Enable Slot
    //  - Address Device (slot 1)
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring, trb0);
    }
    {
        let mut trb1 = Trb::new(input_ctx, 0, 0);
        trb1.set_trb_type(TrbType::AddressDeviceCommand);
        trb1.set_slot_id(1);
        trb1.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, trb1);
    }

    let mut processor = CommandRingProcessor::new(
        mem_size,
        8,
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );

    // Attach a hub chain that matches the route string [2, 5] so Address Device targets the leaf.
    // The default hub model has only 4 ports; allocate enough ports for the route's hop (5).
    let mut inner_hub = UsbHubDevice::with_port_count(8);
    inner_hub.attach(5, Box::new(DummyUsbDevice));
    let mut outer_hub = UsbHubDevice::new();
    outer_hub.attach(2, Box::new(inner_hub));
    processor.attach_root_port(7, Box::new(outer_hub));

    // Enable Slot, then let the guest install a Device Context pointer into the DCBAA entry.
    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);
    let ev0 = mem.read_trb(event_ring);
    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(ev0.slot_id(), 1);
    mem.write_u64(dcbaa + 8, dev_ctx);

    // Now process Address Device.
    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);

    let ev1 = mem.read_trb(event_ring + TRB_LEN as u64);
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev1), CompletionCode::Success.as_u8());

    // Slot Context routing fields should be copied into the Device Context.
    let mut buf = [0u8; 4];
    mem.read_physical(dev_ctx, &mut buf);
    let out_slot_dw0 = u32::from_le_bytes(buf);
    assert_eq!(out_slot_dw0 & 0x000f_ffff, route_string);

    mem.read_physical(dev_ctx + 0x04, &mut buf);
    let out_slot_dw1 = u32::from_le_bytes(buf);
    assert_eq!((out_slot_dw1 >> 16) & 0xff, 7);

    // EP0 TR Dequeue Pointer should be copied through (including DCS).
    let mut lo = [0u8; 4];
    let mut hi = [0u8; 4];
    mem.read_physical(dev_ctx + 0x20 + 8, &mut lo);
    mem.read_physical(dev_ctx + 0x20 + 12, &mut hi);
    let got_raw = (u32::from_le_bytes(hi) as u64) << 32 | (u32::from_le_bytes(lo) as u64);
    assert_eq!(got_raw & !0x0f, tr_dequeue_ptr);
    assert_eq!(got_raw & 0x01, 1);
}

#[test]
fn configure_endpoint_rejects_unsupported_add_flags() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let input_ctx = 0x3000u64;
    let address_ctx = 0x3200u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    // Seed Device Context EP0 MPS so we can verify it is not modified on error.
    mem.write_u32(dev_ctx + 0x20 + 4, 8u32 << 16);

    // Address Device input context: Slot + EP0 for a device on root port 1.
    mem.write_u32(address_ctx, 0);
    mem.write_u32(address_ctx + 0x04, (1 << 0) | (1 << 1));
    mem.write_u32(address_ctx + 0x20 + 4, 1u32 << 16); // RootHubPortNumber = 1
    mem.write_u32(address_ctx + 0x40 + 4, (4u32 << 3) | (8u32 << 16)); // EP0 type=Control, MPS=8

    // Configure Endpoint input context: request EP0 + EP1 OUT (unsupported by MVP).
    mem.write_u32(input_ctx, 0);
    mem.write_u32(input_ctx + 0x04, (1 << 1) | (1 << 2));
    // Provide an EP0 context that would otherwise update MPS to 64 (should be ignored).
    mem.write_u32(input_ctx + 0x40 + 4, (4u32 << 3) | (64u32 << 16));

    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring, trb0);
    }
    {
        let mut trb1 = Trb::new(address_ctx, 0, 0);
        trb1.set_trb_type(TrbType::AddressDeviceCommand);
        trb1.set_slot_id(1);
        trb1.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, trb1);
    }
    {
        let mut trb2 = Trb::new(input_ctx, 0, 0);
        trb2.set_trb_type(TrbType::ConfigureEndpointCommand);
        trb2.set_slot_id(1);
        trb2.set_cycle(true);
        mem.write_trb(cmd_ring + 2 * 16, trb2);
    }

    let mut processor = CommandRingProcessor::new(
        mem_size,
        8,
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );

    processor.attach_root_port(1, Box::new(DummyUsbDevice));

    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);
    let ev0 = mem.read_trb(event_ring);
    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(ev0.slot_id(), 1);
    mem.write_u64(dcbaa + 8, dev_ctx);

    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);
    let ev1 = mem.read_trb(event_ring + TRB_LEN as u64);
    assert_eq!(event_completion_code(ev1), CompletionCode::Success.as_u8());

    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);

    let ev1 = mem.read_trb(event_ring + 2 * 16);
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(
        event_completion_code(ev1),
        CompletionCode::ParameterError.as_u8()
    );

    // Ensure we didn't update EP0 despite the input asking for MPS=64.
    let mut buf = [0u8; 4];
    mem.read_physical(dev_ctx + 0x20 + 4, &mut buf);
    let out_dw1 = u32::from_le_bytes(buf);
    assert_eq!((out_dw1 >> 16) & 0xffff, 8);
}

#[test]
fn noop_and_evaluate_context_emit_events_and_update_ep0_mps() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    // All xHCI context pointers are 64-byte aligned in our model.
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let input_ctx = 0x3000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    let max_slots = 8;

    // DCBAA[1] will be populated after Enable Slot completes (simulating Address Device).

    // Device context EP0 starts with MPS=8.
    // Endpoint Context dword 1 bits 16..31 = Max Packet Size.
    mem.write_u32(dev_ctx + 0x20 + 4, 8u32 << 16);

    // Input control context: Drop=0, Add = Slot + EP0.
    mem.write_u32(input_ctx, 0);
    mem.write_u32(input_ctx + 0x04, (1 << 0) | (1 << 1));

    // Input EP0 context requests MPS=64 and Interval=5.
    mem.write_u32(input_ctx + 0x40, 5u32 << 16);
    mem.write_u32(input_ctx + 0x40 + 4, 64u32 << 16);
    // TR Dequeue Pointer: copy-through field; choose an arbitrary aligned value.
    mem.write_u32(input_ctx + 0x40 + 8, 0xdead_bee0);
    mem.write_u32(input_ctx + 0x40 + 12, 0x0000_0000);

    // Command ring:
    //  - TRB0: Enable Slot
    //  - TRB1: No-Op Command
    //  - TRB2: Evaluate Context (slot 1, input_ctx)
    //  - TRB3: Link back to cmd_ring base, toggle cycle state
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring, trb0);
    }
    {
        let mut trb1 = Trb::new(0, 0, 0);
        trb1.set_trb_type(TrbType::NoOpCommand);
        trb1.set_slot_id(0);
        trb1.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, trb1);
    }
    {
        let mut trb2 = Trb::new(input_ctx, 0, 0);
        trb2.set_trb_type(TrbType::EvaluateContextCommand);
        trb2.set_slot_id(1);
        trb2.set_cycle(true);
        mem.write_trb(cmd_ring + 2 * 16, trb2);
    }
    {
        let mut link = Trb::new(cmd_ring & !0x0f, 0, 0);
        link.set_trb_type(TrbType::Link);
        link.set_link_toggle_cycle(true);
        link.set_cycle(true);
        mem.write_trb(cmd_ring + 3 * 16, link);
    }

    let processor = CommandRingProcessor::new(
        mem_size,
        max_slots,
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );
    let mut processor = processor;

    // Process Enable Slot first so we can populate the DCBAA entry before Evaluate Context.
    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);

    let ev0 = mem.read_trb(event_ring);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(ev0.slot_id(), 1);

    // Enable Slot should have cleared DCBAA[1] to 0. Simulate Address Device by installing a
    // Device Context pointer.
    assert_eq!(mem.read_u64(dcbaa + 8), 0);
    mem.write_u64(dcbaa + 8, dev_ctx);

    processor.process(&mut mem, 16);
    assert!(
        !processor.host_controller_error,
        "processor should not enter HCE"
    );

    // Link TRBs are consumed but do not generate events; we should have exactly two command
    // completion events (No-Op + Evaluate Context) after the initial Enable Slot.
    let ev1 = mem.read_trb(event_ring + TRB_LEN as u64);
    let ev2 = mem.read_trb(event_ring + 2 * 16);

    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev1), CompletionCode::Success.as_u8());
    assert_eq!(event_completion_code(ev2), CompletionCode::Success.as_u8());
    assert_eq!(ev1.pointer(), cmd_ring + TRB_LEN as u64);
    assert_eq!(ev2.pointer(), cmd_ring + 2 * 16);

    // EP0 max packet size should have been updated to 64.
    let mut buf = [0u8; 4];
    mem.read_physical(dev_ctx + 0x20 + 4, &mut buf);
    let out_dw1 = u32::from_le_bytes(buf);
    assert_eq!((out_dw1 >> 16) & 0xffff, 64);

    // Command ring should have followed the Link TRB and toggled the consumer cycle state.
    assert_eq!(processor.command_ring.dequeue_ptr, cmd_ring);
    assert!(!processor.command_ring.cycle_state);

    // Simulate the guest adding another No-Op command after the cycle-state toggle by overwriting
    // TRB0 with cycle=0.
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::NoOpCommand);
        trb0.set_slot_id(0);
        trb0.set_cycle(false);
        mem.write_trb(cmd_ring, trb0);
    }

    processor.process(&mut mem, 16);
    assert!(
        !processor.host_controller_error,
        "processor should not enter HCE after wrap"
    );

    let ev3 = mem.read_trb(event_ring + 3 * 16);
    assert_eq!(ev3.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev3), CompletionCode::Success.as_u8());
    assert_eq!(ev3.pointer(), cmd_ring);
}

#[test]
fn evaluate_context_rejects_unsupported_context_flags() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let input_ctx = 0x3000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    mem.write_u32(dev_ctx + 0x20 + 4, 8u32 << 16);

    // Add EP0 + EP1 (unsupported).
    mem.write_u32(input_ctx, 0);
    mem.write_u32(input_ctx + 0x04, (1 << 1) | (1 << 2));
    mem.write_u32(input_ctx + 0x40 + 4, 64u32 << 16);

    // Command ring:
    //  - TRB0: Enable Slot
    //  - TRB1: Evaluate Context (slot 1, input_ctx)
    {
        let mut cmd = Trb::new(0, 0, 0);
        cmd.set_trb_type(TrbType::EnableSlotCommand);
        cmd.set_cycle(true);
        mem.write_trb(cmd_ring, cmd);
    }
    {
        let mut cmd = Trb::new(input_ctx, 0, 0);
        cmd.set_trb_type(TrbType::EvaluateContextCommand);
        cmd.set_slot_id(1);
        cmd.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, cmd);
    }

    let mut processor = CommandRingProcessor::new(
        mem_size,
        8,
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );

    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);

    // Simulate Address Device by installing a Device Context pointer.
    mem.write_u64(dcbaa + 8, dev_ctx);

    processor.process(&mut mem, 16);
    assert!(!processor.host_controller_error);

    let ev0 = mem.read_trb(event_ring);
    let ev1 = mem.read_trb(event_ring + TRB_LEN as u64);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(
        event_completion_code(ev1),
        CompletionCode::ParameterError.as_u8()
    );

    // Ensure we didn't update EP0 despite the input asking for MPS=64.
    let mut buf = [0u8; 4];
    mem.read_physical(dev_ctx + 0x20 + 4, &mut buf);
    let out_dw1 = u32::from_le_bytes(buf);
    assert_eq!((out_dw1 >> 16) & 0xffff, 8);
}

#[test]
fn endpoint_commands_emit_completion_events_and_update_context() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    let max_slots = 8;
    let slot_id = 1u8;
    let endpoint_id = 2u8; // EP1 OUT context (Device Context index 2).
    let ep_ctx = dev_ctx + u64::from(endpoint_id) * 32;

    // DCBAA[1] will be populated after Enable Slot completes (simulating Address Device).

    // Seed endpoint context state + dequeue pointer.
    mem.write_u32(ep_ctx, 1); // Running
    mem.write_u32(ep_ctx + 8, 0x1110 | 1); // TR Dequeue Pointer low (DCS=1)
    mem.write_u32(ep_ctx + 12, 0);

    // Command ring TRBs:
    //  - Enable Slot
    //  - Stop Endpoint (slot 1, ep_id 2)
    //  - Set TR Dequeue Pointer (slot 1, ep_id 2, ptr=0x6000, dcs=0)
    //  - Reset Endpoint (slot 1, ep_id 2)
    {
        let mut en = Trb::new(0, 0, 0);
        en.set_trb_type(TrbType::EnableSlotCommand);
        en.set_cycle(true);
        mem.write_trb(cmd_ring, en);
    }
    let new_trdp = 0x6000u64;
    {
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::StopEndpointCommand);
        stop.set_slot_id(slot_id);
        stop.set_endpoint_id(endpoint_id);
        stop.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, stop);
    }
    {
        let mut set = Trb::new(new_trdp, 0, 0);
        set.set_trb_type(TrbType::SetTrDequeuePointerCommand);
        set.set_slot_id(slot_id);
        set.set_endpoint_id(endpoint_id);
        set.set_cycle(true);
        mem.write_trb(cmd_ring + 2 * 16, set);
    }
    {
        let mut reset = Trb::new(0, 0, 0);
        reset.set_trb_type(TrbType::ResetEndpointCommand);
        reset.set_slot_id(slot_id);
        reset.set_endpoint_id(endpoint_id);
        reset.set_cycle(true);
        mem.write_trb(cmd_ring + 3 * 16, reset);
    }

    let mut processor = CommandRingProcessor::new(
        mem_size,
        max_slots,
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );

    // Process Enable Slot first, then populate DCBAA[1] (simulating Address Device).
    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);

    let ev0 = mem.read_trb(event_ring);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(ev0.slot_id(), 1);

    mem.write_u64(dcbaa + 8, dev_ctx);

    // Process Stop Endpoint + Set TR Dequeue Pointer.
    processor.process(&mut mem, 2);
    assert!(!processor.host_controller_error);

    let ev1 = mem.read_trb(event_ring + TRB_LEN as u64);
    let ev2 = mem.read_trb(event_ring + 2 * 16);

    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev1), CompletionCode::Success.as_u8());
    assert_eq!(event_completion_code(ev2), CompletionCode::Success.as_u8());
    assert_eq!(ev1.pointer(), cmd_ring + TRB_LEN as u64);
    assert_eq!(ev2.pointer(), cmd_ring + 2 * 16);

    // Stop Endpoint should transition the endpoint state to Stopped (3).
    assert_eq!(read_u32(&mut mem, ep_ctx) & 0x7, 3);

    // Set TR Dequeue Pointer should update dwords 2-3.
    let dw2 = read_u32(&mut mem, ep_ctx + 8);
    let dw3 = read_u32(&mut mem, ep_ctx + 12);
    let raw = (u64::from(dw3) << 32) | u64::from(dw2);
    assert_eq!(raw & !0x0f, new_trdp);
    assert_eq!(raw & 0x01, 0);

    // Simulate a transfer error halting the endpoint, then process Reset Endpoint.
    let halted_dw0 = (read_u32(&mut mem, ep_ctx) & !0x7) | 2;
    mem.write_u32(ep_ctx, halted_dw0);

    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);

    let ev3 = mem.read_trb(event_ring + 3 * 16);
    assert_eq!(ev3.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev3), CompletionCode::Success.as_u8());
    assert_eq!(ev3.pointer(), cmd_ring + 3 * 16);

    // Reset Endpoint should clear the halted condition (Running = 1).
    assert_eq!(read_u32(&mut mem, ep_ctx) & 0x7, 1);
}

#[test]
fn disable_slot_clears_state_and_rejects_double_disable() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    let dcbaa = 0x1000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    let max_slots = 8;

    // Seed DCBAA[1] with a nonzero pointer; Enable Slot should clear it.
    mem.write_u64(dcbaa + 8, 0xdead_beef_cafe_f00d);

    // Command ring:
    //  - TRB0: Enable Slot
    //  - TRB1: Disable Slot (slot 1)
    //  - TRB2: Disable Slot again (slot 1) -> SlotNotEnabledError
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring, trb0);
    }
    {
        let mut trb1 = Trb::new(0, 0, 0);
        trb1.set_trb_type(TrbType::DisableSlotCommand);
        trb1.set_slot_id(1);
        trb1.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, trb1);
    }
    {
        let mut trb2 = Trb::new(0, 0, 0);
        trb2.set_trb_type(TrbType::DisableSlotCommand);
        trb2.set_slot_id(1);
        trb2.set_cycle(true);
        mem.write_trb(cmd_ring + 2 * 16, trb2);
    }

    let mut processor = CommandRingProcessor::new(
        mem_size,
        max_slots,
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );

    // Enable Slot.
    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);
    let ev0 = mem.read_trb(event_ring);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(ev0.slot_id(), 1);
    assert_eq!(mem.read_u64(dcbaa + 8), 0);

    // Simulate Address Device setting a nonzero Device Context pointer, and ensure Disable Slot
    // clears it back to 0.
    mem.write_u64(dcbaa + 8, 0x2222_0000);

    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);
    let ev1 = mem.read_trb(event_ring + TRB_LEN as u64);
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev1), CompletionCode::Success.as_u8());
    assert_eq!(ev1.slot_id(), 1);
    assert_eq!(mem.read_u64(dcbaa + 8), 0);

    // Disable Slot again should fail with SlotNotEnabledError.
    processor.process(&mut mem, 1);
    assert!(!processor.host_controller_error);
    let ev2 = mem.read_trb(event_ring + 2 * 16);
    assert_eq!(ev2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(
        event_completion_code(ev2),
        CompletionCode::SlotNotEnabledError.as_u8()
    );
    assert_eq!(ev2.slot_id(), 1);
}

#[test]
fn enable_slot_allocates_slot_ids_and_reports_exhaustion() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    let dcbaa = 0x1000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    // Seed DCBAA[1] with a garbage pointer to ensure Enable Slot clears it to 0.
    mem.write_u64(dcbaa + 8, 0xdead_beef_cafe_f00d);

    // Command ring:
    //  - TRB0: Enable Slot
    //  - TRB1: Enable Slot (should fail: only one slot supported)
    //  - TRB2: Link back to cmd_ring base, toggle cycle state
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring, trb0);
    }
    {
        let mut trb1 = Trb::new(0, 0, 0);
        trb1.set_trb_type(TrbType::EnableSlotCommand);
        trb1.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, trb1);
    }
    {
        let mut link = Trb::new(cmd_ring & !0x0f, 0, 0);
        link.set_trb_type(TrbType::Link);
        link.set_link_toggle_cycle(true);
        link.set_cycle(true);
        mem.write_trb(cmd_ring + 2 * 16, link);
    }

    let mut processor = CommandRingProcessor::new(
        mem_size,
        1, // max_slots
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );
    processor.process(&mut mem, 16);
    assert!(!processor.host_controller_error);

    let ev0 = mem.read_trb(event_ring);
    let ev1 = mem.read_trb(event_ring + TRB_LEN as u64);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);

    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(ev0.slot_id(), 1);

    assert_eq!(
        event_completion_code(ev1),
        CompletionCode::NoSlotsAvailableError.as_u8()
    );
    assert_eq!(ev1.slot_id(), 0);

    assert_eq!(mem.read_u64(dcbaa + 8), 0);
}

#[test]
fn endpoint_ring_reset_helpers_clear_ep0_control_td_state() {
    let mut mem = TestMem::new(0x20_000);
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;

    let mut ctrl = super::XhciController::new();
    ctrl.set_dcbaap(dcbaa);

    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    mem.write_u64(dcbaa + (u64::from(slot_id) * 8), dev_ctx);

    // Inject non-default EP0 control TD state.
    ctrl.ep0_control_td[usize::from(slot_id)] = super::ControlTdState {
        td_start: None,
        td_cursor: None,
        data_expected: 42,
        data_transferred: 7,
        completion_code: CompletionCode::TrbError,
    };

    // Set TR Dequeue Pointer should reset EP0 TD tracking so the next transfer starts clean.
    let completion = ctrl.set_tr_dequeue_pointer(&mut mem, slot_id, 1, 0x6000, false);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    assert_eq!(
        ctrl.ep0_control_td[usize::from(slot_id)],
        super::ControlTdState::default()
    );

    // Reset Endpoint should also clear any pending TD tracking.
    ctrl.ep0_control_td[usize::from(slot_id)] = super::ControlTdState {
        td_start: None,
        td_cursor: None,
        data_expected: 123,
        data_transferred: 456,
        completion_code: CompletionCode::StallError,
    };
    let completion = ctrl.reset_endpoint(&mut mem, slot_id, 1);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    assert_eq!(
        ctrl.ep0_control_td[usize::from(slot_id)],
        super::ControlTdState::default()
    );
}

#[test]
fn command_ring_context_commands_reset_ep0_control_td_state() {
    let mut mem = TestMem::new(0x20_000);
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let eval_ctx = 0x3000u64;
    let cfg_ctx = 0x3400u64;
    let cmd_ring = 0x4000u64;

    let mut ctrl = super::XhciController::new();
    ctrl.set_dcbaap(dcbaa);

    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    mem.write_u64(dcbaa + (u64::from(slot_id) * 8), dev_ctx);

    // Evaluate Context input context: Drop=0, Add=EP0.
    mem.write_u32(eval_ctx, 0);
    mem.write_u32(eval_ctx + 0x04, 1 << 1);
    let eval_trdp = 0x9000u64;
    let eval_raw = eval_trdp | 1;
    mem.write_u32(eval_ctx + 0x40 + 8, eval_raw as u32);
    mem.write_u32(eval_ctx + 0x40 + 12, (eval_raw >> 32) as u32);

    // Configure Endpoint input context: Drop=0, Add=EP0.
    mem.write_u32(cfg_ctx, 0);
    mem.write_u32(cfg_ctx + 0x04, 1 << 1);
    let cfg_trdp = 0xa000u64;
    let cfg_raw = cfg_trdp;
    mem.write_u32(cfg_ctx + 0x40 + 8, cfg_raw as u32);
    mem.write_u32(cfg_ctx + 0x40 + 12, (cfg_raw >> 32) as u32);

    // Command ring:
    //  - TRB0: Evaluate Context (slot, eval_ctx)
    //  - TRB1: Configure Endpoint (slot, cfg_ctx)
    {
        let mut trb0 = Trb::new(eval_ctx, 0, 0);
        trb0.set_trb_type(TrbType::EvaluateContextCommand);
        trb0.set_slot_id(slot_id);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring, trb0);
    }
    {
        let mut trb1 = Trb::new(cfg_ctx, 0, 0);
        trb1.set_trb_type(TrbType::ConfigureEndpointCommand);
        trb1.set_slot_id(slot_id);
        trb1.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, trb1);
    }

    ctrl.set_command_ring(cmd_ring, true);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // Evaluate Context updates EP0's transfer ring state and should clear any in-flight EP0 control
    // TD tracking.
    ctrl.ep0_control_td[usize::from(slot_id)] = super::ControlTdState {
        td_start: None,
        td_cursor: None,
        data_expected: 7,
        data_transferred: 3,
        completion_code: CompletionCode::TrbError,
    };
    ctrl.process_command_ring(&mut mem, 1);
    assert_eq!(
        ctrl.ep0_control_td[usize::from(slot_id)],
        super::ControlTdState::default()
    );
    let ring = ctrl
        .slot_state(slot_id)
        .unwrap()
        .transfer_ring(1)
        .expect("EP0 transfer ring cursor should be updated");
    assert_eq!(ring.dequeue_ptr(), eval_trdp);
    assert!(ring.cycle_state());

    // Configure Endpoint can also update EP0; ensure it resets control TD tracking as well.
    ctrl.ep0_control_td[usize::from(slot_id)] = super::ControlTdState {
        td_start: None,
        td_cursor: None,
        data_expected: 99,
        data_transferred: 12,
        completion_code: CompletionCode::StallError,
    };
    ctrl.process_command_ring(&mut mem, 1);
    assert_eq!(
        ctrl.ep0_control_td[usize::from(slot_id)],
        super::ControlTdState::default()
    );
    let ring = ctrl
        .slot_state(slot_id)
        .unwrap()
        .transfer_ring(1)
        .expect("EP0 transfer ring cursor should be updated");
    assert_eq!(ring.dequeue_ptr(), cfg_trdp);
    assert!(!ring.cycle_state());
}

#[test]
fn command_ring_address_device_resets_ep0_control_td_state() {
    let mut mem = TestMem::new(0x20_000);
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let input_ctx = 0x3000u64;
    let cmd_ring = 0x4000u64;

    let mut ctrl = super::XhciController::new();
    ctrl.set_dcbaap(dcbaa);

    // Provide a device on root port 1 so Address Device can resolve the topology.
    ctrl.attach_device(0, Box::new(DummyUsbDevice));

    let completion = ctrl.enable_slot(&mut mem);
    assert_eq!(
        completion.completion_code,
        super::CommandCompletionCode::Success
    );
    let slot_id = completion.slot_id;
    assert_ne!(slot_id, 0);

    // Install a Device Context pointer for the slot.
    mem.write_u64(dcbaa + (u64::from(slot_id) * 8), dev_ctx);

    // Input Control Context: Drop=0, Add = Slot + EP0 (required for Address Device).
    mem.write_u32(input_ctx, 0);
    mem.write_u32(input_ctx + 0x04, (1 << 0) | (1 << 1));

    // Slot Context: bind to root port 1 with an empty route string.
    mem.write_u32(input_ctx + 0x20, 0);
    mem.write_u32(input_ctx + 0x20 + 4, 1u32 << 16);

    // EP0 context: seed TRDP + DCS=1.
    let trdp = 0x9000u64;
    let trdp_raw = trdp | 1;
    mem.write_u32(input_ctx + 0x40 + 8, trdp_raw as u32);
    mem.write_u32(input_ctx + 0x40 + 12, (trdp_raw >> 32) as u32);

    // Command ring: Address Device for the enabled slot.
    {
        let mut trb0 = Trb::new(input_ctx, 0, 0);
        trb0.set_trb_type(TrbType::AddressDeviceCommand);
        trb0.set_slot_id(slot_id);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring, trb0);
    }

    ctrl.set_command_ring(cmd_ring, true);
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));

    // If a control TD was in-flight, Address Device updates EP0's dequeue pointer and must reset
    // internal TD tracking to avoid consuming stale cursor state.
    ctrl.ep0_control_td[usize::from(slot_id)] = super::ControlTdState {
        td_start: None,
        td_cursor: None,
        data_expected: 7,
        data_transferred: 3,
        completion_code: CompletionCode::TrbError,
    };

    ctrl.process_command_ring(&mut mem, 1);

    assert_eq!(
        ctrl.ep0_control_td[usize::from(slot_id)],
        super::ControlTdState::default()
    );
    let ring = ctrl
        .slot_state(slot_id)
        .unwrap()
        .transfer_ring(1)
        .expect("EP0 transfer ring cursor should be updated");
    assert_eq!(ring.dequeue_ptr(), trdp);
    assert!(ring.cycle_state());
}

#[test]
fn controller_snapshot_roundtrip_is_deterministic() {
    use aero_io_snapshot::io::state::IoSnapshot;

    use super::context::SlotContext;
    use super::interrupter::IMAN_IE;
    use super::ring::RingCursor;
    use super::trb::{CompletionCode, Trb, TrbType};
    use super::{regs, CommandCompletionCode, XhciController, PORTSC_PR};

    use crate::hid::UsbHidKeyboardHandle;

    let mut mem = TestMem::new(0x40_000);

    // Program a tiny event ring: a single TRB deep so we can force the ring-full condition and keep
    // some controller-side pending events around for snapshot coverage.
    let erstba = 0x1000u64;
    let ring_base = 0x2000u64;
    mem.write_u64(erstba, ring_base);
    mem.write_u32(erstba + 8, 1);
    mem.write_u32(erstba + 12, 0);

    // Use a non-default port count so the snapshot exercises port vector resizing.
    let mut xhci = XhciController::with_port_count(3);

    // Mutate architectural registers.
    xhci.usbcmd = 0x1122_3344;
    // `usbsts` stores only non-derived bits; mask out EINT/HCH/HCE so roundtrips are stable.
    xhci.usbsts = 0x5566_7788 & !(regs::USBSTS_EINT | regs::USBSTS_HCH | regs::USBSTS_HCE);
    // CRCR contains the command ring dequeue pointer (64-byte aligned) + low flag bits (cycle).
    let cmd_ring_ptr = 0x5000u64;
    xhci.crcr = cmd_ring_ptr | 1;
    xhci.sync_command_ring_from_crcr();
    xhci.set_dcbaap(0x8000);
    xhci.config = 0x210;
    xhci.mfindex = 0x1234;
    xhci.dnctrl = 0x0a0b_0c0d;

    // Configure interrupter 0 (this bumps generation counters which are part of the snapshot).
    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, erstba >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, ring_base);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, ring_base >> 32);
    xhci.mmio_write(regs::REG_INTR0_IMAN, 4, u64::from(IMAN_IE));

    // Attach a device so port snapshots include a nested `AttachedUsbDevice` record.
    let keyboard = UsbHidKeyboardHandle::new();
    xhci.attach_device(0, Box::new(keyboard.clone()));

    // Mutate port state (start a reset so the timer is non-zero).
    xhci.write_portsc(0, PORTSC_PR);
    for _ in 0..10 {
        xhci.tick_1ms(&mut mem);
    }

    // Also mutate device state so the nested device snapshot isn't trivially default.
    keyboard.key_event(0x04, true); // HID usage for "A".

    // Enable a slot and bind it to the attached device (exercises slot + endpoint snapshot fields).
    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    assert_eq!(enable.slot_id, 1);

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = xhci.address_device(1, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    // Seed additional controller-local execution state so snapshot covers endpoint + control TD
    // bookkeeping.
    xhci.host_controller_error = true;
    xhci.cmd_kick = true;
    xhci.ring_doorbell(1, 1);
    xhci.ep0_control_td[1].td_start = Some(RingCursor::new(0xa000, true));
    xhci.ep0_control_td[1].td_cursor = Some(RingCursor::new(0xa010, false));
    xhci.ep0_control_td[1].data_expected = 8;
    xhci.ep0_control_td[1].data_transferred = 4;
    xhci.ep0_control_td[1].completion_code = CompletionCode::TrbError;

    // Add some endpoint + transfer ring cursor state.
    {
        let slot = &mut xhci.slots[1];
        slot.device_context_ptr = 0xdead_beef_0000;
        slot.endpoint_contexts[0].set_dword(0, 0x0102_0304);
        slot.transfer_rings[0] = Some(RingCursor::new(0x9000, true));
    }

    // Queue a host-side event (in addition to the port status change event) and service the guest
    // event ring once to mutate the producer cursor + IMAN.IP.
    let mut evt = Trb {
        parameter: 0x1111_2222_3333_4444,
        ..Default::default()
    };
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);
    xhci.service_event_ring(&mut mem);

    // Non-zero drop counter coverage.
    xhci.dropped_event_trbs = 7;

    let snapshot1 = xhci.save_state();
    let snapshot2 = xhci.save_state();
    assert_eq!(snapshot1, snapshot2, "snapshot bytes must be deterministic");

    let mut restored = XhciController::new();
    restored
        .load_state(&snapshot1)
        .expect("snapshot should load");

    // State should round-trip (spot-check a few representative fields).
    assert_eq!(restored.port_count, xhci.port_count);
    assert_eq!(restored.usbcmd, xhci.usbcmd & regs::USBCMD_SNAPSHOT_MASK);
    assert_eq!(
        restored.usbsts,
        xhci.usbsts & !(regs::USBSTS_EINT | regs::USBSTS_HCH | regs::USBSTS_HCE)
    );
    assert_eq!(restored.host_controller_error, xhci.host_controller_error);
    assert_eq!(restored.crcr, xhci.crcr & regs::CRCR_SNAPSHOT_MASK);
    assert_eq!(restored.dcbaap, xhci.dcbaap & regs::DCBAAP_SNAPSHOT_MASK);
    assert_eq!(restored.config, xhci.config & regs::CONFIG_SNAPSHOT_MASK);
    assert_eq!(restored.mfindex, xhci.mfindex & regs::runtime::MFINDEX_MASK);
    assert_eq!(restored.dnctrl, xhci.dnctrl);
    assert_eq!(restored.command_ring, xhci.command_ring);
    assert_eq!(restored.cmd_kick, xhci.cmd_kick);
    assert_eq!(restored.active_endpoints, xhci.active_endpoints);
    assert_eq!(restored.ep0_control_td, xhci.ep0_control_td);
    assert_eq!(
        restored.interrupter0.iman_raw(),
        xhci.interrupter0.iman_raw()
    );
    assert_eq!(restored.interrupter0.erst_gen, xhci.interrupter0.erst_gen);
    assert_eq!(restored.interrupter0.erdp_gen, xhci.interrupter0.erdp_gen);
    assert_eq!(
        restored.event_ring.save_snapshot(),
        xhci.event_ring.save_snapshot()
    );
    assert_eq!(
        restored.ports[0].save_snapshot(),
        xhci.ports[0].save_snapshot()
    );
    assert_eq!(restored.slots[1].enabled, xhci.slots[1].enabled);
    assert_eq!(restored.slots[1].port_id, xhci.slots[1].port_id);
    assert_eq!(
        restored.slots[1].device_context_ptr,
        xhci.slots[1].device_context_ptr
    );
    assert_eq!(
        restored.slots[1].endpoint_contexts[0].dword(0),
        xhci.slots[1].endpoint_contexts[0].dword(0)
    );
    assert_eq!(
        restored.slots[1].transfer_rings[0].unwrap().dequeue_ptr(),
        xhci.slots[1].transfer_rings[0].unwrap().dequeue_ptr()
    );
    assert_eq!(restored.pending_events, xhci.pending_events);
    assert_eq!(restored.dropped_event_trbs, xhci.dropped_event_trbs);
    assert!(restored.slot_device_mut(1).is_some());

    // A save after restore should reproduce byte-identical snapshots.
    let snapshot3 = restored.save_state();
    assert_eq!(snapshot1, snapshot3);
}

#[test]
fn controller_mmio_doorbell_processes_command_ring_and_posts_events() {
    use super::interrupter::{IMAN_IE, IMAN_IP};
    use super::regs;

    let mut mem = TestMem::new(0x20_000);

    // All xHCI context pointers are 64-byte aligned in our model.
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let input_ctx = 0x3000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;
    let erst = 0x6000u64;

    // Device context EP0 starts with MPS=8.
    mem.write_u32(dev_ctx + 0x20 + 4, 8u32 << 16);

    // Input control context: Drop=0, Add = Slot + EP0.
    mem.write_u32(input_ctx, 0);
    mem.write_u32(input_ctx + 0x04, (1 << 0) | (1 << 1));

    // Input EP0 context requests MPS=64 and Interval=5.
    mem.write_u32(input_ctx + 0x40, 5u32 << 16);
    mem.write_u32(input_ctx + 0x40 + 4, 64u32 << 16);
    mem.write_u32(input_ctx + 0x40 + 8, 0xdead_bee0);
    mem.write_u32(input_ctx + 0x40 + 12, 0);

    // Command ring:
    //  - TRB0: Enable Slot
    //  - TRB1: cycle=0 sentinel (ring empty after TRB0)
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring, trb0);
    }
    {
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.set_cycle(false);
        mem.write_trb(cmd_ring + TRB_LEN as u64, stop);
    }

    // Event Ring Segment Table (ERST) with a single segment pointing at `event_ring`.
    mem.write_u64(erst, event_ring);
    mem.write_u32(erst + 8, 16); // segment size in TRBs
    mem.write_u32(erst + 12, 0);

    let mut ctrl = super::XhciController::new();

    // Program controller state via MMIO.
    ctrl.mmio_write(regs::REG_DCBAAP_LO, 4, dcbaa);
    ctrl.mmio_write(regs::REG_DCBAAP_HI, 4, dcbaa >> 32);
    ctrl.mmio_write(regs::REG_CONFIG, 4, 8); // MaxSlotsEn

    // Command ring base + RCS=1.
    ctrl.mmio_write(regs::REG_CRCR_LO, 4, cmd_ring | 1);
    ctrl.mmio_write(regs::REG_CRCR_HI, 4, cmd_ring >> 32);

    // Program interrupter 0 event ring.
    ctrl.mmio_write(regs::REG_INTR0_IMAN, 4, u64::from(IMAN_IE));
    ctrl.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    ctrl.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, erst);
    ctrl.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, erst >> 32);
    ctrl.mmio_write(regs::REG_INTR0_ERDP_LO, 4, event_ring);
    ctrl.mmio_write(regs::REG_INTR0_ERDP_HI, 4, event_ring >> 32);

    // Start controller and clear the synthetic RUN-transition IRQ.
    ctrl.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
    ctrl.tick_1ms(&mut mem);
    ctrl.mmio_write(regs::REG_USBSTS, 4, u64::from(regs::USBSTS_EINT));
    assert!(
        !ctrl.irq_level(),
        "IRQ should be clear before ringing doorbell"
    );

    // Ring the command doorbell (DB0).
    ctrl.mmio_write(regs::DBOFF_VALUE as u64, 4, 0);
    ctrl.tick_1ms(&mut mem);

    // Enable Slot -> one completion event.
    let ev0 = mem.read_trb(event_ring);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(ev0.pointer(), cmd_ring);
    assert_eq!(ev0.slot_id(), 1);

    // Interrupt should be asserted for the completion event.
    assert!(ctrl.irq_level());
    assert_ne!(
        (ctrl.mmio_read(regs::REG_USBSTS, 4) as u32) & regs::USBSTS_EINT,
        0
    );

    // Clear interrupt pending state so we can observe a second interrupt.
    ctrl.mmio_write(regs::REG_INTR0_IMAN, 4, u64::from(IMAN_IP | IMAN_IE));
    ctrl.mmio_write(regs::REG_USBSTS, 4, u64::from(regs::USBSTS_EINT));
    assert!(!ctrl.irq_level());

    // Enable Slot clears DCBAA[1] to 0; install the device context pointer after it completes.
    mem.write_u64(dcbaa + 8, dev_ctx);

    // Command ring continuation at TRB1/TRB2:
    //  - TRB1: No-Op Command
    //  - TRB2: Evaluate Context (slot 1, input_ctx)
    //  - TRB3: cycle=0 sentinel
    {
        let mut trb1 = Trb::new(0, 0, 0);
        trb1.set_trb_type(TrbType::NoOpCommand);
        trb1.set_slot_id(0);
        trb1.set_cycle(true);
        mem.write_trb(cmd_ring + TRB_LEN as u64, trb1);
    }
    {
        let mut trb2 = Trb::new(input_ctx, 0, 0);
        trb2.set_trb_type(TrbType::EvaluateContextCommand);
        trb2.set_slot_id(1);
        trb2.set_cycle(true);
        mem.write_trb(cmd_ring + 2 * 16, trb2);
    }
    {
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.set_cycle(false);
        mem.write_trb(cmd_ring + 3 * 16, stop);
    }

    // Ring the command doorbell (DB0) again.
    ctrl.mmio_write(regs::DBOFF_VALUE as u64, 4, 0);
    ctrl.tick_1ms(&mut mem);

    // Two commands -> two completion events.
    let ev1 = mem.read_trb(event_ring + TRB_LEN as u64);
    let ev2 = mem.read_trb(event_ring + 2 * 16);
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev1), CompletionCode::Success.as_u8());
    assert_eq!(event_completion_code(ev2), CompletionCode::Success.as_u8());
    assert_eq!(ev1.pointer(), cmd_ring + TRB_LEN as u64);
    assert_eq!(ev2.pointer(), cmd_ring + 2 * 16);
    assert_eq!(ev2.slot_id(), 1);

    // EP0 max packet size should have been updated to 64.
    let mut buf = [0u8; 4];
    mem.read_physical(dev_ctx + 0x20 + 4, &mut buf);
    let out_dw1 = u32::from_le_bytes(buf);
    assert_eq!((out_dw1 >> 16) & 0xffff, 64);

    // Interrupt should be asserted for the second batch of events.
    assert!(ctrl.irq_level());
    assert_ne!(
        (ctrl.mmio_read(regs::REG_USBSTS, 4) as u32) & regs::USBSTS_EINT,
        0
    );
}

#[test]
fn snapshot_roundtrip_preserves_regs_ports_slots_and_device_tree() {
    use core::any::Any;

    use crate::hid::UsbHidKeyboardHandle;
    use crate::SetupPacket;

    let kb0 = UsbHidKeyboardHandle::new();
    let kb1 = UsbHidKeyboardHandle::new();
    let mut mem = TestMem::new(0x20_000);

    let mut ctrl = XhciController::with_port_count(2);
    ctrl.usbcmd = regs::USBCMD_RUN;
    // `usbsts` stores only the non-derived subset of USBSTS bits. Snapshots persist only the
    // architectural bits we model, so clamp to the snapshot mask and mask out derived bits
    // (EINT/HCH/HCE) for stable roundtrips.
    ctrl.usbsts = 0x1122_3344
        & regs::USBSTS_SNAPSHOT_MASK
        & !(regs::USBSTS_EINT | regs::USBSTS_HCH | regs::USBSTS_HCE);
    // CRCR pointers are 64-byte aligned in xHCI; the low bits are flags (cycle state, etc).
    ctrl.crcr = 0x1234_5678_9abc_de01;
    ctrl.sync_command_ring_from_crcr();
    ctrl.set_dcbaap(0xdead_beef_1000);
    ctrl.config = 0x0208;

    // Attach two devices so we exercise per-port snapshots.
    ctrl.attach_device(0, Box::new(kb0.clone()));
    ctrl.attach_device(1, Box::new(kb1.clone()));

    // Port 0: complete a reset so PED/PEC become set.
    ctrl.write_portsc(0, PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms_no_dma();
    }

    // Port 1: start a reset and leave it in-progress across snapshot/restore.
    ctrl.write_portsc(1, PORTSC_PR);
    for _ in 0..7 {
        ctrl.tick_1ms_no_dma();
    }

    // Configure the keyboard on port 0 so it can queue interrupt reports.
    {
        let dev0 = ctrl.ports[0].device_mut().expect("device attached");
        control_no_data(
            dev0,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
    }
    // Queue a report and leave it pending across snapshot/restore.
    kb0.key_event(0x04, true); // 'A'

    // Seed slot state + ring cursors.
    let slot = &mut ctrl.slots[1];
    slot.enabled = true;
    slot.port_id = Some(1);
    slot.device_attached = true;
    slot.device_context_ptr = 0x2000;
    slot.slot_context.set_dword(0, 0xfeed_face);
    slot.endpoint_contexts[0].set_dword(0, 0x1111_2222);
    slot.transfer_rings[0] = Some(RingCursor::new(0x3000, true));

    // Configure an event ring and enqueue some in-flight events so the event ring producer has
    // nontrivial state across snapshot/restore.
    //
    // Use a tiny ring segment so it fills up after two events, ensuring we preserve both the guest
    // event ring producer cursor and the remaining host-side pending event queue.
    let erstba = 0x1000u64;
    let event_ring_base = 0x2000u64;
    mem.write_u64(erstba, event_ring_base);
    mem.write_u32(erstba + 8, 2); // segment size in TRBs

    ctrl.interrupter0.write_iman(super::interrupter::IMAN_IE);
    ctrl.interrupter0.write_erstsz(1);
    ctrl.interrupter0.write_erstba(erstba);
    ctrl.interrupter0.write_erdp(event_ring_base);

    assert_eq!(ctrl.pending_event_count(), 3);
    ctrl.service_event_ring(&mut mem);
    assert_eq!(ctrl.pending_event_count(), 1);
    let ev0_before = mem.read_trb(event_ring_base);
    let ev1_before = mem.read_trb(event_ring_base + TRB_LEN as u64);
    assert_eq!(ev0_before.trb_type(), TrbType::PortStatusChangeEvent);
    assert_eq!(ev1_before.trb_type(), TrbType::PortStatusChangeEvent);
    assert!(ev0_before.cycle());
    assert!(ev1_before.cycle());

    let snapshot1 = ctrl.save_state();
    let snapshot2 = ctrl.save_state();
    assert_eq!(
        snapshot1, snapshot2,
        "snapshot output must be deterministic"
    );

    let mut restored = XhciController::with_port_count(1);
    restored
        .load_state(&snapshot1)
        .expect("restore should succeed");

    assert_eq!(restored.port_count, 2);
    assert_eq!(restored.usbcmd, regs::USBCMD_RUN);
    // Snapshot/restore persists the software-visible USBSTS view (masked) and reconstructs derived
    // bits (EINT/HCH/HCE) from the saved interrupter + error latches. The internal `usbsts` field
    // therefore contains only the non-derived subset of USBSTS bits that are snapshotted.
    let expected_usbsts = (ctrl.usbsts_read() & regs::USBSTS_SNAPSHOT_MASK)
        & !(regs::USBSTS_EINT | regs::USBSTS_HCH | regs::USBSTS_HCE);
    assert_eq!(restored.usbsts, expected_usbsts);
    assert_eq!(
        restored.usbsts_read() & regs::USBSTS_SNAPSHOT_MASK,
        ctrl.usbsts_read() & regs::USBSTS_SNAPSHOT_MASK
    );
    assert_eq!(restored.crcr, ctrl.crcr);
    assert_eq!(restored.dcbaap, 0xdead_beef_1000);
    assert_eq!(restored.config, ctrl.config);
    assert_eq!(restored.mfindex, ctrl.mfindex);
    assert_eq!(restored.dropped_event_trbs(), ctrl.dropped_event_trbs());
    assert_eq!(restored.pending_event_count(), ctrl.pending_event_count());

    // Ensure the guest event ring producer state was restored by consuming one event (advance ERDP)
    // and verifying that the next event is written into the consumed slot instead of overwriting
    // the still-pending event at index 1.
    assert_eq!(mem.read_trb(event_ring_base), ev0_before);
    assert_eq!(mem.read_trb(event_ring_base + TRB_LEN as u64), ev1_before);

    // Guest consumes the first event, leaving index 1 unconsumed.
    restored
        .interrupter0
        .write_erdp(event_ring_base + TRB_LEN as u64);
    restored.service_event_ring(&mut mem);
    assert_eq!(restored.pending_event_count(), 0);

    let ev0_after = mem.read_trb(event_ring_base);
    let ev1_after = mem.read_trb(event_ring_base + TRB_LEN as u64);
    assert_eq!(
        ev1_after, ev1_before,
        "unconsumed event should not be overwritten"
    );
    assert_eq!(ev0_after.trb_type(), TrbType::PortStatusChangeEvent);
    assert!(
        !ev0_after.cycle(),
        "producer should have wrapped and toggled cycle"
    );
    assert_eq!(((ev0_after.parameter >> 24) & 0xff) as u8, 1);

    // Port 0 should be connected/enabled with CSC, PEC, and PRC latched.
    let port0 = restored.read_portsc(0);
    assert_ne!(port0 & PORTSC_CCS, 0);
    assert_ne!(port0 & PORTSC_PED, 0);
    assert_ne!(port0 & PORTSC_CSC, 0);
    assert_ne!(port0 & PORTSC_PEC, 0);
    assert_ne!(port0 & PORTSC_PRC, 0);
    assert_eq!(port0 & PORTSC_PR, 0);

    // Port 1 should still be resetting (PR=1, PED=0).
    let port1 = restored.read_portsc(1);
    assert_ne!(port1 & PORTSC_CCS, 0);
    assert_eq!(port1 & PORTSC_PED, 0);
    assert_ne!(port1 & PORTSC_PR, 0);
    assert_eq!(port1 & PORTSC_PRC, 0);

    // Slot state and transfer ring cursor should survive restore.
    let slot = &restored.slots[1];
    assert!(slot.enabled);
    assert_eq!(slot.port_id, Some(1));
    assert!(slot.device_attached);
    assert_eq!(slot.device_context_ptr, 0x2000);
    assert_eq!(slot.slot_context.dword(0), 0xfeed_face);
    assert_eq!(slot.endpoint_contexts[0].dword(0), 0x1111_2222);
    assert_eq!(slot.transfer_ring(1), Some(RingCursor::new(0x3000, true)));

    // The keyboard's pending report should survive snapshot/restore via the nested ADEV/UKBD
    // snapshot.
    let restored_dev = restored.ports[0]
        .device_mut()
        .expect("device should be reconstructed");
    match restored_dev.handle_in(1, 8) {
        crate::UsbInResult::Data(data) => assert!(!data.is_empty()),
        other => panic!("expected queued report after restore, got {other:?}"),
    }

    // The restored device should still be a keyboard handle under the hood.
    let any = restored_dev.model_mut() as &mut dyn Any;
    assert!(
        any.downcast_mut::<UsbHidKeyboardHandle>().is_some(),
        "restored device should be a keyboard model"
    );

    // Confirm that port 1 reset completes after the remaining 43ms (50 - 7).
    for _ in 0..42 {
        restored.tick_1ms_no_dma();
    }
    assert_ne!(restored.read_portsc(1) & PORTSC_PR, 0);
    restored.tick_1ms_no_dma();
    let port1 = restored.read_portsc(1);
    assert_eq!(port1 & PORTSC_PR, 0);
    assert_ne!(port1 & PORTSC_PED, 0);
    assert_ne!(port1 & PORTSC_PRC, 0);
}

#[test]
fn endpoint_doorbell_ignores_endpoint_id_zero() {
    use super::context::SlotContext;
    use super::CommandCompletionCode;

    let mut mem = TestMem::new(0x40_000);
    let mut xhci = XhciController::new();
    xhci.set_dcbaap(0x1000);
    xhci.attach_device(0, Box::new(DummyUsbDevice));

    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    xhci.ring_doorbell(slot_id, 0);
    assert!(
        xhci.active_endpoints.is_empty(),
        "endpoint ID 0 is reserved and should not enqueue work"
    );

    xhci.ring_doorbell(slot_id, 33);
    assert!(
        xhci.active_endpoints.is_empty(),
        "invalid endpoint IDs must not alias onto valid doorbell targets"
    );

    xhci.ring_doorbell(slot_id, 1);
    assert_eq!(xhci.active_endpoints.len(), 1);
}

#[test]
fn endpoint_doorbell_dedupes_active_queue_entries() {
    use super::context::SlotContext;
    use super::CommandCompletionCode;

    let mut mem = TestMem::new(0x40_000);
    let mut xhci = XhciController::new();
    xhci.set_dcbaap(0x1000);
    xhci.attach_device(0, Box::new(DummyUsbDevice));

    let enable = xhci.enable_slot(&mut mem);
    assert_eq!(enable.completion_code, CommandCompletionCode::Success);
    let slot_id = enable.slot_id;

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let addr = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(addr.completion_code, CommandCompletionCode::Success);

    xhci.ring_doorbell(slot_id, 1);
    xhci.ring_doorbell(slot_id, 1);
    assert_eq!(xhci.active_endpoints.len(), 1);
}

#[test]
fn port_remote_wakeup_resumes_u3_link_and_latches_plc() {
    use crate::hid::UsbHidKeyboardHandle;

    let kb = UsbHidKeyboardHandle::new();
    let mut ctrl = XhciController::new();
    ctrl.attach_device(0, Box::new(kb.clone()));

    // Reset the port so it becomes enabled (PED=1) and the keyboard can be configured.
    ctrl.write_portsc(0, PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms_no_dma();
    }

    // Drain initial port-change events (attach + reset completion).
    while ctrl.pop_pending_event().is_some() {}

    // Configure the keyboard and enable DEVICE_REMOTE_WAKEUP so it will signal wake events while
    // suspended.
    let dev = ctrl
        .find_device_by_topology(1, &[])
        .expect("expected keyboard behind root port 1");
    control_no_data(
        dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 1,      // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Suspend the port (U3) via Port Link State write strobe.
    ctrl.write_portsc(0, regs::PORTSC_LWS | (3u32 << regs::PORTSC_PLS_SHIFT));
    let portsc = ctrl.read_portsc(0);
    assert_eq!(
        (portsc & regs::PORTSC_PLS_MASK) >> regs::PORTSC_PLS_SHIFT,
        3,
        "expected port to enter U3"
    );
    assert_ne!(portsc & PORTSC_PLC, 0, "expected PLC to latch on suspend");

    // Clear PLC so the subsequent remote wake can latch it again.
    ctrl.write_portsc(0, PORTSC_PLC);
    assert_eq!(ctrl.read_portsc(0) & PORTSC_PLC, 0);
    while ctrl.pop_pending_event().is_some() {}

    // While suspended, a keypress should request remote wake and the controller should resume the
    // port back to U0, latching PLC and queueing a Port Status Change Event TRB.
    kb.key_event(0x04, true);
    ctrl.tick_1ms_no_dma();

    let portsc = ctrl.read_portsc(0);
    assert_eq!(
        (portsc & regs::PORTSC_PLS_MASK) >> regs::PORTSC_PLS_SHIFT,
        0,
        "expected port to resume to U0"
    );
    assert_ne!(portsc & PORTSC_PLC, 0, "expected PLC to latch on resume");

    let ev = ctrl
        .pop_pending_event()
        .expect("expected Port Status Change Event TRB");
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);
    assert_eq!(((ev.parameter >> 24) & 0xff) as u8, 1);
}
