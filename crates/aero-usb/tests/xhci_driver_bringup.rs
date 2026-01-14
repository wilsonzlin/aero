//! End-to-end xHCI "guest driver bring-up" sequence test.
//!
//! The goal is to exercise:
//! - MMIO programming of an [`aero_usb::xhci::XhciController`]
//! - Command ring consumption + Command Completion events
//! - Doorbells + Transfer ring execution + Transfer events
//! - Event ring cycle-bit wrap behaviour
//! - IRQ assertion + RW1C clearing via USBSTS

use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType};
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::{regs, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult};

mod util;

use util::{Alloc, TestMemory};

#[derive(Debug)]
struct FixedInDevice {
    data: Vec<u8>,
    sent: bool,
}

impl FixedInDevice {
    fn new(data: Vec<u8>) -> Self {
        Self { data, sent: false }
    }
}

impl UsbDeviceModel for FixedInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        if ep_addr != 0x81 {
            return UsbInResult::Stall;
        }
        if self.sent {
            return UsbInResult::Nak;
        }
        self.sent = true;
        let mut out = self.data.clone();
        if out.len() > max_len {
            out.truncate(max_len);
        }
        UsbInResult::Data(out)
    }
}

fn make_command_trb(trb_type: TrbType, pointer: u64, slot_id: u8, cycle: bool) -> Trb {
    let mut trb = Trb::new(pointer, 0, 0);
    trb.set_trb_type(trb_type);
    trb.set_slot_id(slot_id);
    trb.set_cycle(cycle);
    trb
}

fn make_link_trb(target: u64, cycle: bool, toggle_cycle: bool) -> Trb {
    let mut trb = Trb::new(target & !0x0f, 0, 0);
    trb.set_trb_type(TrbType::Link);
    trb.set_cycle(cycle);
    trb.set_link_toggle_cycle(toggle_cycle);
    trb
}

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, ioc: bool) -> Trb {
    let mut dword3 = 0u32;
    if cycle {
        dword3 |= 1;
    }
    if ioc {
        dword3 |= 1 << 5;
    }
    dword3 |= (u32::from(TrbType::Normal.raw())) << 10;
    Trb::from_dwords(
        buf_ptr as u32,
        (buf_ptr >> 32) as u32,
        len & 0x1ffff,
        dword3,
    )
}

