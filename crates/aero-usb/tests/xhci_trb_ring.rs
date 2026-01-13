use aero_usb::xhci::ring::{RingCursor, RingError, RingPoll};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};

mod util;

use util::TestMemory;

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

