use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_perf::{PerfCounters, PerfWorker};
use aero_x86::Register;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let iterations: usize = std::env::args()
        .nth(1)
        .as_deref()
        .unwrap_or("5000000")
        .parse()
        .expect("iterations must be an integer");

    const CS_BASE: u64 = 0x20_000;

    // Use 16-bit mode so SI/DI/IP wrap naturally, avoiding bounds checks even for
    // very large iteration counts.
    let mut cpu_base = CpuState::new(CpuMode::Bit16);
    cpu_base.segments.cs.base = CS_BASE;
    cpu_base.segments.ds.base = 0x0000;
    cpu_base.segments.es.base = 0x8000;
    cpu_base.write_reg(Register::SI, 0);
    cpu_base.write_reg(Register::DI, 0);
    cpu_base.set_rip(0);

    let mut bus_base = FlatTestBus::new(0x30_000);
    // Initialize DS memory with a simple repeating pattern.
    for i in 0..0x10_000u64 {
        bus_base.write_u8(i, (i & 0xFF) as u8).unwrap();
    }
    // Place a MOVSB instruction stream at CS:0.. so IP can wrap without needing
    // to reset RIP in the hot loop.
    for i in 0..0x10_000u64 {
        bus_base.write_u8(CS_BASE + i, 0xA4).unwrap(); // MOVSB
    }

    let start = Instant::now();
    for _ in 0..iterations {
        let res = run_batch(&mut cpu_base, &mut bus_base, 1);
        assert_eq!(res.exit, BatchExit::Completed);
    }
    let dt_base = start.elapsed();
    black_box(&cpu_base);
    black_box(&bus_base);

    let mut cpu_count = CpuState::new(CpuMode::Bit16);
    cpu_count.segments.cs.base = CS_BASE;
    cpu_count.segments.ds.base = 0x0000;
    cpu_count.segments.es.base = 0x8000;
    cpu_count.write_reg(Register::SI, 0);
    cpu_count.write_reg(Register::DI, 0);
    cpu_count.set_rip(0);
    let mut bus_count = bus_base.clone();

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);

    let start = Instant::now();
    for _ in 0..iterations {
        let res = run_batch(&mut cpu_count, &mut bus_count, 1);
        assert_eq!(res.exit, BatchExit::Completed);
        perf.retire_instructions(1);
    }
    let dt_count = start.elapsed();

    let retired = perf.lifetime_snapshot().instructions_executed;
    assert_eq!(retired, iterations as u64);

    let base_s = dt_base.as_secs_f64();
    let count_s = dt_count.as_secs_f64();
    let base_ips = (iterations as f64) / base_s;
    let count_ips = (iterations as f64) / count_s;
    let overhead = ((count_s / base_s) - 1.0) * 100.0;

    println!("iterations: {iterations}");
    println!("baseline:  {base_ips:.0} IPS ({base_s:.3}s)");
    println!("counting:  {count_ips:.0} IPS ({count_s:.3}s)");
    println!("overhead:  {overhead:.2}%");
}
