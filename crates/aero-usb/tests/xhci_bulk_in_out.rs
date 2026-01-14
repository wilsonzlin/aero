use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use aero_usb::xhci::transfer::{
    write_trb, CompletionCode, Trb, TrbType, TransferEvent, XhciTransferExecutor,
};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult};

mod util;

use util::{Alloc, TestMemory};

const TRB_LEN: u64 = 0x10;

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, chain: bool, ioc: bool) -> Trb {
    let mut trb = Trb::new(buf_ptr, len & Trb::STATUS_TRANSFER_LEN_MASK, 0);
    trb.set_trb_type(TrbType::Normal);
    trb.set_cycle(cycle);
    if chain {
        trb.control |= Trb::CONTROL_CHAIN_BIT;
    }
    if ioc {
        trb.control |= Trb::CONTROL_IOC_BIT;
    }
    trb
}

fn make_link_trb(target: u64, cycle: bool, toggle_cycle: bool) -> Trb {
    let mut trb = Trb::new(target & !0x0f, 0, 0);
    trb.set_trb_type(TrbType::Link);
    trb.set_cycle(cycle);
    trb.set_link_toggle_cycle(toggle_cycle);
    trb
}

#[derive(Clone, Debug)]
struct BulkEndpointDevice {
    in_queue: Rc<RefCell<VecDeque<Vec<u8>>>>,
    out_received: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl BulkEndpointDevice {
    fn new(
        in_queue: Rc<RefCell<VecDeque<Vec<u8>>>>,
        out_received: Rc<RefCell<Vec<Vec<u8>>>>,
    ) -> Self {
        Self {
            in_queue,
            out_received,
        }
    }
}

impl UsbDeviceModel for BulkEndpointDevice {
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

        let Some(mut data) = self.in_queue.borrow_mut().pop_front() else {
            return UsbInResult::Nak;
        };
        if data.len() > max_len {
            data.truncate(max_len);
        }
        UsbInResult::Data(data)
    }

    fn handle_out_transfer(&mut self, ep_addr: u8, data: &[u8]) -> UsbOutResult {
        if ep_addr != 0x01 {
            return UsbOutResult::Stall;
        }

        self.out_received.borrow_mut().push(data.to_vec());
        UsbOutResult::Ack
    }
}

#[derive(Clone, Debug)]
struct BulkOutNakOnceDevice {
    out_received: Rc<RefCell<Vec<Vec<u8>>>>,
    nak_next: Rc<Cell<bool>>,
}

impl BulkOutNakOnceDevice {
    fn new(out_received: Rc<RefCell<Vec<Vec<u8>>>>) -> Self {
        Self {
            out_received,
            nak_next: Rc::new(Cell::new(true)),
        }
    }
}

impl UsbDeviceModel for BulkOutNakOnceDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, _ep_addr: u8, _max_len: usize) -> UsbInResult {
        UsbInResult::Stall
    }

    fn handle_out_transfer(&mut self, ep_addr: u8, data: &[u8]) -> UsbOutResult {
        if ep_addr != 0x01 {
            return UsbOutResult::Stall;
        }

        if self.nak_next.get() {
            self.nak_next.set(false);
            return UsbOutResult::Nak;
        }

        self.out_received.borrow_mut().push(data.to_vec());
        UsbOutResult::Ack
    }
}

#[derive(Clone, Debug)]
struct TimeoutInDevice;

impl UsbDeviceModel for TimeoutInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, _max_len: usize) -> UsbInResult {
        assert_eq!(ep_addr, 0x81);
        UsbInResult::Timeout
    }
}

#[derive(Clone, Debug)]
struct TimeoutOnceInDevice {
    call_count: Rc<Cell<u32>>,
}

impl TimeoutOnceInDevice {
    fn new() -> Self {
        Self {
            call_count: Rc::new(Cell::new(0)),
        }
    }
}

