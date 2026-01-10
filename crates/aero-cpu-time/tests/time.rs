use std::sync::Arc;

use aero_cpu_time::{CpuidModel, TimeInstructions};
use aero_timers::{ApicTimerMode, DeviceTimer};
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

#[test]
fn rdtsc_frequency_and_monotonicity() {
    let host = Arc::new(FakeHostClock::new(0));
    let time = TimeSource::new(host.clone());

    let mut cpu = TimeInstructions::new(2_000_000_000, 1_000_000_000);

    cpu.instr_rdtsc(time.now_ns());
    assert_eq!(cpu.regs.rdx << 32 | cpu.regs.rax, 0);

    host.set_ns(1_000_000_000);
    cpu.instr_rdtsc(time.now_ns());
    assert_eq!(cpu.regs.rdx << 32 | cpu.regs.rax, 2_000_000_000);

    host.set_ns(1_000_000_100);
    cpu.instr_rdtsc(time.now_ns());
    let tsc = cpu.regs.rdx << 32 | cpu.regs.rax;
    assert!(tsc > 2_000_000_000);
}

#[test]
fn rdtscp_sets_aux() {
    let host = Arc::new(FakeHostClock::new(0));
    let time = TimeSource::new(host.clone());

    let mut cpu = TimeInstructions::new(1_000_000_000, 1_000_000_000);
    cpu.tsc.set_aux(0xBEEF_F00D);

    host.set_ns(123);
    cpu.instr_rdtscp(time.now_ns());
    assert_eq!(cpu.regs.rcx as u32, 0xBEEF_F00D);
}

#[test]
fn cpuid_reports_tsc_features() {
    let mut cpu = TimeInstructions::new(1_000_000_000, 1_000_000_000);
    cpu.apic_timer.set_mode(ApicTimerMode::TscDeadline);
    cpu.apic_timer.set_masked(false);

    let model = CpuidModel::for_time(&cpu.tsc, &cpu.apic_timer);
    let leaf1 = model.cpuid(0x0000_0001, 0);
    assert_ne!(leaf1.edx & (1 << 4), 0);
    assert_ne!(leaf1.ecx & (1 << 24), 0);

    let leaf8000_0001 = model.cpuid(0x8000_0001, 0);
    assert_ne!(leaf8000_0001.ecx & (1 << 27), 0);

    let leaf8000_0007 = model.cpuid(0x8000_0007, 0);
    assert_ne!(leaf8000_0007.edx & (1 << 8), 0);
}

#[test]
fn tsc_deadline_schedules_local_apic_timer() {
    let host = Arc::new(FakeHostClock::new(0));
    let time = TimeSource::new(host.clone());

    let mut queue = TimerQueue::<DeviceTimer>::new();
    let mut cpu = TimeInstructions::new(1_000_000_000, 1_000_000_000);
    let mut sink = InterruptRecorder::default();

    cpu.apic_timer.set_mode(ApicTimerMode::TscDeadline);
    cpu.apic_timer.set_masked(false);
    cpu.apic_timer.set_vector(0x40);

    cpu.wrmsr(0x6E0, 1_000_000_000, time.now_ns(), &mut queue)
        .unwrap();

    host.set_ns(1_000_000_000);
    while let Some(ev) = queue.pop_due(time.now_ns()) {
        cpu.apic_timer
            .handle_timer_event(ev.deadline_ns, time.now_ns(), &mut queue, &mut sink);
    }

    assert_eq!(sink.events, vec![(1_000_000_000, Interrupt::Vector(0x40))]);
}
