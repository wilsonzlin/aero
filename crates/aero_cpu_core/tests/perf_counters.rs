#![cfg(feature = "legacy-interp")]

use aero_cpu_core::{Bus, Cpu, CpuMode, RamBus};
use aero_perf::{PerfCounters, PerfWorker};
use std::sync::Arc;

fn setup_bus() -> RamBus {
    RamBus::new(0x10_000)
}

#[test]
fn execute_bytes_counted_increments_instruction_counter() {
    let mut cpu = Cpu::new(CpuMode::Real16);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_si(0x10);
    cpu.regs.set_di(0x20);

    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x10, 0xAA);

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);
    cpu.execute_bytes_counted(&mut bus, &[0xA4], &mut perf).unwrap(); // MOVSB

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 1);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 0);
}

#[test]
fn rep_string_iterations_are_tracked_separately() {
    let mut cpu = Cpu::new(CpuMode::Real16);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_si(0x10);
    cpu.regs.set_di(0x20);
    cpu.regs.set_cx(3);

    let mut bus = setup_bus();
    bus.write_u8(0x1000 + 0x10, 0x11);
    bus.write_u8(0x1000 + 0x11, 0x22);
    bus.write_u8(0x1000 + 0x12, 0x33);

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);
    cpu.execute_bytes_counted(&mut bus, &[0xF3, 0xA4], &mut perf)
        .unwrap(); // REP MOVSB

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 1);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 3);
    assert_eq!(cpu.regs.cx(), 0);
}

#[test]
fn repe_cmpsb_reports_actual_iterations_executed() {
    let mut cpu = Cpu::new(CpuMode::Protected32);
    cpu.segs.ds.base = 0x1000;
    cpu.segs.es.base = 0x2000;
    cpu.regs.set_esi(0x10, CpuMode::Protected32);
    cpu.regs.set_edi(0x20, CpuMode::Protected32);
    cpu.regs.set_ecx(5, CpuMode::Protected32);

    let mut bus = setup_bus();
    // First 3 bytes match, 4th differs.
    for i in 0..5 {
        bus.write_u8(0x1000 + 0x10 + i, if i == 3 { 0x99 } else { i as u8 });
        bus.write_u8(0x2000 + 0x20 + i, i as u8);
    }

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);
    cpu.execute_bytes_counted(&mut bus, &[0xF3, 0xA6], &mut perf)
        .unwrap(); // REPE CMPSB

    assert_eq!(perf.lifetime_snapshot().instructions_executed, 1);
    assert_eq!(perf.lifetime_snapshot().rep_iterations, 4);
    assert_eq!(cpu.regs.ecx(), 1);
}
