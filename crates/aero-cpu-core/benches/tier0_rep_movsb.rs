#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::state::{gpr, CpuMode, CpuState, RFLAGS_RESERVED1};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

#[cfg(not(target_arch = "wasm32"))]
fn criterion_config() -> Criterion {
    // Default to a CI-friendly profile so `cargo bench` completes under `scripts/safe-run.sh`'s
    // default timeout. Opt into longer runs explicitly with `AERO_BENCH_PROFILE=full`.
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("full") => Criterion::default()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(50)
            .noise_threshold(0.03),
        _ => Criterion::default()
            // Keep PR/CI runtime low.
            .warm_up_time(Duration::from_millis(150))
            .measurement_time(Duration::from_millis(400))
            .sample_size(20)
            .noise_threshold(0.05),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_tier0_rep_movsb(c: &mut Criterion) {
    const CODE_MOVSB_ADDR: u64 = 0x0000;
    const CODE_STOSB_ADDR: u64 = 0x0100;
    const SRC_ADDR: u64 = 0x10_000;
    const DST_ADDR: u64 = 0x20_000;
    const LEN: usize = 64 * 1024;

    // rep movsb
    let mut code_movsb = vec![0xF3, 0xA4];
    // Padding so instruction fetch always has lookahead bytes.
    code_movsb.extend(std::iter::repeat_n(0x90, 16));

    // rep stosb
    let mut code_stosb = vec![0xF3, 0xAA];
    code_stosb.extend(std::iter::repeat_n(0x90, 16));

    let bus_len = (DST_ADDR + LEN as u64 + 0x1000) as usize;
    let mut bus_base = FlatTestBus::new(bus_len);
    bus_base.load(CODE_MOVSB_ADDR, &code_movsb);
    bus_base.load(CODE_STOSB_ADDR, &code_stosb);

    // Deterministic source pattern (one-time init).
    let mut src = Vec::with_capacity(LEN);
    for i in 0..LEN {
        src.push((i as u8).wrapping_mul(3) ^ 0x5A);
    }
    bus_base.load(SRC_ADDR, &src);

    let mut bus_long = bus_base.clone();
    let mut bus_32 = bus_base;

    // Sanity-check once outside measurement: should complete without assist/exception.
    let mut state_long = CpuState::new(CpuMode::Long);
    state_long.set_rip(CODE_MOVSB_ADDR);
    state_long.gpr[gpr::RSI] = SRC_ADDR;
    state_long.gpr[gpr::RDI] = DST_ADDR;
    state_long.gpr[gpr::RCX] = LEN as u64;
    let res = run_batch(&mut state_long, &mut bus_long, 1);
    assert_eq!(res.exit, BatchExit::Completed);
    // Verify that bulk-copy did something observable.
    assert_eq!(bus_long.read_u8(DST_ADDR).unwrap(), src[0]);

    let mut state_32 = CpuState::new(CpuMode::Bit32);
    state_32.set_rip(CODE_MOVSB_ADDR);
    state_32.gpr[gpr::RSI] = SRC_ADDR;
    state_32.gpr[gpr::RDI] = DST_ADDR;
    state_32.gpr[gpr::RCX] = LEN as u64;
    let res = run_batch(&mut state_32, &mut bus_32, 1);
    assert_eq!(res.exit, BatchExit::Completed);
    assert_eq!(bus_32.read_u8(DST_ADDR).unwrap(), src[0]);

    let mut group = c.benchmark_group("tier0_string");
    group.throughput(Throughput::Bytes(LEN as u64));
    group.bench_function("rep_movsb_64kib", |b| {
        b.iter(|| {
            state_long.set_rip(CODE_MOVSB_ADDR);
            state_long.rflags = RFLAGS_RESERVED1;
            state_long.lazy_flags = Default::default();
            state_long.gpr[gpr::RSI] = SRC_ADDR;
            state_long.gpr[gpr::RDI] = DST_ADDR;
            state_long.gpr[gpr::RCX] = LEN as u64;

            let res = run_batch(black_box(&mut state_long), black_box(&mut bus_long), 1);
            debug_assert!(matches!(res.exit, BatchExit::Completed));
            black_box(res.executed);
        });
    });
    group.bench_function("rep_movsb_64kib_32", |b| {
        b.iter(|| {
            state_32.set_rip(CODE_MOVSB_ADDR);
            state_32.rflags = RFLAGS_RESERVED1;
            state_32.lazy_flags = Default::default();
            state_32.gpr[gpr::RSI] = SRC_ADDR;
            state_32.gpr[gpr::RDI] = DST_ADDR;
            state_32.gpr[gpr::RCX] = LEN as u64;

            let res = run_batch(black_box(&mut state_32), black_box(&mut bus_32), 1);
            debug_assert!(matches!(res.exit, BatchExit::Completed));
            black_box(res.executed);
        });
    });
    group.bench_function("rep_stosb_64kib", |b| {
        b.iter(|| {
            state_long.set_rip(CODE_STOSB_ADDR);
            state_long.rflags = RFLAGS_RESERVED1;
            state_long.lazy_flags = Default::default();
            state_long.gpr[gpr::RAX] = 0x5A; // AL
            state_long.gpr[gpr::RDI] = DST_ADDR;
            state_long.gpr[gpr::RCX] = LEN as u64;

            let res = run_batch(black_box(&mut state_long), black_box(&mut bus_long), 1);
            debug_assert!(matches!(res.exit, BatchExit::Completed));
            black_box(res.executed);
        });
    });
    group.bench_function("rep_stosb_64kib_32", |b| {
        b.iter(|| {
            state_32.set_rip(CODE_STOSB_ADDR);
            state_32.rflags = RFLAGS_RESERVED1;
            state_32.lazy_flags = Default::default();
            state_32.gpr[gpr::RAX] = 0x5A; // AL
            state_32.gpr[gpr::RDI] = DST_ADDR;
            state_32.gpr[gpr::RCX] = LEN as u64;

            let res = run_batch(black_box(&mut state_32), black_box(&mut bus_32), 1);
            debug_assert!(matches!(res.exit, BatchExit::Completed));
            black_box(res.executed);
        });
    });
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_tier0_rep_movsb
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
