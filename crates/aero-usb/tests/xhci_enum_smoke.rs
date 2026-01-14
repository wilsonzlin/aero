//! Driver-sequence style xHCI bring-up + enumeration smoke test.
//!
//! This test intentionally programs the xHCI controller through a realistic initialization flow:
//! - read capability registers and derive register block bases,
//! - configure DCBAA, command ring, event ring (interrupter 0),
//! - run the controller and wait for HCHalted to clear,
//! - attach a USB2 HID keyboard to root port 1 and perform the port reset/enable dance,
//! - issue core command ring commands (Enable Slot / Address Device / Evaluate Context /
//!   Configure Endpoint),
//! - execute one interrupt IN transfer via a Normal TRB and observe a Transfer Event TRB.
//!
//! The goal is to guard xHCI register + ring semantics as the controller model evolves.

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::xhci::context::CONTEXT_SIZE;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::ring::{RingCursor, RingPoll};
use aero_usb::xhci::trb::{CompletionCode as CommandCompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController, PORTSC_CCS, PORTSC_PED, PORTSC_PR};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

mod util;

use util::{Alloc, TestMemory};

fn write_u32(mem: &mut TestMemory, paddr: u64, value: u32) {
    mem.write_u32(paddr as u32, value);
}

fn write_u64(mem: &mut TestMemory, paddr: u64, value: u64) {
    mem.write_u64(paddr, value);
}

fn build_input_context_addr_device(mem: &mut TestMemory, base: u64, port: u8, ep0_mps: u16) {
    // Input Control Context (ICC) at base.
    // Drop=0, Add Slot (bit0) + EP0 (bit1).
    write_u32(mem, base + 0x00, 0);
    write_u32(mem, base + 0x04, (1 << 0) | (1 << 1));

    // Slot Context at index 1.
    let slot = base + CONTEXT_SIZE as u64;
    let speed_id = 1u32; // "full-speed" per xHCI speed ID table (value not critical here).
    let context_entries = 1u32;
    write_u32(mem, slot + 0x00, (speed_id << 20) | (context_entries << 27));
    // Root Hub Port Number is DW1 bits 16..=23.
    write_u32(mem, slot + 0x04, (port as u32) << 16);

    // EP0 Endpoint Context: Device Context index 1 => Input Context index 2.
    let ep0 = base + (2 * CONTEXT_SIZE) as u64;
    let ep_type_control = 4u32;
    write_u32(
        mem,
        ep0 + 0x04,
        (ep_type_control << 3) | ((ep0_mps as u32) << 16),
    );
}

fn build_input_context_eval_ep0_mps(mem: &mut TestMemory, base: u64, ep0_mps: u16) {
    // ICC: Add EP0 only (bit1).
    write_u32(mem, base + 0x00, 0);
    write_u32(mem, base + 0x04, 1 << 1);

    // EP0 Endpoint Context: Input Context index 2.
    let ep0 = base + (2 * CONTEXT_SIZE) as u64;
    let ep_type_control = 4u32;
    write_u32(
        mem,
        ep0 + 0x04,
        (ep_type_control << 3) | ((ep0_mps as u32) << 16),
    );
}

fn build_input_context_config_interrupt_in(
    mem: &mut TestMemory,
    base: u64,
    port: u8,
    ep1_in_mps: u16,
    tr_dequeue_ptr: u64,
) {
    // ICC: Add Slot (bit0) + EP1 IN (Device Context index 3 => bit3).
    write_u32(mem, base + 0x00, 0);
    write_u32(mem, base + 0x04, (1 << 0) | (1 << 3));

    // Slot Context: context entries up to endpoint ID 3.
    let slot = base + CONTEXT_SIZE as u64;
    let speed_id = 1u32;
    let context_entries = 3u32;
    write_u32(mem, slot + 0x00, (speed_id << 20) | (context_entries << 27));
    write_u32(mem, slot + 0x04, (port as u32) << 16);

    // EP1 IN context: Device Context index 3 => Input Context index 4.
    let ep1_in = base + (4 * CONTEXT_SIZE) as u64;
    let ep_type_interrupt_in = 7u32;
    write_u32(
        mem,
        ep1_in + 0x04,
        (ep_type_interrupt_in << 3) | ((ep1_in_mps as u32) << 16),
    );

    // TR Dequeue Pointer in DW2/DW3; bit0 = DCS.
    let tr = tr_dequeue_ptr | 1;
    write_u32(mem, ep1_in + 0x08, tr as u32);
    write_u32(mem, ep1_in + 0x0c, (tr >> 32) as u32);
}

