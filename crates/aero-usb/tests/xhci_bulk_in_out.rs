use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_usb::xhci::transfer::{
    write_trb, CompletionCode, Trb, TrbType, TransferEvent, XhciTransferExecutor,
};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult};

mod util;

use util::{Alloc, TestMemory};

const TRB_LEN: u64 = 0x10;

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, ioc: bool) -> Trb {
    let mut dword3 = 0u32;
    if cycle {
        dword3 |= 1;
    }
    if ioc {
        dword3 |= 1 << 5;
    }
    dword3 |= (TrbType::Normal as u32) << 10;
    Trb {
        dword0: buf_ptr as u32,
        dword1: (buf_ptr >> 32) as u32,
        dword2: len & 0x1ffff,
        dword3,
    }
}

fn make_link_trb(target: u64, cycle: bool, toggle_cycle: bool) -> Trb {
    let mut dword3 = 0u32;
    if cycle {
        dword3 |= 1;
    }
    if toggle_cycle {
        dword3 |= 1 << 1;
    }
    dword3 |= (TrbType::Link as u32) << 10;
    Trb {
        dword0: target as u32,
        dword1: (target >> 32) as u32,
        dword2: 0,
        dword3,
    }
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
        make_normal_trb(buf, payload.len() as u32, true, true),
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
fn xhci_bulk_in_normal_trb_writes_guest_memory() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let link_trb_addr = ring_base + TRB_LEN;

    let buf = alloc.alloc(8, 0x10) as u64;
    let sentinel = [0xa5u8; 8];
    mem.write(buf as u32, &sentinel);

    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 4, true, true));
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
    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 8, true, true));
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

    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 3, true, true));
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