impl UsbDeviceModel for TimeoutOnceInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        assert_eq!(ep_addr, 0x81);
        let n = self.call_count.get();
        self.call_count.set(n + 1);
        if n == 0 {
            UsbInResult::Timeout
        } else {
            let mut data = vec![1u8, 2, 3, 4];
            if data.len() > max_len {
                data.truncate(max_len);
            }
            UsbInResult::Data(data)
        }
    }
}

#[derive(Clone, Debug)]
struct TimeoutOnceOutDevice {
    call_count: Rc<Cell<u32>>,
    out_received: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl TimeoutOnceOutDevice {
    fn new(out_received: Rc<RefCell<Vec<Vec<u8>>>>) -> Self {
        Self {
            call_count: Rc::new(Cell::new(0)),
            out_received,
        }
    }
}

impl UsbDeviceModel for TimeoutOnceOutDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, _ep_addr: u8, _max_len: usize) -> UsbInResult {
        UsbInResult::Stall
    }

    fn handle_out_transfer(&mut self, ep_addr: u8, data: &[u8]) -> UsbOutResult {
        assert_eq!(ep_addr, 0x01);
        let n = self.call_count.get();
        self.call_count.set(n + 1);
        if n == 0 {
            UsbOutResult::Timeout
        } else {
            self.out_received.borrow_mut().push(data.to_vec());
            UsbOutResult::Ack
        }
    }
}

fn assert_single_success_event(events: &[TransferEvent], ep_addr: u8, trb_ptr: u64, residual: u32) {
    assert_eq!(
        events,
        &[TransferEvent {
            ep_addr,
            trb_ptr,
            residual,
            completion_code: CompletionCode::Success,
        }]
    );
}

#[test]
fn xhci_bulk_out_normal_trb_delivers_payload() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let payload = [0x10u8, 0x20, 0x30, 0x40];
    let buf = alloc.alloc(payload.len() as u32, 0x10) as u64;
    mem.write(buf as u32, &payload);

    write_trb(
        &mut mem,
        normal_trb_addr,
        make_normal_trb(buf, payload.len() as u32, true, false, true),
    );
    write_trb(&mut mem, link_trb_addr, make_link_trb(ring_base, true, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received.clone());
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x01, ring_base);
    xhci.tick_1ms(&mut mem);

    assert_eq!(out_received.borrow().as_slice(), &[payload.to_vec()]);

    let events = xhci.take_events();
    assert_single_success_event(&events, 0x01, normal_trb_addr, 0);
}

#[test]
fn xhci_bulk_out_nak_leaves_trb_pending_until_ack() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let payload = [0xdeu8, 0xad, 0xbe, 0xef];
    let buf = alloc.alloc(payload.len() as u32, 0x10) as u64;
    mem.write(buf as u32, &payload);

    write_trb(
        &mut mem,
        normal_trb_addr,
        make_normal_trb(buf, payload.len() as u32, true, false, true),
    );
    write_trb(&mut mem, link_trb_addr, make_link_trb(ring_base, true, true));

    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkOutNakOnceDevice::new(out_received.clone());
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x01, ring_base);

    // First tick: device NAKs; TD remains pending and no data is delivered.
    xhci.tick_1ms(&mut mem);
    assert!(xhci.take_events().is_empty());
    assert!(out_received.borrow().is_empty());
    assert_eq!(xhci.endpoint_state(0x01).unwrap().ring.dequeue_ptr, ring_base);

    // Second tick: device ACKs; TD completes and payload is delivered once.
    xhci.tick_1ms(&mut mem);
    assert_eq!(out_received.borrow().as_slice(), &[payload.to_vec()]);
    let events = xhci.take_events();
    assert_single_success_event(&events, 0x01, normal_trb_addr, 0);
    assert_eq!(
        xhci.endpoint_state(0x01).unwrap().ring.dequeue_ptr,
        link_trb_addr
    );
}

