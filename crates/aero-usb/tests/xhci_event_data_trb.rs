use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_usb::xhci::transfer::{write_trb, CompletionCode, Trb, TrbType, TransferEvent, XhciTransferExecutor};
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

fn make_event_data_trb(event_data: u64, cycle: bool, ioc: bool) -> Trb {
    let mut trb = Trb::new(event_data, 0, 0);
    trb.set_trb_type(TrbType::EventData);
    trb.set_cycle(cycle);
    if ioc {
        trb.control |= Trb::CONTROL_IOC_BIT;
    }
    trb
}

#[derive(Clone, Debug)]
struct InQueueDevice {
    in_queue: Rc<RefCell<VecDeque<Vec<u8>>>>,
}

impl InQueueDevice {
    fn new(in_queue: Rc<RefCell<VecDeque<Vec<u8>>>>) -> Self {
        Self { in_queue }
    }
}

impl UsbDeviceModel for InQueueDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        assert_eq!(ep_addr, 0x81);
        let Some(mut data) = self.in_queue.borrow_mut().pop_front() else {
            return UsbInResult::Nak;
        };
        if data.len() > max_len {
            data.truncate(max_len);
        }
        UsbInResult::Data(data)
    }

    fn handle_out_transfer(&mut self, _ep_addr: u8, _data: &[u8]) -> UsbOutResult {
        UsbOutResult::Stall
    }
}

#[test]
fn xhci_td_can_terminate_with_event_data_trb() {
    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    let ring_base = alloc.alloc((TRB_LEN * 2) as u32, 0x10) as u64;
    let normal_trb_addr = ring_base;
    let event_data_trb_addr = ring_base + TRB_LEN;

    let buf = alloc.alloc(8, 0x10) as u64;
    let sentinel = [0xa5u8; 8];
    mem.write(buf as u32, &sentinel);

    // TD: Normal (CH=1) then Event Data (CH=0, IOC=1). The Event Data TRB does not contribute data
    // bytes, but should be accepted as a TD terminator.
    write_trb(&mut mem, normal_trb_addr, make_normal_trb(buf, 4, true, true, false));
    write_trb(
        &mut mem,
        event_data_trb_addr,
        make_event_data_trb(0xfeed_beef, true, true),
    );

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    in_queue.borrow_mut().push_back(vec![1, 2, 3, 4]);
    let dev = InQueueDevice::new(in_queue);
    let mut xhci = XhciTransferExecutor::new(Box::new(dev));
    xhci.add_endpoint(0x81, ring_base);

    xhci.tick_1ms(&mut mem);

    let mut got = [0u8; 8];
    mem.read(buf as u32, &mut got);
    assert_eq!(&got[..4], &[1, 2, 3, 4]);
    assert_eq!(&got[4..], &sentinel[4..]);

    assert_eq!(
        xhci.take_events(),
        &[TransferEvent {
            ep_addr: 0x81,
            trb_ptr: event_data_trb_addr,
            event_data: Some(0xfeed_beef),
            residual: 0,
            completion_code: CompletionCode::Success,
        }]
    );
    assert_eq!(
        xhci.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        ring_base + 2 * TRB_LEN
    );
}
