use aero_usb::xhci::ring::{RingCursor, RingError, RingPoll};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::MemoryBus;
use std::collections::BTreeMap;

mod util;

use util::TestMemory;

#[derive(Default)]
struct SparseMem {
    bytes: BTreeMap<u64, u8>,
}

impl MemoryBus for SparseMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = paddr.wrapping_add(i as u64);
            *slot = *self.bytes.get(&addr).unwrap_or(&0);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (i, val) in buf.iter().enumerate() {
            let addr = paddr.wrapping_add(i as u64);
            self.bytes.insert(addr, *val);
        }
    }
}

#[test]
fn trb_pack_unpack_roundtrip() {
    let mut trb = Trb::new(0x1122_3344_5566_7788, 0xaabb_ccdd, 0);
    trb.set_cycle(true);
    trb.set_trb_type(TrbType::Normal);
    trb.set_slot_id(0x5a);
    trb.set_endpoint_id(0x0f);

    let bytes = trb.to_bytes();
    let decoded = Trb::from_bytes(bytes);
    assert_eq!(decoded, trb);

    assert!(decoded.cycle());
    assert_eq!(decoded.trb_type(), TrbType::Normal);
    assert_eq!(decoded.trb_type_raw(), TrbType::Normal.raw());
    assert_eq!(decoded.slot_id(), 0x5a);
    assert_eq!(decoded.endpoint_id(), 0x0f);

    let mut mem = TestMemory::new(0x1000);
    trb.write_to(&mut mem, 0x100);
    let read_back = Trb::read_from(&mut mem, 0x100);
    assert_eq!(read_back, trb);
}

#[test]
fn trb_unknown_type_is_preserved() {
    let mut trb = Trb::default();
    trb.set_cycle(true);
    trb.set_trb_type_raw(0xff); // masked to 0x3f
    assert_eq!(trb.trb_type_raw(), 0x3f);
    assert_eq!(trb.trb_type(), TrbType::Unknown(0x3f));

    let bytes = trb.to_bytes();
    let decoded = Trb::from_bytes(bytes);
    assert_eq!(decoded.trb_type_raw(), 0x3f);
    assert_eq!(decoded.trb_type(), TrbType::Unknown(0x3f));
}

#[test]
fn ring_cursor_masks_dequeue_pointer_low_bits() {
    let cur = RingCursor::new(0x1234_u64 | 0xf, true);
    assert_eq!(cur.dequeue_ptr(), 0x1230);
}

#[test]
fn ring_cursor_zero_step_budget_errors() {
    let mut mem = TestMemory::new(0x1000);
    let mut cur = RingCursor::new(0x100, true);
    assert_eq!(
        cur.poll(&mut mem, 0),
        RingPoll::Err(RingError::StepBudgetExceeded)
    );
    assert_eq!(cur.dequeue_ptr(), 0x100);
}

#[test]
fn ring_cursor_reports_address_overflow() {
    let mut mem = SparseMem::default();

    let trb_addr = u64::MAX & !0x0f;
    let mut trb = Trb::default();
    trb.set_cycle(true);
    trb.set_trb_type(TrbType::Normal);
    trb.write_to(&mut mem, trb_addr);

    let mut cur = RingCursor::new(u64::MAX, true);
    assert_eq!(cur.dequeue_ptr(), trb_addr);
    assert_eq!(
        cur.poll(&mut mem, 8),
        RingPoll::Err(RingError::AddressOverflow)
    );
    assert_eq!(cur.dequeue_ptr(), trb_addr);
}