#[test]
fn xhci_bulk_out_success_without_ioc_advances_ring_without_event() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let payload = [0x11u8, 0x22, 0x33, 0x44];
    let buf = alloc.alloc(payload.len() as u32, 0x10) as u64;
    mem.write(buf as u32, &payload);

    write_trb(
        &mut mem,
        normal_trb_addr,
        make_normal_trb(buf, payload.len() as u32, true, false, false),
    );
    write_trb(&mut mem, link_trb_addr, make_link_trb(ring_base, true, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received.clone());
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x01, ring_base);
    xhci.tick_1ms(&mut mem);

    assert_eq!(out_received.borrow().as_slice(), &[payload.to_vec()]);
    assert!(xhci.take_events().is_empty(), "IOC=0 should suppress event on success");
    assert_eq!(
        xhci.endpoint_state(0x01).unwrap().ring.dequeue_ptr,
        link_trb_addr
    );
}

#[test]
fn xhci_bulk_in_normal_trb_writes_guest_memory() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let buf = alloc.alloc(8, 0x10) as u64;
    let sentinel = [0xa5u8; 8];
    mem.write(buf as u32, &sentinel);

    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 4, true, false, true));
    write_trb(&mut mem, link_trb_addr, make_link_trb(ring_base, true, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    in_queue.borrow_mut().push_back(vec![1, 2, 3, 4]);
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x81, ring_base);
    xhci.tick_1ms(&mut mem);

    let mut got = [0u8; 4];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, [1, 2, 3, 4]);

    let events = xhci.take_events();
    assert_single_success_event(&events, 0x81, normal_trb_addr, 0);
}

#[test]
fn xhci_bulk_in_success_without_ioc_advances_ring_without_event() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let buf = alloc.alloc(4, 0x10) as u64;
    let sentinel = [0xa5u8; 4];
    mem.write(buf as u32, &sentinel);

    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 4, true, false, false));
    write_trb(&mut mem, link_trb_addr, make_link_trb(ring_base, true, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    in_queue.borrow_mut().push_back(vec![1, 2, 3, 4]);
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x81, ring_base);
    xhci.tick_1ms(&mut mem);

    let mut got = [0u8; 4];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, [1, 2, 3, 4]);

    assert!(
        xhci.take_events().is_empty(),
        "IOC=0 should suppress event on success"
    );
    assert_eq!(
        xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        link_trb_addr
    );
}

#[test]
fn xhci_bulk_in_short_packet_sets_residual_bytes() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let buf = alloc.alloc(8, 0x10) as u64;
    let sentinel = [0xa5u8; 8];
    mem.write(buf as u32, &sentinel);

    // Request 8 bytes but only provide 3. xHCI should report the residual byte count (5) and
    // complete with ShortPacket.
    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 8, true, false, true));
    write_trb(&mut mem, link_trb_addr, make_link_trb(ring_base, true, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    in_queue.borrow_mut().push_back(vec![9, 8, 7]);
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x81, ring_base);
    xhci.tick_1ms(&mut mem);

    let mut got = [0u8; 8];
    mem.read(buf as u32, &mut got);
    assert_eq!(&got[..3], &[9, 8, 7]);
    assert_eq!(&got[3..], &sentinel[3..]);

    let events = xhci.take_events();
    assert_eq!(
        events,
        &[TransferEvent {
            ep_addr: 0x81,
            trb_ptr: normal_trb_addr,
            residual: 5,
            completion_code: CompletionCode::ShortPacket,
        }]
    );
}

#[test]
fn xhci_bulk_in_nak_leaves_trb_pending_until_data_available() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let buf = alloc.alloc(8, 0x10) as u64;
    let sentinel = [0xa5u8; 3];
    mem.write(buf as u32, &sentinel);

    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 3, true, false, true));
    write_trb(&mut mem, link_trb_addr, make_link_trb(ring_base, true, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue.clone(), out_received);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x81, ring_base);

    // Tick once with no data; should NAK and leave the TRB pending.
    xhci.tick_1ms(&mut mem);
    assert!(xhci.take_events().is_empty());
    assert_eq!(
        xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        ring_base,
        "NAK should leave the TRB pending"
    );

    let mut got = [0u8; 3];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, sentinel);

    // Provide data and tick again; now the IN TD should complete.
    in_queue.borrow_mut().push_back(vec![1, 2, 3]);
    xhci.tick_1ms(&mut mem);

    let mut got = [0u8; 3];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, [1, 2, 3]);

    let events = xhci.take_events();
    assert_single_success_event(&events, 0x81, normal_trb_addr, 0);
}

