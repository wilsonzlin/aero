mod util;

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::transfer::{write_trb, CompletionCode, Trb, TrbType, XhciTransferExecutor};
use aero_usb::xhci::{regs, CommandCompletionCode, XhciController};
use aero_usb::{
    ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult,
};

use util::{Alloc, TestMemory};

const RING_BASE: u64 = 0x1000;
const NORMAL_TRB_ADDR: u64 = RING_BASE;
const LINK_TRB_ADDR: u64 = RING_BASE + 0x10;

const DATA_BUF: u64 = 0x2000;

struct TestMemBus {
    mem: Vec<u8>,
}

impl TestMemBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn slice(&self, range: Range<usize>) -> &[u8] {
        &self.mem[range]
    }
}

impl MemoryBus for TestMemBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        buf.copy_from_slice(&self.mem[start..start + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        self.mem[start..start + buf.len()].copy_from_slice(buf);
    }
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

fn make_link_trb(target: u64, cycle: bool, toggle_cycle: bool) -> Trb {
    let mut dword3 = 0u32;
    if cycle {
        dword3 |= 1;
    }
    if toggle_cycle {
        dword3 |= 1 << 1;
    }
    dword3 |= (u32::from(TrbType::Link.raw())) << 10;
    Trb::from_dwords(target as u32, (target >> 32) as u32, 0, dword3)
}

#[derive(Clone, Debug)]
struct TestOutDevice {
    log: Rc<RefCell<Vec<(u8, Vec<u8>)>>>,
}

impl TestOutDevice {
    fn new() -> Self {
        Self {
            log: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl UsbDeviceModel for TestOutDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        self.log.borrow_mut().push((ep, data.to_vec()));
        UsbOutResult::Ack
    }
}

#[test]
fn xhci_interrupt_in_completes_only_when_report_available() {
    let keyboard = UsbHidKeyboardHandle::new();

    let mut xhci = XhciTransferExecutor::new(Box::new(keyboard.clone()));

    // Configure the HID device so it is allowed to emit interrupt reports.
    let setup = SetupPacket {
        bm_request_type: 0x00, // HostToDevice | Standard | Device
        b_request: 0x09,       // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    assert_eq!(
        xhci.device_mut().handle_control_request(setup, None),
        ControlResponse::Ack
    );

    let mut mem = TestMemBus::new(0x10000);

    // A tiny transfer ring with a single Normal TRB and a Link TRB back to the start.
    write_trb(
        &mut mem,
        NORMAL_TRB_ADDR,
        make_normal_trb(DATA_BUF, 8, true, true),
    );
    write_trb(
        &mut mem,
        LINK_TRB_ADDR,
        make_link_trb(RING_BASE, true, true),
    );

    // Point the executor at the ring and poll once with no pending report.
    xhci.add_endpoint(0x81, RING_BASE);
    xhci.tick_1ms(&mut mem);

    assert!(xhci.take_events().is_empty());
    assert_eq!(
        xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        RING_BASE,
        "NAK should leave the TRB pending"
    );

    // The device hasn't produced a report yet, so the guest buffer should be untouched.
    assert_eq!(mem.slice(DATA_BUF as usize..DATA_BUF as usize + 8), &[0; 8]);

    // Inject a keypress (usage 0x04 = 'A') and tick again; now the IN TD should complete.
    keyboard.key_event(0x04, true);
    xhci.tick_1ms(&mut mem);

    let events = xhci.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x81);
    assert_eq!(events[0].completion_code, CompletionCode::Success);
    assert_eq!(events[0].residual, 0);

    // Verify the USB HID boot keyboard report bytes landed in guest memory.
    assert_eq!(
        mem.slice(DATA_BUF as usize..DATA_BUF as usize + 8),
        &[0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]
    );
}

#[test]
fn xhci_interrupt_out_dmas_and_completes_normal_trb() {
    let dev = TestOutDevice::new();
    let log = dev.log.clone();

    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    let mut mem = TestMemBus::new(0x10000);
    mem.write_physical(DATA_BUF, &[0xde, 0xad, 0xbe, 0xef]);

    write_trb(
        &mut mem,
        NORMAL_TRB_ADDR,
        make_normal_trb(DATA_BUF, 4, true, true),
    );

    xhci.add_endpoint(0x01, RING_BASE);
    xhci.tick_1ms(&mut mem);

    let events = xhci.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x01);
    assert_eq!(events[0].completion_code, CompletionCode::Success);
    assert_eq!(events[0].residual, 0);

    let received = log.borrow().clone();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].0, 0x01);
    assert_eq!(received[0].1, vec![0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn xhci_stall_marks_endpoint_halted_and_completes_with_stall_error() {
    let keyboard = UsbHidKeyboardHandle::new();
    let mut xhci = XhciTransferExecutor::new(Box::new(keyboard));

    const BUF2: u64 = 0x3000;
    let mut mem = TestMemBus::new(0x10000);

    // Two Normal TRBs in sequence. The first will STALL because the keyboard only implements EP 0x81.
    write_trb(
        &mut mem,
        RING_BASE,
        make_normal_trb(DATA_BUF, 8, true, false),
    );
    write_trb(
        &mut mem,
        RING_BASE + 0x10,
        make_normal_trb(BUF2, 8, true, true),
    );

    xhci.add_endpoint(0x82, RING_BASE);
    xhci.tick_1ms(&mut mem);

    let events = xhci.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x82);
    assert_eq!(events[0].completion_code, CompletionCode::StallError);
    assert_eq!(events[0].residual, 8);
    assert!(xhci.endpoint_state(0x82).unwrap().halted);

    // Further ticks must not process additional TRBs while halted.
    xhci.tick_1ms(&mut mem);
    assert!(xhci.take_events().is_empty());
    assert_eq!(mem.slice(BUF2 as usize..BUF2 as usize + 8), &[0; 8]);
}

fn write_erst_entry(mem: &mut TestMemory, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[derive(Clone)]
struct NakUntilReadyInterruptIn {
    ready: Rc<Cell<bool>>,
    payload: Rc<RefCell<Vec<u8>>>,
}

impl NakUntilReadyInterruptIn {
    fn new(ready: Rc<Cell<bool>>, payload: Vec<u8>) -> Self {
        Self {
            ready,
            payload: Rc::new(RefCell::new(payload)),
        }
    }
}

impl UsbDeviceModel for NakUntilReadyInterruptIn {
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
        if !self.ready.get() {
            return UsbInResult::Nak;
        }
        let mut data = self.payload.borrow().clone();
        data.truncate(max_len);
        UsbInResult::Data(data)
    }

    fn handle_out_transfer(&mut self, _ep: u8, _data: &[u8]) -> UsbOutResult {
        UsbOutResult::Stall
    }
}

#[test]
fn xhci_interrupt_in_nak_is_retried_on_tick_without_additional_doorbells() {
    let mut mem = TestMemory::new(0x40_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;
    let erstba = alloc.alloc(16, 0x10) as u64;
    let event_ring_base = alloc.alloc(16 * 16, 0x10) as u64;
    // Transfer ring with Normal TRB + Link TRB back to the start.
    let transfer_ring_base = alloc.alloc(2 * 16, 0x10) as u64;
    let buf_addr = alloc.alloc(8, 0x10) as u64;

    write_erst_entry(&mut mem, erstba, event_ring_base, 16);

    let ready = Rc::new(Cell::new(false));
    let device = NakUntilReadyInterruptIn::new(ready.clone(), vec![0xaa, 0xbb, 0xcc]);

    let mut xhci = XhciController::with_port_count(1);
    xhci.set_dcbaap(dcbaa);
    xhci.attach_device(0, Box::new(device));

    // Drain any PSC events so the event ring only contains the transfer completion.
    while xhci.pop_pending_event().is_some() {}

    let completion = xhci.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let completion = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    // Configure interrupter 0 to deliver events into our guest event ring.
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_LO, 4, erstba as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERSTBA_HI, 4, (erstba >> 32) as u32);
    xhci.mmio_write(&mut mem, regs::REG_INTR0_ERDP_LO, 4, event_ring_base as u32);
    xhci.mmio_write(
        &mut mem,
        regs::REG_INTR0_ERDP_HI,
        4,
        (event_ring_base >> 32) as u32,
    );
    xhci.mmio_write(&mut mem, regs::REG_INTR0_IMAN, 4, IMAN_IE);

    // Interrupt IN endpoint 1 uses DCI=3.
    const ENDPOINT_ID: u8 = 3;
    xhci.set_endpoint_ring(slot_id, ENDPOINT_ID, transfer_ring_base, true);

    // Normal TRB requesting 3 bytes.
    write_trb(
        &mut mem,
        transfer_ring_base,
        make_normal_trb(buf_addr, 3, true, true),
    );
    write_trb(
        &mut mem,
        transfer_ring_base + 16,
        make_link_trb(transfer_ring_base, true, true),
    );

    let doorbell_offset = u64::from(regs::DBOFF_VALUE)
        + u64::from(slot_id) * u64::from(regs::doorbell::DOORBELL_STRIDE);
    xhci.mmio_write(&mut mem, doorbell_offset, 4, u32::from(ENDPOINT_ID));

    // First tick: transfer is attempted and should NAK. Buffer unchanged and no transfer event.
    xhci.tick_1ms(&mut mem);
    assert_eq!(
        &mem.data[buf_addr as usize..buf_addr as usize + 3],
        &[0, 0, 0]
    );
    let ev0 = Trb::read_from(&mut mem, event_ring_base);
    assert_ne!(
        ev0.trb_type(),
        TrbType::TransferEvent,
        "unexpected transfer event before device is ready"
    );

    // Make data available and advance time: controller should retry and complete the transfer.
    ready.set(true);
    xhci.tick_1ms(&mut mem);

    assert_eq!(
        &mem.data[buf_addr as usize..buf_addr as usize + 3],
        &[0xaa, 0xbb, 0xcc]
    );

    let ev = Trb::read_from(&mut mem, event_ring_base);
    assert_eq!(ev.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev.slot_id(), slot_id);
    assert_eq!(ev.endpoint_id(), ENDPOINT_ID);
    assert_eq!(ev.parameter & !0x0f, transfer_ring_base);
    assert_eq!(ev.completion_code_raw(), CompletionCode::Success.raw());
}

#[test]
fn xhci_tick_has_bounded_work_budget_for_large_transfer_backlogs() {
    let mut mem = TestMemory::new(0x200_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;

    let transfers: usize = 1000;
    let transfer_ring_base = alloc
        .alloc((transfers as u32) * 16, 0x10)
        .into();
    let buf_base = alloc.alloc((transfers as u32) * 8, 0x10) as u64;

    #[derive(Clone)]
    struct AlwaysData;

    impl UsbDeviceModel for AlwaysData {
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
            let mut data = vec![0x5a, 0x5b];
            data.truncate(max_len);
            UsbInResult::Data(data)
        }
    }

    let mut xhci = XhciController::with_port_count(1);
    xhci.set_dcbaap(dcbaa);
    xhci.attach_device(0, Box::new(AlwaysData));

    while xhci.pop_pending_event().is_some() {}

    let completion = xhci.enable_slot(&mut mem);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);
    let slot_id = completion.slot_id;

    let mut slot_ctx = SlotContext::default();
    slot_ctx.set_root_hub_port_number(1);
    let completion = xhci.address_device(slot_id, slot_ctx);
    assert_eq!(completion.completion_code, CommandCompletionCode::Success);

    // Interrupt IN endpoint 1 uses DCI=3.
    const ENDPOINT_ID: u8 = 3;
    xhci.set_endpoint_ring(slot_id, ENDPOINT_ID, transfer_ring_base, true);

    for i in 0..transfers {
        let buf = buf_base + (i as u64) * 8;
        write_trb(
            &mut mem,
            transfer_ring_base + (i as u64) * 16,
            make_normal_trb(buf, 2, true, false),
        );
    }

    let doorbell_offset = u64::from(regs::DBOFF_VALUE)
        + u64::from(slot_id) * u64::from(regs::doorbell::DOORBELL_STRIDE);
    xhci.mmio_write(&mut mem, doorbell_offset, 4, u32::from(ENDPOINT_ID));

    xhci.tick_1ms(&mut mem);

    let mut completed1 = 0usize;
    for i in 0..transfers {
        let buf = (buf_base + (i as u64) * 8) as usize;
        if mem.data[buf] == 0x5a && mem.data[buf + 1] == 0x5b {
            completed1 += 1;
        }
    }

    assert!(completed1 > 0, "expected at least some progress");
    assert!(
        completed1 < transfers,
        "tick_1ms processed an unbounded backlog (completed {completed1} of {transfers})"
    );

    // Budget exhaustion should keep the endpoint active so the backlog continues without another
    // doorbell.
    xhci.tick_1ms(&mut mem);

    let mut completed2 = 0usize;
    for i in 0..transfers {
        let buf = (buf_base + (i as u64) * 8) as usize;
        if mem.data[buf] == 0x5a && mem.data[buf + 1] == 0x5b {
            completed2 += 1;
        }
    }

    assert!(
        completed2 > completed1,
        "expected more progress without additional doorbells"
    );
}
