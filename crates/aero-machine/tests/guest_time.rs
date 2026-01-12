use aero_machine::{GuestTime, DEFAULT_GUEST_CPU_HZ};
use pretty_assertions::assert_eq;

#[test]
fn guest_time_accumulates_remainder_and_eventually_advances_ns() {
    // Use a frequency where a single cycle is < 1ns so a naive `cycles * 1e9 / hz` conversion would
    // return 0 forever for `executed = 1`.
    let mut t = GuestTime::new(DEFAULT_GUEST_CPU_HZ);

    let deltas: Vec<u64> = (0..3)
        .map(|_| t.advance_guest_time_for_instructions(1))
        .collect();

    // 3GHz => 3 cycles per nanosecond, so 3 single-cycle calls should advance 1ns in total.
    assert_eq!(deltas, vec![0, 0, 1]);
}

#[test]
fn guest_time_split_batches_match_single_batch() {
    let hz = DEFAULT_GUEST_CPU_HZ;
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

#[test]
fn guest_time_default_frequency_matches_cpu_core_default_tsc() {
    assert_eq!(GuestTime::default().cpu_hz(), aero_cpu_core::time::DEFAULT_TSC_HZ);
}

#[test]
fn pit_irq0_pulse_after_accumulated_guest_time() {
    use aero_devices::pit8254::{Pit8254, PIT_CH0, PIT_CMD, PIT_HZ};

    fn program_divisor(pit: &mut Pit8254, mode_cmd: u8, divisor: u16) {
        pit.port_write(PIT_CMD, 1, mode_cmd as u32);
        pit.port_write(PIT_CH0, 1, (divisor & 0xFF) as u32);
        pit.port_write(PIT_CH0, 1, (divisor >> 8) as u32);
    }

    let mut pit = Pit8254::new();
    // ch0, lobyte/hibyte, mode2 (rate generator), binary.
    program_divisor(&mut pit, 0x34, 4);

    // Small batches that would stall forever without remainder accumulation.
    let mut time = GuestTime::new(DEFAULT_GUEST_CPU_HZ);

    // Minimum nanoseconds required to produce 4 PIT input ticks:
    // ticks = floor(ns * PIT_HZ / 1e9)  =>  ns = ceil(ticks * 1e9 / PIT_HZ)
    let ns_needed = ((4u128) * 1_000_000_000u128 + (PIT_HZ as u128) - 1) / (PIT_HZ as u128);

    // Convert that to the minimum number of cycles needed at `DEFAULT_GUEST_CPU_HZ`.
    let cycles_needed =
        (ns_needed * (DEFAULT_GUEST_CPU_HZ as u128) + 1_000_000_000u128 - 1) / 1_000_000_000u128;

    for _ in 0..(cycles_needed as u64) {
        let delta_ns = time.advance_guest_time_for_instructions(1);
        pit.advance_ns(delta_ns);
    }

    assert_eq!(pit.take_irq0_pulses(), 1);
}

#[test]
fn guest_time_resync_from_tsc_preserves_future_deltas() {
    let hz = DEFAULT_GUEST_CPU_HZ;

    let prefix = [1u64, 2, 3, 1, 1, 7, 11, 1];
    let suffix = [1u64, 1, 2, 5, 1, 3, 8];

    let mut a = GuestTime::new(hz);
    let mut total_cycles = 0u64;
    for &n in &prefix {
        total_cycles = total_cycles.saturating_add(n);
        let _ = a.advance_guest_time_for_instructions(n);
    }

    let mut b = GuestTime::new(hz);
    b.resync_from_tsc(total_cycles);

    for &n in &suffix {
        assert_eq!(
            a.advance_guest_time_for_instructions(n),
            b.advance_guest_time_for_instructions(n)
        );
    }
}
