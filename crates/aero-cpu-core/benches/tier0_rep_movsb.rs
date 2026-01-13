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
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
            // Keep PR runtime low.
            .warm_up_time(Duration::from_millis(150))
            .measurement_time(Duration::from_millis(400))
            .sample_size(20)
            .noise_threshold(0.05),
        _ => Criterion::default()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(50)
            .noise_threshold(0.03),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_tier0_rep_movsb(c: &mut Criterion) {
    const CODE_ADDR: u64 = 0x0000;
    const SRC_ADDR: u64 = 0x10_000;
    const DST_ADDR: u64 = 0x20_000;
    const LEN: usize = 64 * 1024;

    // rep movsb
    let mut code = vec![0xF3, 0xA4];
    // Padding so instruction fetch always has lookahead bytes.
    code.extend(std::iter::repeat_n(0x90, 16));

    let bus_len = (DST_ADDR + LEN as u64 + 0x1000) as usize;
    let mut bus = FlatTestBus::new(bus_len);
    bus.load(CODE_ADDR, &code);

    // Deterministic source pattern (one-time init).
    let mut src = Vec::with_capacity(LEN);
    for i in 0..LEN {
        src.push((i as u8).wrapping_mul(3) ^ 0x5A);
    }
    bus.load(SRC_ADDR, &src);

    let mut state = CpuState::new(CpuMode::Long);
    state.set_rip(CODE_ADDR);
    state.gpr[gpr::RSI] = SRC_ADDR;
    state.gpr[gpr::RDI] = DST_ADDR;
    state.gpr[gpr::RCX] = LEN as u64;

    // Sanity-check once outside measurement: should complete without assist/exception.
    let res = run_batch(&mut state, &mut bus, 1);
    assert_eq!(res.exit, BatchExit::Completed);
    // Verify that bulk-copy did something observable.
    assert_eq!(bus.read_u8(DST_ADDR).unwrap(), src[0]);

    let mut group = c.benchmark_group("tier0_string");
    group.throughput(Throughput::Bytes(LEN as u64));
    group.bench_function("rep_movsb_64kib", |b| {
        b.iter(|| {
            state.set_rip(CODE_ADDR);
            state.rflags = RFLAGS_RESERVED1;
            state.lazy_flags = Default::default();
            state.gpr[gpr::RSI] = SRC_ADDR;
            state.gpr[gpr::RDI] = DST_ADDR;
            state.gpr[gpr::RCX] = LEN as u64;

            let res = run_batch(black_box(&mut state), black_box(&mut bus), 1);
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