#[test]
fn xhci_transfer_executor_follows_link_trb_and_toggles_cycle_state() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let buf = alloc.alloc(4, 0x10) as u64;
    let sentinel = [0xa5u8; 4];
    mem.write(buf as u32, &sentinel);

    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 4, true, false, true));
    write_trb(&mut mem, link_trb_addr, make_link_trb(ring_base, true, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    in_queue.borrow_mut().push_back(vec![1, 2, 3, 4]);
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue.clone(), out_received);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x81, ring_base);
    xhci.tick_1ms(&mut mem);

    let mut got = [0u8; 4];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, [1, 2, 3, 4]);

    let events = xhci.take_events();
    assert_single_success_event(&events, 0x81, normal_trb_addr, 0);

    // After consuming the first TRB, the dequeue pointer should land on the Link TRB.
    assert_eq!(xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr, link_trb_addr);
    assert!(xhci.endpoint_state(0x81).unwrap().ring.cycle);

    // Tick again without producing new TRBs: the executor should follow the Link TRB, toggle cycle,
    // then stop because the next TRB's cycle bit doesn't match.
    xhci.tick_1ms(&mut mem);
    assert!(xhci.take_events().is_empty());
    assert_eq!(xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr, ring_base);
    assert!(!xhci.endpoint_state(0x81).unwrap().ring.cycle);

    // Produce another TRB in the new cycle state (C=0) and confirm it completes.
    in_queue.borrow_mut().push_back(vec![5, 6, 7, 8]);
    mem.write(buf as u32, &sentinel);
    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 4, false, false, true));

    xhci.tick_1ms(&mut mem);
    let mut got = [0u8; 4];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, [5, 6, 7, 8]);

    let events = xhci.take_events();
    assert_single_success_event(&events, 0x81, normal_trb_addr, 0);
}

#[test]
fn xhci_transfer_executor_td_can_span_linked_segments() {
    let mut mem = TestMemory::new(0x20000);
    let mut alloc = Alloc::new(0x2000);

    let seg1 = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let seg2 = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;

    let normal1_addr = seg1;
    let link_addr = seg1 + TRB_LEN;
    let normal2_addr = seg2;

    let buf1 = alloc.alloc(4, 0x10) as u64;
    let buf2 = alloc.alloc(4, 0x10) as u64;
    let sentinel = [0xa5u8; 4];
    mem.write(buf1 as u32, &sentinel);
    mem.write(buf2 as u32, &sentinel);

    // TD: Normal (CH=1) then Link to seg2, then Normal (CH=0, IOC=1).
    write_trb(&mut mem, normal1_addr, make_normal_trb(buf1, 4, true, true, false));
    write_trb(&mut mem, link_addr, make_link_trb(seg2, true, false));
    write_trb(&mut mem, normal2_addr, make_normal_trb(buf2, 4, true, false, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    in_queue.borrow_mut().push_back(vec![10, 11, 12, 13, 20, 21, 22, 23]);
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x81, seg1);
    xhci.tick_1ms(&mut mem);

    let mut got1 = [0u8; 4];
    let mut got2 = [0u8; 4];
    mem.read(buf1 as u32, &mut got1);
    mem.read(buf2 as u32, &mut got2);
    assert_eq!(got1, [10, 11, 12, 13]);
    assert_eq!(got2, [20, 21, 22, 23]);

    let events = xhci.take_events();
    assert_single_success_event(&events, 0x81, normal2_addr, 0);
}

#[test]
fn xhci_bulk_out_chained_normal_trbs_concatenate_payload() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let trb0_addr = ring_base;
    let trb1_addr = ring_base + TRB_LEN;

    let buf0 = alloc.alloc(2, 0x10) as u64;
    let buf1 = alloc.alloc(2, 0x10) as u64;

    mem.write(buf0 as u32, &[0x10, 0x20]);
    mem.write(buf1 as u32, &[0x30, 0x40]);

    // TD: Normal (CH=1) then Normal (CH=0, IOC=1).
    write_trb(&mut mem, trb0_addr, make_normal_trb(buf0, 2, true, true, false));
    write_trb(&mut mem, trb1_addr, make_normal_trb(buf1, 2, true, false, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received.clone());
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x01, ring_base);
    xhci.tick_1ms(&mut mem);

    assert_eq!(
        out_received.borrow().as_slice(),
        &[vec![0x10, 0x20, 0x30, 0x40]]
    );

    let events = xhci.take_events();
    assert_single_success_event(&events, 0x01, trb1_addr, 0);
}