fn wait_for_event(
    xhci: &mut XhciController,
    mem: &mut TestMemory,
    ev_cursor: &mut RingCursor,
    rt_erdp_lo: u64,
    rt_erdp_hi: u64,
    max_ticks: usize,
) -> Trb {
    for _ in 0..max_ticks {
        match ev_cursor.poll(mem, 64) {
            RingPoll::Ready(item) => {
                // Update ERDP to simulate an interrupt handler consuming the event.
                let erdp = ev_cursor.dequeue_ptr();
                xhci.mmio_write(mem, rt_erdp_lo, 4, erdp as u32);
                xhci.mmio_write(mem, rt_erdp_hi, 4, (erdp >> 32) as u32);
                return item.trb;
            }
            RingPoll::NotReady => xhci.tick_1ms_and_service_event_ring(mem),
            RingPoll::Err(err) => panic!("event ring error: {err:?}"),
        }
    }
    panic!("timed out waiting for event");
}

#[test]
fn xhci_enum_smoke_bringup_enumerate_and_interrupt_in() {
    let mut mem = TestMemory::new(0x20000);
    let mut alloc = Alloc::new(0x1000);

    let mut xhci = XhciController::new();

    // --- Capability discovery (driver-style) ---
    let cap0 = xhci.mmio_read(&mut mem, regs::REG_CAPLENGTH_HCIVERSION, 4);
    let caplength = (cap0 & 0xff) as u64;
    assert_eq!(caplength as u8, regs::CAPLENGTH_BYTES);

    let _hcsparams1 = xhci.mmio_read(&mut mem, regs::REG_HCSPARAMS1, 4);
    let _hccparams1 = xhci.mmio_read(&mut mem, regs::REG_HCCPARAMS1, 4);

    let dboff = xhci.mmio_read(&mut mem, regs::REG_DBOFF, 4) as u64 & !0x3;
    let rtsoff = xhci.mmio_read(&mut mem, regs::REG_RTSOFF, 4) as u64 & !0x1f;

    let op_base = caplength;
    let reg_usbcmd = op_base + 0x00;
    let reg_usbsts = op_base + 0x04;
    let reg_crcr_lo = op_base + 0x18;
    let reg_crcr_hi = op_base + 0x1c;
    let reg_dcbaap_lo = op_base + 0x30;
    let reg_dcbaap_hi = op_base + 0x34;
    let reg_config = op_base + 0x38;
    let reg_portsc1 = op_base + 0x400;

    // Runtime interrupter 0.
    let rt_imod = rtsoff + 0x20 + 0x04;
    let rt_iman = rtsoff + 0x20 + 0x00;
    let rt_erstsz = rtsoff + 0x20 + 0x08;
    let rt_erstba_lo = rtsoff + 0x20 + 0x10;
    let rt_erstba_hi = rtsoff + 0x20 + 0x14;
    let rt_erdp_lo = rtsoff + 0x20 + 0x18;
    let rt_erdp_hi = rtsoff + 0x20 + 0x1c;

    // --- Data structures (DCBAA + contexts + rings) ---
    let dcbaa = alloc.alloc(256 * 8, 0x40) as u64;

    let cmd_ring = alloc.alloc((TRB_LEN * 16) as u32, 0x10) as u64;
    let event_ring = alloc.alloc((TRB_LEN * 64) as u32, 0x40) as u64;
    let erst = alloc.alloc(16, 0x40) as u64;

    // ERST entry 0.
    write_u64(&mut mem, erst + 0x00, event_ring);
    write_u32(&mut mem, erst + 0x08, 64);
    write_u32(&mut mem, erst + 0x0c, 0);

    // --- Program operational registers ---
    xhci.mmio_write(&mut mem, reg_dcbaap_lo, 4, dcbaa as u32);
    xhci.mmio_write(&mut mem, reg_dcbaap_hi, 4, (dcbaa >> 32) as u32);

    // Command ring pointer + RCS=1.
    let crcr = cmd_ring | 1;
    xhci.mmio_write(&mut mem, reg_crcr_lo, 4, crcr as u32);
    xhci.mmio_write(&mut mem, reg_crcr_hi, 4, (crcr >> 32) as u32);

    // Enable one slot.
    xhci.mmio_write(&mut mem, reg_config, 4, 1);

    // --- Configure interrupter 0 event ring ---
    xhci.mmio_write(&mut mem, rt_imod, 4, 0);
    xhci.mmio_write(&mut mem, rt_erstsz, 4, 1);
    xhci.mmio_write(&mut mem, rt_erstba_lo, 4, erst as u32);
    xhci.mmio_write(&mut mem, rt_erstba_hi, 4, (erst >> 32) as u32);
    xhci.mmio_write(&mut mem, rt_erdp_lo, 4, event_ring as u32);
    xhci.mmio_write(&mut mem, rt_erdp_hi, 4, (event_ring >> 32) as u32);
    // Enable interrupter.
    xhci.mmio_write(&mut mem, rt_iman, 4, IMAN_IE);

    // --- Run controller and wait for HCHalted to clear ---
    xhci.mmio_write(&mut mem, reg_usbcmd, 4, regs::USBCMD_RUN);
    for _ in 0..10 {
        let st = xhci.mmio_read(&mut mem, reg_usbsts, 4);
        if (st & regs::USBSTS_HCHALTED) == 0 {
            break;
        }
        xhci.tick_1ms_and_service_event_ring(&mut mem);
    }
    assert_eq!(
        xhci.mmio_read(&mut mem, reg_usbsts, 4) & regs::USBSTS_HCHALTED,
        0
    );

    // --- Attach USB2 HID keyboard to root port 1 ---
    let mut keyboard = UsbHidKeyboardHandle::new();
    xhci.attach_device(0, Box::new(keyboard.clone()));

    let mut ev_cursor = RingCursor::new(event_ring, true);

    // Tick once to surface the connect Port Status Change event.
    xhci.tick_1ms_and_service_event_ring(&mut mem);
    let ev = wait_for_event(
        &mut xhci,
        &mut mem,
        &mut ev_cursor,
        rt_erdp_lo,
        rt_erdp_hi,
        10,
    );
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);

    // --- Port reset / enable sequence ---
    let portsc_before = xhci.mmio_read(&mut mem, reg_portsc1, 4);
    assert_ne!(portsc_before & PORTSC_CCS, 0, "device should be connected");
    assert_eq!(portsc_before & PORTSC_PED, 0, "port starts disabled");

    // Assert port reset.
    xhci.mmio_write(&mut mem, reg_portsc1, 4, portsc_before | PORTSC_PR);

    // Wait for reset to complete and the port to become enabled (50ms model).
    for _ in 0..60 {
        let portsc = xhci.mmio_read(&mut mem, reg_portsc1, 4);
        if (portsc & PORTSC_PR) == 0 && (portsc & PORTSC_PED) != 0 {
            break;
        }
        xhci.tick_1ms_and_service_event_ring(&mut mem);
    }
    let portsc_after = xhci.mmio_read(&mut mem, reg_portsc1, 4);
    assert_eq!(portsc_after & PORTSC_PR, 0);
    assert_ne!(portsc_after & PORTSC_PED, 0);

    // Consume the reset-completion Port Status Change event.
    let ev = wait_for_event(
        &mut xhci,
        &mut mem,
        &mut ev_cursor,
        rt_erdp_lo,
        rt_erdp_hi,
        10,
    );
    assert_eq!(ev.trb_type(), TrbType::PortStatusChangeEvent);

    // --- Command ring: Enable Slot ---
    let mut enable_slot = Trb::default();
    enable_slot.set_cycle(true);
    enable_slot.set_trb_type(TrbType::EnableSlotCommand);
    enable_slot.write_to(&mut mem, cmd_ring);

    // Ring doorbell 0.
    xhci.mmio_write(&mut mem, dboff + 0 * 4, 4, 0);
    let ev = wait_for_event(
        &mut xhci,
        &mut mem,
        &mut ev_cursor,
        rt_erdp_lo,
        rt_erdp_hi,
        50,
    );
    assert_eq!(ev.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(
        ev.completion_code_raw(),
        CommandCompletionCode::Success.as_u8()
    );
    let slot_id = ev.slot_id();
    assert_eq!(slot_id, 1, "CONFIG.MaxSlotsEn=1 should yield slot ID 1");

    // Program DCBAA[slot_id] with an output Device Context pointer (driver-style).
    let dev_ctx = alloc.alloc(32 * CONTEXT_SIZE as u32, 0x40) as u64;
    write_u64(&mut mem, dcbaa + u64::from(slot_id) * 8, dev_ctx);

    // --- Address Device (EP0 MPS=8) ---
    let input_addr = alloc.alloc((33 * CONTEXT_SIZE) as u32, 0x40) as u64;
    build_input_context_addr_device(&mut mem, input_addr, 1, 8);

    let mut addr_dev = Trb::default();
    addr_dev.parameter = input_addr;
    addr_dev.set_cycle(true);
    addr_dev.set_trb_type(TrbType::AddressDeviceCommand);
    addr_dev.set_slot_id(slot_id);
    addr_dev.write_to(&mut mem, cmd_ring + TRB_LEN as u64);

    xhci.mmio_write(&mut mem, dboff + 0 * 4, 4, 0);
    let ev = wait_for_event(
        &mut xhci,
        &mut mem,
        &mut ev_cursor,
        rt_erdp_lo,
        rt_erdp_hi,
        50,
    );
    assert_eq!(ev.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(
        ev.completion_code_raw(),
        CommandCompletionCode::Success.as_u8()
    );
    assert_eq!(ev.slot_id(), slot_id);

    // Read the device descriptor from the model and use it to update EP0 MPS.
    let dev_desc = match keyboard.handle_control_request(
        SetupPacket {
            bm_request_type: 0x80, // DeviceToHost | Standard | Device
            b_request: 0x06,       // GET_DESCRIPTOR
            w_value: 0x0100,
            w_index: 0,
            w_length: 18,
        },
        None,
    ) {
        ControlResponse::Data(data) => data,
        other => panic!("expected device descriptor, got {other:?}"),
    };
    let mps0 = dev_desc[7];
    assert_eq!(mps0, 0x40, "keyboard bMaxPacketSize0 should be 64");

    // --- Evaluate Context: update EP0 MPS to 64 ---
    let input_eval = alloc.alloc((33 * CONTEXT_SIZE) as u32, 0x40) as u64;
    build_input_context_eval_ep0_mps(&mut mem, input_eval, u16::from(mps0));

    let mut eval_ctx = Trb::default();
    eval_ctx.parameter = input_eval;
    eval_ctx.set_cycle(true);
    eval_ctx.set_trb_type(TrbType::EvaluateContextCommand);
    eval_ctx.set_slot_id(slot_id);
    eval_ctx.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);

    xhci.mmio_write(&mut mem, dboff + 0 * 4, 4, 0);
    let ev = wait_for_event(
        &mut xhci,
        &mut mem,
        &mut ev_cursor,
        rt_erdp_lo,
        rt_erdp_hi,
        50,
    );
    assert_eq!(ev.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(
        ev.completion_code_raw(),
        CommandCompletionCode::Success.as_u8()
    );
    assert_eq!(ev.slot_id(), slot_id);

    // --- Configure Endpoint: interrupt IN endpoint 1 (0x81) ---
    let ep1_ring = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let dma_buf = alloc.alloc(64, 0x10) as u64;

    let input_cfg = alloc.alloc((33 * CONTEXT_SIZE) as u32, 0x40) as u64;
    build_input_context_config_interrupt_in(&mut mem, input_cfg, 1, 8, ep1_ring);

    let mut cfg_ep = Trb::default();
    cfg_ep.parameter = input_cfg;
    cfg_ep.set_cycle(true);
    cfg_ep.set_trb_type(TrbType::ConfigureEndpointCommand);
    cfg_ep.set_slot_id(slot_id);
    cfg_ep.write_to(&mut mem, cmd_ring + 3 * TRB_LEN as u64);

    xhci.mmio_write(&mut mem, dboff + 0 * 4, 4, 0);
    let ev = wait_for_event(
        &mut xhci,
        &mut mem,
        &mut ev_cursor,
        rt_erdp_lo,
        rt_erdp_hi,
        50,
    );
    assert_eq!(ev.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(
        ev.completion_code_raw(),
        CommandCompletionCode::Success.as_u8()
    );
    assert_eq!(ev.slot_id(), slot_id);

    // --- Prepare interrupt IN report ---
    assert_eq!(
        keyboard.handle_control_request(
            SetupPacket {
                bm_request_type: 0x00, // HostToDevice | Standard | Device
                b_request: 0x09,       // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
            None,
        ),
        ControlResponse::Ack
    );
    keyboard.key_event(0x04, true); // 'A'

    // Transfer ring: Normal TRB + Link TRB back to start (toggle cycle).
    let mut normal = Trb::default();
    normal.parameter = dma_buf;
    normal.status = 8; // TRB Transfer Length.
    normal.set_cycle(true);
    normal.set_trb_type(TrbType::Normal);
    // IOC bit (bit 5) in the control dword.
    normal.control |= 1 << 5;
    normal.write_to(&mut mem, ep1_ring);

    let mut link = Trb::default();
    link.parameter = ep1_ring;
    link.set_cycle(true);
    link.set_trb_type(TrbType::Link);
    link.set_link_toggle_cycle(true);
    link.write_to(&mut mem, ep1_ring + TRB_LEN as u64);

    // Ring slot doorbell for endpoint ID 3 (EP1 IN).
    xhci.mmio_write(&mut mem, dboff + (slot_id as u64) * 4, 4, 3);

    // Wait for a Transfer Event.
    let ev = wait_for_event(
        &mut xhci,
        &mut mem,
        &mut ev_cursor,
        rt_erdp_lo,
        rt_erdp_hi,
        50,
    );
    assert_eq!(ev.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev.completion_code_raw(), 1);
    assert_eq!(ev.slot_id(), slot_id);
    assert_eq!(ev.endpoint_id(), 3);

    // Transfer Event status lower bits carry residual bytes; ensure some data transferred.
    let residual = ev.status & 0x00ff_ffff;
    let transferred = 8u32.saturating_sub(residual);
    assert!(transferred != 0, "expected non-zero transferred length");

    // Verify the HID boot keyboard report was DMA'd into guest memory.
    let mut report = [0u8; 8];
    mem.read_physical(dma_buf, &mut report);
    assert_eq!(report, [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]);
}
