use aero_machine::GuestTime;
use pretty_assertions::assert_eq;

#[test]
fn guest_time_accumulates_remainder_and_eventually_advances_ns() {
    // Use a frequency where a single cycle is < 1ns so a naive `cycles * 1e9 / hz` conversion would
    // return 0 forever for `executed = 1`.
    let hz = 3_000_000_000u64;
    let mut t = GuestTime::new(hz);

    let deltas: Vec<u64> = (0..3)
        .map(|_| t.advance_guest_time_for_instructions(1))
        .collect();

    // 3GHz => 3 cycles per nanosecond, so 3 single-cycle calls should advance 1ns in total.
    assert_eq!(deltas, vec![0, 0, 1]);
}

#[test]
fn guest_time_split_batches_match_single_batch() {
    let hz = 3_000_000_000u64;
    let cycles = 10_000u64;

    let mut split = GuestTime::new(hz);
    let mut sum_split = 0u64;
    for _ in 0..cycles {
        sum_split += split.advance_guest_time_for_instructions(1);
    }

    let mut single = GuestTime::new(hz);
    let sum_single = single.advance_guest_time_for_instructions(cycles);
    assert_eq!(sum_split, sum_single);

    // The remainder accumulator should also be consistent: continuing with the same instruction
    // stream should yield the same future deltas.
    assert_eq!(
        split.advance_guest_time_for_instructions(1),
        single.advance_guest_time_for_instructions(1)
    );
}