#[test]
fn xhci_bulk_out_stall_halts_endpoint_and_reports_residual() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc(TRB_LEN as u32, 0x10) as u64;

    let payload = [0xdeu8, 0xad, 0xbe, 0xef];
    let buf = alloc.alloc(payload.len() as u32, 0x10) as u64;
    mem.write(buf as u32, &payload);

    write_trb(
        &mut mem,
        ring_base,
        make_normal_trb(buf, payload.len() as u32, true, false, true),
    );

    // BulkEndpointDevice only implements EP 0x01. Using a different OUT endpoint should STALL.
    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received.clone());
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));

    xhci.add_endpoint(0x02, ring_base);
    xhci.tick_1ms(&mut mem);

    // Device must not observe the stalled transfer.
    assert!(out_received.borrow().is_empty());

    let events = xhci.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x02);
    assert_eq!(events[0].trb_ptr, ring_base);
    assert_eq!(events[0].residual, payload.len() as u32);
    assert_eq!(events[0].completion_code, CompletionCode::StallError);

    let state = xhci.endpoint_state(0x02).unwrap();
    assert!(state.halted);
    assert_eq!(state.ring.dequeue_ptr, ring_base + TRB_LEN);

    // Further ticks must not process additional TRBs while halted.
    xhci.tick_1ms(&mut mem);
    assert!(xhci.take_events().is_empty());
}

#[test]
fn xhci_bulk_in_timeout_completes_with_usb_transaction_error() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc(TRB_LEN as u32, 0x10) as u64;
    let buf = alloc.alloc(4, 0x10) as u64;
    let sentinel = [0xa5u8; 4];
    mem.write(buf as u32, &sentinel);

    write_trb(&mut mem, ring_base, make_normal_trb(buf, 4, true, false, false));

    let mut xhci = XhciTransferExecutor::new(Box::new(TimeoutInDevice));
    xhci.add_endpoint(0x81, ring_base);
    xhci.tick_1ms(&mut mem);

    // On timeout, we should advance the TD, emit an error event, and leave guest memory untouched.
    let mut got = [0u8; 4];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, sentinel);

    let events = xhci.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x81);
    assert_eq!(events[0].trb_ptr, ring_base);
    assert_eq!(events[0].residual, 4);
    assert_eq!(events[0].completion_code, CompletionCode::UsbTransactionError);
    assert!(!xhci.endpoint_state(0x81).unwrap().halted);
    assert_eq!(
        xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        ring_base + TRB_LEN
    );
}

