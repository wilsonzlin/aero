use aero_cpu_core::{Bus, Cpu, CpuMode, RamBus};
use aero_perf::{PerfCounters, PerfWorker};
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

    // Use 16-bit mode so SI/DI wrap naturally, avoiding bounds checks even for
    // very large iteration counts.
    let mut cpu_base = Cpu::new(CpuMode::Real16);
    cpu_base.segs.ds.base = 0x0000;
    cpu_base.segs.es.base = 0x8000;
    cpu_base.regs.set_si(0);
    cpu_base.regs.set_di(0);

    let mut bus_base = RamBus::new(0x20_000);
    for i in 0..0x10_000u64 {
        bus_base.write_u8(i, (i & 0xFF) as u8);
    }

    let start = Instant::now();
    for _ in 0..iterations {
        cpu_base.execute_bytes(&mut bus_base, &[0xA4]).unwrap(); // MOVSB
    }
    let dt_base = start.elapsed();
    black_box(&cpu_base);
    black_box(&bus_base);

    let mut cpu_count = Cpu::new(CpuMode::Real16);
    cpu_count.segs.ds.base = 0x0000;
    cpu_count.segs.es.base = 0x8000;
    cpu_count.regs.set_si(0);
    cpu_count.regs.set_di(0);
    let mut bus_count = bus_base.clone();

    let shared = Arc::new(PerfCounters::new());
    let mut perf = PerfWorker::new(shared);

    let start = Instant::now();
    for _ in 0..iterations {
        cpu_count
            .execute_bytes_counted(&mut bus_count, &[0xA4], &mut perf)
            .unwrap();
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
