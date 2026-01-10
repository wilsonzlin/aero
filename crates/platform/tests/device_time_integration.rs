use aero_platform::time::VirtualTime;

/// Integration-style test that demonstrates how device models (e.g. PIT/HPET)
/// can be driven deterministically via the shared virtual clock + scheduler.
#[test]
fn pit_and_hpet_interrupt_rates_are_deterministic() {
    const PIT_PERIOD_NS: u64 = 1_000_000; // 1 kHz
    const HPET_PERIOD_NS: u64 = 200_000; // 5 kHz
    const SIM_TIME_NS: u64 = 10_000_000; // 10 ms

    fn simulate(steps: &[u64]) -> (Vec<u64>, Vec<u64>) {
        let mut time = VirtualTime::new();

        // PIT: periodic interrupt source.
        let pit_timer = time.timers_mut().alloc_timer();
        time.timers_mut()
            .arm_periodic(pit_timer, PIT_PERIOD_NS, PIT_PERIOD_NS)
            .unwrap();

        // HPET: modeled as a one-shot comparator that the device re-arms using the
        // previous deadline (not "now") to avoid phase drift when time advances in
        // large chunks.
        let hpet_timer = time.timers_mut().alloc_timer();
        time.timers_mut()
            .arm_one_shot(hpet_timer, HPET_PERIOD_NS)
            .unwrap();

        let mut pit_irqs = Vec::new();
        let mut hpet_irqs = Vec::new();

        for &step in steps {
            time.clock_mut().advance(step);
            loop {
                let now = time.now_ns();
                let events = time.timers_mut().advance_to(now);
                if events.is_empty() {
                    break;
                }
                for event in events {
                    if event.timer_id == pit_timer {
                        pit_irqs.push(event.deadline_ns);
                    } else if event.timer_id == hpet_timer {
                        hpet_irqs.push(event.deadline_ns);
                        let next = event.deadline_ns + HPET_PERIOD_NS;
                        time.timers_mut().arm_one_shot(hpet_timer, next).unwrap();
                    } else {
                        panic!("unexpected timer id {}", event.timer_id.as_u64());
                    }
                }
            }
        }

        (pit_irqs, hpet_irqs)
    }

    // Single-step to the final time.
    let (pit_single, hpet_single) = simulate(&[SIM_TIME_NS]);
    assert_eq!(pit_single.len() as u64, SIM_TIME_NS / PIT_PERIOD_NS);
    assert_eq!(hpet_single.len() as u64, SIM_TIME_NS / HPET_PERIOD_NS);

    // Chunked advances must result in identical interrupt times.
    let (pit_chunked, hpet_chunked) = simulate(&[3_333_333, 2_222_222, 1_111_111, 3_333_334]);
    assert_eq!(pit_single, pit_chunked);
    assert_eq!(hpet_single, hpet_chunked);
}