#[test]
fn xhci_bulk_in_timeout_does_not_halt_and_allows_subsequent_trbs() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let trb0_addr = ring_base;
    let trb1_addr = ring_base + TRB_LEN;

    let buf0 = alloc.alloc(4, 0x10) as u64;
    let buf1 = alloc.alloc(4, 0x10) as u64;
    let sentinel = [0xa5u8; 4];
    mem.write(buf0 as u32, &sentinel);
    mem.write(buf1 as u32, &sentinel);

    // First TD will timeout; second TD should succeed.
    write_trb(&mut mem, trb0_addr, make_normal_trb(buf0, 4, true, false, false));
    write_trb(&mut mem, trb1_addr, make_normal_trb(buf1, 4, true, false, true));

    let dev = TimeoutOnceInDevice::new();
    let call_count = dev.call_count.clone();
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));
    xhci.add_endpoint(0x81, ring_base);

    // Tick #1: timeout should complete the first TD, emit an error event, and advance.
    xhci.tick_1ms(&mut mem);
    assert_eq!(call_count.get(), 1);
    let events = xhci.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x81);
    assert_eq!(events[0].trb_ptr, trb0_addr);
    assert_eq!(events[0].residual, 4);
    assert_eq!(events[0].completion_code, CompletionCode::UsbTransactionError);
    assert!(!xhci.endpoint_state(0x81).unwrap().halted);
    assert_eq!(xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr, trb1_addr);

    // Guest buffers should be unchanged after the timeout.
    let mut got0 = [0u8; 4];
    mem.read(buf0 as u32, &mut got0);
    assert_eq!(got0, sentinel);
    let mut got1 = [0u8; 4];
    mem.read(buf1 as u32, &mut got1);
    assert_eq!(got1, sentinel);

    // Tick #2: should complete the second TD successfully.
    xhci.tick_1ms(&mut mem);
    assert_eq!(call_count.get(), 2);
    let events = xhci.take_events();
    assert_single_success_event(&events, 0x81, trb1_addr, 0);
    assert_eq!(xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr, ring_base + 2 * TRB_LEN);

    mem.read(buf1 as u32, &mut got1);
    assert_eq!(got1, [1, 2, 3, 4]);
}

#[test]
fn xhci_bulk_out_timeout_does_not_halt_and_allows_subsequent_trbs() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let trb0_addr = ring_base;
    let trb1_addr = ring_base + TRB_LEN;

    let payload0 = [0x10u8, 0x20, 0x30, 0x40];
    let payload1 = [0xaau8, 0xbb, 0xcc, 0xdd];

    let buf0 = alloc.alloc(4, 0x10) as u64;
    let buf1 = alloc.alloc(4, 0x10) as u64;
    mem.write(buf0 as u32, &payload0);
    mem.write(buf1 as u32, &payload1);

    // First TD will timeout (IOC=0); second TD should succeed.
    write_trb(&mut mem, trb0_addr, make_normal_trb(buf0, 4, true, false, false));
    write_trb(&mut mem, trb1_addr, make_normal_trb(buf1, 4, true, false, true));

    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = TimeoutOnceOutDevice::new(out_received.clone());
    let call_count = dev.call_count.clone();
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));
    xhci.add_endpoint(0x01, ring_base);

    // Tick #1: timeout should complete the first TD, emit an error event, and advance.
    xhci.tick_1ms(&mut mem);
    assert_eq!(call_count.get(), 1);
    let events = xhci.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x01);
    assert_eq!(events[0].trb_ptr, trb0_addr);
    assert_eq!(events[0].residual, 4);
    assert_eq!(events[0].completion_code, CompletionCode::UsbTransactionError);
    assert!(!xhci.endpoint_state(0x01).unwrap().halted);
    assert_eq!(xhci.endpoint_state(0x01).unwrap().ring.dequeue_ptr, trb1_addr);
    assert!(out_received.borrow().is_empty());

    // Tick #2: should complete the second TD successfully.
    xhci.tick_1ms(&mut mem);
    assert_eq!(call_count.get(), 2);
    let events = xhci.take_events();
    assert_single_success_event(&events, 0x01, trb1_addr, 0);
    assert_eq!(out_received.borrow().as_slice(), &[payload1.to_vec()]);
    assert_eq!(xhci.endpoint_state(0x01).unwrap().ring.dequeue_ptr, ring_base + 2 * TRB_LEN);
}