#[test]
fn xhci_driver_bringup_mmio_rings_and_transfer() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    // ---- Guest memory allocations ----

    // DCBAA (Device Context Base Address Array): 256 entries * 8 bytes = 2048 bytes.
    let dcbaa = alloc.alloc(0x800, 0x40) as u64;
    let dev_ctx = alloc.alloc(0x400, 0x40) as u64;

    // Input contexts for Address Device and Configure Endpoint.
    let input_ctx_addr = alloc.alloc(0x200, 0x40) as u64;
    let input_ctx_cfg = alloc.alloc(0x200, 0x40) as u64;

    // Command ring: 3 commands + link TRB.
    let cmd_ring = alloc.alloc(0x40, 0x10) as u64;

    // Event ring + ERST (single segment, 4 TRBs => force wrap once).
    let event_ring = alloc.alloc(0x40, 0x10) as u64;
    let erst = alloc.alloc(0x40, 0x40) as u64;

    // Transfer ring for endpoint 0x81 (Normal + Link).
    let tr_ring = alloc.alloc(0x20, 0x10) as u64;
    let dma_buf = alloc.alloc(0x10, 0x10) as u64;

    // Seed DMA buffer with sentinel bytes.
    mem.write_bytes(dma_buf, &[0xa5; 8]);

    // ---- Build ERST[0] ----
    // DW0-1: segment base address.
    // DW2: segment size (TRBs) in bits 0..=15.
    mem.write_u64(erst, event_ring);
    MemoryBus::write_u32(&mut mem, erst + 8, 4); // 4 TRBs

    // ---- Build input context for Address Device ----
    // Input Control Context (ICC): Drop=0, Add = Slot + EP0.
    MemoryBus::write_u32(&mut mem, input_ctx_addr + 0x00, 0);
    MemoryBus::write_u32(&mut mem, input_ctx_addr + 0x04, (1 << 0) | (1 << 1));
    // Slot Context Root Hub Port Number = 1 (port 0 in `attach_device`).
    MemoryBus::write_u32(&mut mem, input_ctx_addr + 0x20 + 4, 1 << 16);

    // ---- Build input context for Configure Endpoint ----
    // Drop=0, Add = Slot + EP0 + EP1 IN (device context index 3).
    MemoryBus::write_u32(&mut mem, input_ctx_cfg + 0x00, 0);
    MemoryBus::write_u32(&mut mem, input_ctx_cfg + 0x04, (1 << 0) | (1 << 1) | (1 << 3));
    MemoryBus::write_u32(&mut mem, input_ctx_cfg + 0x20 + 4, 1 << 16);

    // Endpoint 1 IN context lives at Input Context index 4 (device index 3 + 1):
    // offset = 0x20 * 4 = 0x80.
    let ep1in_ctx = input_ctx_cfg + 0x80;
    // DW1: EP Type (bits 3..=5) + Max Packet Size (bits 16..=31).
    let ep_type_interrupt_in = 7u32;
    let max_packet_size = 8u32;
    MemoryBus::write_u32(
        &mut mem,
        ep1in_ctx + 4,
        (ep_type_interrupt_in << 3) | (max_packet_size << 16),
    );
    // DW2-3: TR Dequeue Pointer + DCS in bit0.
    let tr_dequeue_raw = tr_ring | 1;
    MemoryBus::write_u32(&mut mem, ep1in_ctx + 8, tr_dequeue_raw as u32);
    MemoryBus::write_u32(&mut mem, ep1in_ctx + 12, (tr_dequeue_raw >> 32) as u32);

    // ---- Build command ring ----
    // Cycle state starts at 1. Make only the Enable Slot command visible initially; the remaining
    // TRBs are written with cycle=0 so the controller stops after processing TRB0.
    make_command_trb(TrbType::EnableSlotCommand, 0, 0, true).write_to(&mut mem, cmd_ring);
    make_command_trb(TrbType::NoOpCommand, 0, 0, false).write_to(&mut mem, cmd_ring + 0x10);
    make_command_trb(TrbType::NoOpCommand, 0, 0, false).write_to(&mut mem, cmd_ring + 0x20);
    make_link_trb(cmd_ring, false, true).write_to(&mut mem, cmd_ring + 0x30);

    // ---- Build transfer ring TD (cycle state = 1) ----
    let payload = [0x11, 0x22, 0x33, 0x44];
    make_normal_trb(dma_buf, payload.len() as u32, true, true).write_to(&mut mem, tr_ring);
    make_link_trb(tr_ring, true, true).write_to(&mut mem, tr_ring + 0x10);

    // ---- Controller setup ----
    let mut xhci = XhciController::new();
    xhci.attach_device(0, Box::new(FixedInDevice::new(payload.to_vec())));
    // `attach_device` queues a Port Status Change Event TRB. This test focuses on
    // command/transfer events; drain host-side pending events so the first IRQ we observe is
    // deterministic.
    while xhci.pop_pending_event().is_some() {}
    xhci.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);

    // Verify CAPLENGTH is sane and fetch DBOFF/RTSOFF like a real guest driver would.
    let caplen_hciver = xhci.mmio_read_u32(&mut mem, regs::REG_CAPLENGTH_HCIVERSION);
    assert_eq!(
        (caplen_hciver & 0xff) as u8,
        regs::CAPLENGTH_BYTES,
        "CAPLENGTH mismatch"
    );
    let dboff = xhci.mmio_read_u32(&mut mem, regs::cap::DBOFF as u64);
    let rtsoff = xhci.mmio_read_u32(&mut mem, regs::cap::RTSOFF as u64);

    // Program operational registers.
    xhci.mmio_write(&mut mem, regs::REG_DCBAAP_LO, 4, dcbaa as u32);
    xhci.mmio_write(&mut mem, regs::REG_DCBAAP_HI, 4, (dcbaa >> 32) as u32);

    // CRCR: pointer (aligned) + initial cycle state = 1.
    xhci.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, (cmd_ring as u32) | 1);
    xhci.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, (cmd_ring >> 32) as u32);

    // Enable a single slot.
    xhci.mmio_write(&mut mem, regs::REG_CONFIG, 4, 1);

    // Program interrupter 0 event ring.
    let intr0 = (rtsoff as u64) + 0x20;
    xhci.mmio_write(&mut mem, intr0 + 0x08, 4, 1); // ERSTSZ = 1 entry
    xhci.mmio_write(&mut mem, intr0 + 0x10, 4, erst as u32);
    xhci.mmio_write(&mut mem, intr0 + 0x14, 4, (erst >> 32) as u32);
    xhci.mmio_write(&mut mem, intr0 + 0x18, 4, event_ring as u32);
    xhci.mmio_write(&mut mem, intr0 + 0x1c, 4, (event_ring >> 32) as u32);
    // Enable interrupter 0.
    xhci.mmio_write(&mut mem, intr0 + 0x00, 4, IMAN_IE);

    // Start controller (RUN). This triggers a small DMA read + sets USBSTS.EINT; clear it so the
    // subsequent assertions are tied to command/transfer events.
    xhci.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);
    assert!(xhci.irq_level(), "RUN should assert an IRQ due to dma_on_run");
    xhci.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);
    assert!(!xhci.irq_level(), "USBSTS RW1C should clear IRQ");

    // ---- Ring doorbell 0 (Enable Slot) ----
    xhci.mmio_write(&mut mem, dboff as u64, 4, 0);
    xhci.service_event_ring(&mut mem);

    let ev0 = Trb::read_from(&mut mem, event_ring + 0x00);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert!(ev0.cycle(), "event[0] should use initial event-ring cycle state (1)");
    assert_eq!(ev0.pointer(), cmd_ring + 0x00);
    assert_eq!(ev0.completion_code_raw(), CompletionCode::Success.as_u8());
    let slot_id = ev0.slot_id();
    assert_eq!(slot_id, 1, "model should allocate slot 1 for the first device");

    assert!(xhci.irq_level(), "Enable Slot completion should assert IRQ");
    xhci.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);
    assert!(!xhci.irq_level(), "IRQ should be clearable via USBSTS RW1C");

    // Install DCBAA[slot_id] -> Device Context pointer (guest responsibility, after Enable Slot).
    mem.write_u64(dcbaa + u64::from(slot_id) * 8, dev_ctx);

    // Make Address Device + Configure Endpoint visible (cycle=1), then ring doorbell again.
    make_command_trb(TrbType::AddressDeviceCommand, input_ctx_addr, slot_id, true)
        .write_to(&mut mem, cmd_ring + 0x10);
    make_command_trb(TrbType::ConfigureEndpointCommand, input_ctx_cfg, slot_id, true)
        .write_to(&mut mem, cmd_ring + 0x20);
    make_link_trb(cmd_ring, true, true).write_to(&mut mem, cmd_ring + 0x30);

    xhci.mmio_write(&mut mem, dboff as u64, 4, 0);
    xhci.service_event_ring(&mut mem);

    let ev1 = Trb::read_from(&mut mem, event_ring + 0x10);
    let ev2 = Trb::read_from(&mut mem, event_ring + 0x20);
    for (idx, ev) in [(1usize, ev1), (2, ev2)] {
        assert_eq!(
            ev.trb_type(),
            TrbType::CommandCompletionEvent,
            "event[{idx}] type"
        );
        assert!(
            ev.cycle(),
            "event[{idx}] should use initial event-ring cycle state (1)"
        );
        assert_eq!(
            ev.completion_code_raw(),
            CompletionCode::Success.as_u8(),
            "event[{idx}] completion code"
        );
    }
    assert_eq!(ev1.pointer(), cmd_ring + 0x10);
    assert_eq!(ev2.pointer(), cmd_ring + 0x20);

    assert!(xhci.irq_level(), "command completions should assert IRQ");
    xhci.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);
    assert!(!xhci.irq_level(), "IRQ should be clearable via USBSTS RW1C");

    // ---- Validate command-ring cycle-bit wrap ----
    // The Link TRB toggles the consumer cycle state. Overwrite TRB0 with cycle=0 and ensure it is
    // consumed on a subsequent doorbell ring.
    make_command_trb(TrbType::NoOpCommand, 0, 0, false).write_to(&mut mem, cmd_ring);
    xhci.mmio_write(&mut mem, dboff as u64, 4, 0);
    xhci.service_event_ring(&mut mem);

    let ev3 = Trb::read_from(&mut mem, event_ring + 0x30);
    assert_eq!(ev3.trb_type(), TrbType::CommandCompletionEvent);
    assert!(ev3.cycle(), "event[3] should still be in cycle state 1");
    assert_eq!(ev3.pointer(), cmd_ring + 0x00);
    assert_eq!(ev3.completion_code_raw(), CompletionCode::Success.as_u8());

    assert!(xhci.irq_level(), "second command completion should assert IRQ");
    xhci.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);
    assert!(!xhci.irq_level());

    // Tell the controller we consumed all 4 events by writing ERDP back to the same address.
    // When the ring was full, this should toggle the consumer cycle state and allow new events.
    xhci.mmio_write(&mut mem, intr0 + 0x18, 4, event_ring as u32);
    xhci.mmio_write(&mut mem, intr0 + 0x1c, 4, (event_ring >> 32) as u32);

    // ---- Ring endpoint doorbell (slot 1, target EP1 IN = endpoint ID 3) ----
    xhci.mmio_write(&mut mem, dboff as u64 + u64::from(slot_id) * 4, 4, 3);
    xhci.tick(&mut mem);
    xhci.service_event_ring(&mut mem);

    // The next event write should wrap the 4-TRB event ring, toggling cycle to 0 and overwriting
    // entry 0.
    let tev = Trb::read_from(&mut mem, event_ring + 0x00);
    assert_eq!(tev.trb_type(), TrbType::TransferEvent);
    assert!(
        !tev.cycle(),
        "transfer event should be written after event-ring wrap with cycle=0"
    );
    assert_eq!(tev.pointer(), tr_ring);
    assert_eq!(tev.endpoint_id(), 3, "endpoint id should be 3 (EP1 IN)");
    assert_eq!(tev.slot_id(), slot_id, "slot id should be 1");
    assert_eq!(
        tev.completion_code_raw(),
        1,
        "transfer completion code should be Success(1)"
    );

    let mut got = [0u8; 4];
    mem.read_bytes(dma_buf, &mut got);
    assert_eq!(got, payload, "DMA buffer should contain IN payload");

    assert!(xhci.irq_level(), "transfer event should assert IRQ");
    xhci.mmio_write(&mut mem, regs::REG_USBSTS, 4, regs::USBSTS_EINT);
    assert!(!xhci.irq_level());
}
