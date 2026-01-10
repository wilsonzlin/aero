use std::sync::Arc;

use aero_timers::{ApicTimerMode, DeviceTimer, Hpet, HpetTimerConfig, LocalApicTimer, Pit};
use aero_time::{FakeHostClock, Interrupt, InterruptSink, TimeSource, TimerQueue};

#[derive(Default)]
struct InterruptRecorder {
    events: Vec<(u64, Interrupt)>,
}

impl InterruptSink for InterruptRecorder {
    fn raise(&mut self, interrupt: Interrupt, at_ns: u64) {
        self.events.push((at_ns, interrupt));
    }
}

fn pump(
    guest_now_ns: u64,
    queue: &mut TimerQueue<DeviceTimer>,
    pit: &mut Pit,
    hpet: &mut Hpet,
    apic: &mut LocalApicTimer,
    sink: &mut InterruptRecorder,
) {
    while let Some(ev) = queue.pop_due(guest_now_ns) {
        match ev.payload {
            DeviceTimer::PitChannel0 => pit.handle_timer_event(ev.deadline_ns, queue, sink),
            DeviceTimer::HpetTimer0 => {
                hpet.handle_timer0_event(ev.deadline_ns, guest_now_ns, queue, sink)
            }
            DeviceTimer::LocalApicTimer => {
                apic.handle_timer_event(ev.deadline_ns, guest_now_ns, queue, sink)
            }
        }
    }
}

#[test]
fn pit_periodic_irq0() {
    let host = Arc::new(FakeHostClock::new(0));
    let time = TimeSource::new(host.clone());

    let mut queue = TimerQueue::<DeviceTimer>::new();
    let mut pit = Pit::new();
    let mut hpet = Hpet::default();
    let mut apic = LocalApicTimer::default();
    let mut sink = InterruptRecorder::default();

    pit.write_command(0x34); // ch0, lo/hi, mode 2
    let now = time.now_ns();
    pit.write_channel0_data(0xE8, now, &mut queue); // 1000
    pit.write_channel0_data(0x03, now, &mut queue);

    // 1000 PIT ticks => 1000 / 1_193_182 seconds.
    let period_ns = (1000u128 * 1_000_000_000u128 + 1_193_182u128 - 1) / 1_193_182u128;
    for n in 1..=5u64 {
        host.set_ns((period_ns as u64) * n);
        let now = time.now_ns();
        pump(now, &mut queue, &mut pit, &mut hpet, &mut apic, &mut sink);
    }

    assert_eq!(sink.events.len(), 5);
    assert!(sink.events.iter().all(|(_, i)| *i == Interrupt::Irq(0)));
    assert_eq!(sink.events[0].0, period_ns as u64);
}

#[test]
fn hpet_comparator_one_shot() {
    let host = Arc::new(FakeHostClock::new(0));
    let time = TimeSource::new(host.clone());

    let mut queue = TimerQueue::<DeviceTimer>::new();
    let mut pit = Pit::default();
    let mut hpet = Hpet::default();
    let mut apic = LocalApicTimer::default();
    let mut sink = InterruptRecorder::default();

    hpet.set_enabled(true, time.now_ns(), &mut queue);
    hpet.configure_timer0(
        HpetTimerConfig {
            enabled: true,
            periodic: false,
            period_ticks: 0,
            irq: 2,
        },
        time.now_ns(),
        &mut queue,
    );
    hpet.set_timer0_comparator(1_000, time.now_ns(), &mut queue);

    // 1000 ticks at 10 MHz => 100 µs.
    host.set_ns(99_999);
    pump(
        time.now_ns(),
        &mut queue,
        &mut pit,
        &mut hpet,
        &mut apic,
        &mut sink,
    );
    assert!(sink.events.is_empty());

    host.set_ns(100_000);
    pump(
        time.now_ns(),
        &mut queue,
        &mut pit,
        &mut hpet,
        &mut apic,
        &mut sink,
    );
    assert_eq!(sink.events, vec![(100_000, Interrupt::Irq(2))]);
}

#[test]
fn apic_timer_periodic_vector() {
    let host = Arc::new(FakeHostClock::new(0));
    let time = TimeSource::new(host.clone());

    let mut queue = TimerQueue::<DeviceTimer>::new();
    let mut pit = Pit::default();
    let mut hpet = Hpet::default();
    let mut apic = LocalApicTimer::new(1_000_000); // 1 MHz for easy math
    let mut sink = InterruptRecorder::default();

    apic.set_masked(false);
    apic.set_vector(0x31);
    apic.set_divide(1);
    apic.set_mode(ApicTimerMode::Periodic);
    apic.write_initial_count(time.now_ns(), 250, &mut queue); // 250 µs period

    host.set_ns(250_000);
    pump(
        time.now_ns(),
        &mut queue,
        &mut pit,
        &mut hpet,
        &mut apic,
        &mut sink,
    );
    host.set_ns(500_000);
    pump(
        time.now_ns(),
        &mut queue,
        &mut pit,
        &mut hpet,
        &mut apic,
        &mut sink,
    );

    assert_eq!(
        sink.events,
        vec![
            (250_000, Interrupt::Vector(0x31)),
            (500_000, Interrupt::Vector(0x31)),
        ]
    );
}

#[test]
fn integration_smoke_pit_and_hpet() {
    let host = Arc::new(FakeHostClock::new(0));
    let time = TimeSource::new(host.clone());

    let mut queue = TimerQueue::<DeviceTimer>::new();
    let mut pit = Pit::default();
    let mut hpet = Hpet::default();
    let mut apic = LocalApicTimer::default();
    let mut sink = InterruptRecorder::default();

    pit.write_command(0x34); // mode 2
    pit.write_channel0_data(0xA9, time.now_ns(), &mut queue); // 11945-ish (about 100 Hz)
    pit.write_channel0_data(0x2E, time.now_ns(), &mut queue);

    hpet.set_enabled(true, time.now_ns(), &mut queue);
    hpet.configure_timer0(
        HpetTimerConfig {
            enabled: true,
            periodic: false,
            period_ticks: 0,
            irq: 2,
        },
        time.now_ns(),
        &mut queue,
    );
    hpet.set_timer0_comparator(500, time.now_ns(), &mut queue);

    host.set_ns(20_000_000);
    pump(
        time.now_ns(),
        &mut queue,
        &mut pit,
        &mut hpet,
        &mut apic,
        &mut sink,
    );

    assert!(!sink.events.is_empty());
    assert!(sink.events.iter().any(|(_, i)| *i == Interrupt::Irq(2)));
    assert!(sink.events.iter().any(|(_, i)| *i == Interrupt::Irq(0)));
}