#[test]
fn xhci_transfer_executor_advances_past_unsupported_trb_and_processes_next() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let bad_trb_addr = ring_base;
    let normal_trb_addr = ring_base + TRB_LEN;

    let mut bad_trb = Trb::new(0, 0, 0);
    bad_trb.set_cycle(true);
    // TRB type 0 is reserved/invalid.
    bad_trb.set_trb_type_raw(0);
    write_trb(&mut mem, bad_trb_addr, bad_trb);

    let buf = alloc.alloc(4, 0x10) as u64;
    let sentinel = [0xa5u8; 4];
    mem.write(buf as u32, &sentinel);
    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 4, true, false, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    in_queue.borrow_mut().push_back(vec![1, 2, 3, 4]);
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue, out_received);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));
    xhci.add_endpoint(0x81, ring_base);

    // Tick #1: executor should emit a TRB error and advance one TRB without halting.
    xhci.tick_1ms(&mut mem);
    let events = xhci.take_events();
    assert_eq!(
        events,
        &[TransferEvent {
            ep_addr: 0x81,
            trb_ptr: bad_trb_addr,
            residual: 0,
            completion_code: CompletionCode::TrbError,
        }]
    );
    assert!(!xhci.endpoint_state(0x81).unwrap().halted);
    assert_eq!(xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr, normal_trb_addr);

    // Buffer should be unchanged after the unsupported TRB.
    let mut got = [0u8; 4];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, sentinel);

    // Tick #2: executor should process the next TRB normally.
    xhci.tick_1ms(&mut mem);
    let events = xhci.take_events();
    assert_single_success_event(&events, 0x81, normal_trb_addr, 0);

    mem.read(buf as u32, &mut got);
    assert_eq!(got, [1, 2, 3, 4]);

    assert_eq!(
        xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        ring_base + 2 * TRB_LEN
    );
}

#[test]
fn xhci_transfer_executor_noop_trb_advances_ring_and_does_not_touch_device() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let noop_addr = ring_base;
    let normal_trb_addr = ring_base + TRB_LEN;

    let mut noop = Trb::new(0, 0, 0);
    noop.set_trb_type(TrbType::NoOp);
    noop.set_cycle(true);
    noop.control |= Trb::CONTROL_IOC_BIT;
    write_trb(&mut mem, noop_addr, noop);

    let buf = alloc.alloc(4, 0x10) as u64;
    let sentinel = [0xa5u8; 4];
    mem.write(buf as u32, &sentinel);
    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 4, true, false, true));

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    in_queue.borrow_mut().push_back(vec![1, 2, 3, 4]);
    let out_received = Rc::new(RefCell::new(Vec::new()));
    let dev = BulkEndpointDevice::new(in_queue.clone(), out_received);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));
    xhci.add_endpoint(0x81, ring_base);

    // Tick #1: the NoOp TRB should complete successfully, advance the ring, and not invoke the
    // device model (queue should be untouched, guest buffers unchanged).
    xhci.tick_1ms(&mut mem);
    assert_eq!(in_queue.borrow().len(), 1, "NoOp TRB must not consume IN queue");

    let mut got = [0u8; 4];
    mem.read(buf as u32, &mut got);
    assert_eq!(got, sentinel);

    assert_eq!(
        xhci.take_events(),
        &[TransferEvent {
            ep_addr: 0x81,
            trb_ptr: noop_addr,
            residual: 0,
            completion_code: CompletionCode::Success,
        }]
    );
    assert_eq!(xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr, normal_trb_addr);

    // Tick #2: the Normal TRB should now complete and DMA the queued bytes.
    xhci.tick_1ms(&mut mem);
    assert!(in_queue.borrow().is_empty());
    let events = xhci.take_events();
    assert_single_success_event(&events, 0x81, normal_trb_addr, 0);

    mem.read(buf as u32, &mut got);
    assert_eq!(got, [1, 2, 3, 4]);
    assert_eq!(
        xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        ring_base + 2 * TRB_LEN
    );
}
