use std::ops::Range;

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::xhci::transfer::{write_trb, CompletionCode, Trb, TrbType, XhciTransferExecutor};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket};

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