#[test]
fn ring_cursor_follows_links_and_toggles_cycle() {
    let mut mem = TestMemory::new(0x10_000);

    let seg1: u64 = 0x1000;
    let seg2: u64 = 0x2000;

    // Segment 1: [Normal] [Link -> seg2, TC=0]
    let mut n1 = Trb::default();
    n1.parameter = 0xaaaabbbb_ccccdddd;
    n1.set_cycle(true);
    n1.set_trb_type(TrbType::Normal);
    n1.write_to(&mut mem, seg1);

    let mut l1 = Trb::default();
    l1.parameter = seg2;
    l1.set_cycle(true);
    l1.set_trb_type(TrbType::Link);
    l1.set_link_toggle_cycle(false);
    l1.write_to(&mut mem, seg1 + TRB_LEN as u64);

    // Segment 2: [Normal] [Link -> seg1, TC=1]
    let mut n2 = Trb::default();
    n2.parameter = 0x1111_2222_3333_4444;
    n2.set_cycle(true);
    n2.set_trb_type(TrbType::Normal);
    n2.write_to(&mut mem, seg2);

    let mut l2 = Trb::default();
    l2.parameter = seg1;
    l2.set_cycle(true);
    l2.set_trb_type(TrbType::Link);
    l2.set_link_toggle_cycle(true);
    l2.write_to(&mut mem, seg2 + TRB_LEN as u64);

    let mut cur = RingCursor::new(seg1, true);

    // First TRB: seg1 normal.
    match cur.poll(&mut mem, 8) {
        RingPoll::Ready(item) => {
            assert_eq!(item.paddr, seg1);
            assert_eq!(item.trb.trb_type(), TrbType::Normal);
            assert_eq!(item.trb.parameter, 0xaaaabbbb_ccccdddd);
        }
        other => panic!("expected Ready, got {other:?}"),
    }
    assert_eq!(cur.dequeue_ptr(), seg1 + TRB_LEN as u64);
    assert_eq!(cur.cycle_state(), true);

    // Second TRB: should skip Link TRB and return seg2 normal.
    match cur.poll(&mut mem, 8) {
        RingPoll::Ready(item) => {
            assert_eq!(item.paddr, seg2);
            assert_eq!(item.trb.trb_type(), TrbType::Normal);
            assert_eq!(item.trb.parameter, 0x1111_2222_3333_4444);
        }
        other => panic!("expected Ready, got {other:?}"),
    }
    assert_eq!(cur.dequeue_ptr(), seg2 + TRB_LEN as u64);
    assert_eq!(cur.cycle_state(), true);

    // Third poll: should follow Link TRB, toggle cycle, then stop due to cycle mismatch at seg1.
    assert_eq!(cur.poll(&mut mem, 8), RingPoll::NotReady);
    assert_eq!(cur.dequeue_ptr(), seg1);
    assert_eq!(cur.cycle_state(), false);
}

#[test]
fn ring_cursor_does_not_follow_link_when_cycle_mismatch() {
    let mut mem = TestMemory::new(0x10_000);

    let seg1: u64 = 0x1000;
    let seg2: u64 = 0x2000;

    let mut link = Trb::default();
    link.parameter = seg2;
    link.set_cycle(false); // mismatch
    link.set_trb_type(TrbType::Link);
    link.set_link_toggle_cycle(true);
    link.write_to(&mut mem, seg1);

    let mut normal = Trb::default();
    normal.parameter = 0xdead_beef;
    normal.set_cycle(true);
    normal.set_trb_type(TrbType::Normal);
    normal.write_to(&mut mem, seg2);

    let mut cur = RingCursor::new(seg1, true);
    assert_eq!(cur.poll(&mut mem, 8), RingPoll::NotReady);
    assert_eq!(cur.dequeue_ptr(), seg1);
    assert_eq!(cur.cycle_state(), true);
}

#[test]
fn ring_cursor_rejects_null_link_target() {
    let mut mem = TestMemory::new(0x10_000);

    let seg1: u64 = 0x1000;

    let mut link = Trb::default();
    link.parameter = 0;
    link.set_cycle(true);
    link.set_trb_type(TrbType::Link);
    link.write_to(&mut mem, seg1);

    let mut cur = RingCursor::new(seg1, true);
    assert_eq!(
        cur.poll(&mut mem, 8),
        RingPoll::Err(RingError::InvalidLinkTarget)
    );
    assert_eq!(cur.dequeue_ptr(), seg1);
    assert_eq!(cur.cycle_state(), true);
}

#[test]
fn ring_cursor_step_budget_prevents_infinite_link_loops() {
    let mut mem = TestMemory::new(0x10_000);

    let a: u64 = 0x1000;
    let b: u64 = 0x2000;

    // Malformed ring: link TRBs pointing to each other, alternating cycle bits and toggling cycle.
    // This would loop forever without a step budget.
    let mut link_a = Trb::default();
    link_a.parameter = b;
    link_a.set_cycle(true);
    link_a.set_trb_type(TrbType::Link);
    link_a.set_link_toggle_cycle(true);
    link_a.write_to(&mut mem, a);

    let mut link_b = Trb::default();
    link_b.parameter = a;
    link_b.set_cycle(false);
    link_b.set_trb_type(TrbType::Link);
    link_b.set_link_toggle_cycle(true);
    link_b.write_to(&mut mem, b);

    let mut cur = RingCursor::new(a, true);
    assert_eq!(
        cur.poll(&mut mem, 4),
        RingPoll::Err(RingError::StepBudgetExceeded)
    );
}

#[test]
fn ring_cursor_treats_all_ones_trb_fetch_as_error() {
    let mut mem = TestMemory::new(0x1000);
    let addr: u64 = 0x100;
    mem.write_physical(addr, &[0xFF; TRB_LEN]);

    let mut cur = RingCursor::new(addr, true);
    assert_eq!(
        cur.poll(&mut mem, 8),
        RingPoll::Err(RingError::InvalidDmaRead)
    );
    assert_eq!(cur.dequeue_ptr(), addr);
}
