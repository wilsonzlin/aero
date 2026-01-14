use std::collections::BTreeMap;

use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::xhci::transfer::{write_trb, CompletionCode, Trb, TrbType, XhciTransferExecutor};
use aero_usb::MemoryBus;
use std::boxed::Box;

const RING_BASE: u64 = 0x1000;

#[derive(Default)]
struct SparseMem {
    bytes: BTreeMap<u64, u8>,
}

impl MemoryBus for SparseMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        for (i, out) in buf.iter_mut().enumerate() {
            *out = *self.bytes.get(&paddr.wrapping_add(i as u64)).unwrap_or(&0);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (i, b) in buf.iter().enumerate() {
            self.bytes.insert(paddr.wrapping_add(i as u64), *b);
        }
    }
}

#[derive(Default)]
struct AllOnesMem;

impl MemoryBus for AllOnesMem {
    fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
        buf.fill(0xFF);
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
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

fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, chain: bool) -> Trb {
    let mut dword3 = 0u32;
    if cycle {
        dword3 |= 1;
    }
    if chain {
        dword3 |= 1 << 4;
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
fn xhci_transfer_executor_rejects_null_link_target() {
    let keyboard = UsbHidKeyboardHandle::new();
    let mut exec = XhciTransferExecutor::new(Box::new(keyboard.clone()));

    let mut mem = SparseMem::default();
    write_trb(&mut mem, RING_BASE, make_link_trb(0, true, false));

    exec.add_endpoint(0x81, RING_BASE);
    exec.tick_1ms(&mut mem);

    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_code, CompletionCode::TrbError);
    assert!(exec.endpoint_state(0x81).unwrap().halted);
    assert_eq!(
        exec.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        RING_BASE
    );
}

#[test]
fn xhci_transfer_executor_halts_on_all_ones_trb_fetch() {
    let keyboard = UsbHidKeyboardHandle::new();
    let mut exec = XhciTransferExecutor::new(Box::new(keyboard.clone()));

    let mut mem = AllOnesMem::default();
    exec.add_endpoint(0x81, RING_BASE);
    exec.tick_1ms(&mut mem);

    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_code, CompletionCode::TrbError);
    assert_eq!(events[0].trb_ptr, RING_BASE);
    assert!(exec.endpoint_state(0x81).unwrap().halted);

    exec.tick_1ms(&mut mem);
    assert!(exec.take_events().is_empty());
}

#[test]
fn xhci_transfer_executor_halts_on_self_referential_link_trb() {
    let keyboard = UsbHidKeyboardHandle::new();
    let mut exec = XhciTransferExecutor::new(Box::new(keyboard.clone()));

    let mut mem = SparseMem::default();
    // Malformed ring: a Link TRB that points to itself without toggling cycle.
    write_trb(&mut mem, RING_BASE, make_link_trb(RING_BASE, true, false));

    exec.add_endpoint(0x81, RING_BASE);
    exec.tick_1ms(&mut mem);

    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_code, CompletionCode::TrbError);
    assert_eq!(events[0].trb_ptr, RING_BASE);
    assert!(exec.endpoint_state(0x81).unwrap().halted);
    assert_eq!(
        exec.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        RING_BASE
    );

    exec.tick_1ms(&mut mem);
    assert!(exec.take_events().is_empty());
}

#[test]
fn xhci_transfer_executor_halts_on_excessive_link_trbs() {
    let keyboard = UsbHidKeyboardHandle::new();
    let mut exec = XhciTransferExecutor::new(Box::new(keyboard.clone()));

    let mut mem = SparseMem::default();

    // 33 link TRBs in a cycle; MAX_LINK_SKIP is 32, so this should fault.
    for i in 0..33u64 {
        let addr = RING_BASE + i * 16;
        let next = if i == 32 { RING_BASE } else { addr + 16 };
        write_trb(&mut mem, addr, make_link_trb(next, true, false));
    }

    exec.add_endpoint(0x81, RING_BASE);
    exec.tick_1ms(&mut mem);

    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_code, CompletionCode::TrbError);
    assert!(exec.endpoint_state(0x81).unwrap().halted);
}

#[test]
fn xhci_transfer_executor_halts_on_overlong_td_chain() {
    let keyboard = UsbHidKeyboardHandle::new();
    let mut exec = XhciTransferExecutor::new(Box::new(keyboard.clone()));

    let mut mem = SparseMem::default();

    // MAX_TD_TRBS is 64; chain more than that with CH=1 so the TD never terminates.
    for i in 0..65u64 {
        let addr = RING_BASE + i * 16;
        write_trb(&mut mem, addr, make_normal_trb(0x2000, 8, true, true));
    }

    exec.add_endpoint(0x81, RING_BASE);
    exec.tick_1ms(&mut mem);

    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_code, CompletionCode::TrbError);
    assert!(exec.endpoint_state(0x81).unwrap().halted);
    assert_eq!(
        exec.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        RING_BASE
    );
}

#[test]
fn xhci_transfer_executor_halts_on_dequeue_ptr_overflow() {
    let keyboard = UsbHidKeyboardHandle::new();
    let mut exec = XhciTransferExecutor::new(Box::new(keyboard.clone()));

    let mut mem = SparseMem::default();

    // Place an unsupported TRB at the last aligned address so advancing would overflow.
    let trb_addr = u64::MAX & !0x0f;
    let trb = Trb::from_dwords(0, 0, 0, 1); // cycle=1, type=0 (invalid/unsupported)
    write_trb(&mut mem, trb_addr, trb);

    exec.add_endpoint(0x81, trb_addr);
    exec.tick_1ms(&mut mem);

    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_code, CompletionCode::TrbError);
    assert!(exec.endpoint_state(0x81).unwrap().halted);
    assert_eq!(
        exec.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        trb_addr
    );
}

#[test]
fn xhci_transfer_executor_halts_on_unexpected_trb_inside_td_chain() {
    let keyboard = UsbHidKeyboardHandle::new();
    let mut exec = XhciTransferExecutor::new(Box::new(keyboard.clone()));

    let mut mem = SparseMem::default();

    // Start a TD with a chained Normal TRB.
    write_trb(&mut mem, RING_BASE, make_normal_trb(0x2000, 8, true, true));

    // Follow it with an invalid/unknown TRB type (0), which should fault the TD gather and halt the
    // endpoint. This is distinct from encountering an unsupported TRB when *not* inside a TD chain,
    // which the executor treats as a recoverable error (advance by one TRB without halting).
    let mut bad = Trb::new(0, 0, 0);
    bad.set_cycle(true);
    bad.set_trb_type_raw(0);
    write_trb(&mut mem, RING_BASE + 0x10, bad);

    exec.add_endpoint(0x81, RING_BASE);
    exec.tick_1ms(&mut mem);

    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].completion_code, CompletionCode::TrbError);
    assert_eq!(events[0].trb_ptr, RING_BASE + 0x10);
    assert!(exec.endpoint_state(0x81).unwrap().halted);
    assert_eq!(
        exec.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        RING_BASE
    );

    // Further ticks must not process additional TRBs while halted.
    exec.tick_1ms(&mut mem);
    assert!(exec.take_events().is_empty());
}
