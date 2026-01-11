use aero_audio::clock::AudioFrameClock;

const SAMPLE_RATE_HZ: u32 = 48_000;
const NS_PER_SEC: u64 = 1_000_000_000;

fn tick_60hz_ns(tick_index: u32) -> u64 {
    // 1 second / 60 = 16_666_666.666... ns. Use a common pattern that distributes the extra
    // 40 nanoseconds across the second: 40 ticks of 16_666_667ns and 20 ticks of 16_666_666ns.
    if tick_index < 40 {
        16_666_667
    } else {
        16_666_666
    }
}

#[test]
fn advance_one_second_is_exact() {
    let mut clock = AudioFrameClock::new(SAMPLE_RATE_HZ, 0);
    assert_eq!(clock.advance_to(NS_PER_SEC), SAMPLE_RATE_HZ as usize);
    assert_eq!(clock.frac_fp, 0);
}

#[test]
fn repeated_small_steps_sum_to_single_large_step() {
    let mut clock_single = AudioFrameClock::new(SAMPLE_RATE_HZ, 0);
    let single = clock_single.advance_to(NS_PER_SEC);

    let mut clock_steps = AudioFrameClock::new(SAMPLE_RATE_HZ, 0);
    let mut now_ns = 0u64;
    let mut total = 0usize;
    for tick in 0..60 {
        now_ns += tick_60hz_ns(tick);
        total += clock_steps.advance_to(now_ns);
    }

    assert_eq!(now_ns, NS_PER_SEC);
    assert_eq!(total, single);
    assert_eq!(total, SAMPLE_RATE_HZ as usize);
    assert_eq!(clock_steps.frac_fp, 0);
}

#[test]
fn no_drift_over_ten_minutes_at_60hz() {
    let mut clock = AudioFrameClock::new(SAMPLE_RATE_HZ, 0);
    let mut now_ns = 0u64;
    let mut total_frames = 0usize;

    for _second in 0..600u32 {
        for tick in 0..60u32 {
            now_ns += tick_60hz_ns(tick);
            total_frames += clock.advance_to(now_ns);
        }
    }

    assert_eq!(now_ns, NS_PER_SEC * 600);
    assert_eq!(total_frames, SAMPLE_RATE_HZ as usize * 600);
    assert_eq!(clock.frac_fp, 0);
}

#[test]
fn time_going_backwards_is_ignored() {
    let mut clock = AudioFrameClock::new(SAMPLE_RATE_HZ, 1_000);
    clock.advance_to(2_000);
    assert_eq!(clock.last_time_ns, 2_000);
    assert_eq!(clock.advance_to(1_500), 0);
    assert_eq!(clock.last_time_ns, 2_000);
}
