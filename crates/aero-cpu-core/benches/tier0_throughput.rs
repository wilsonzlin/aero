// Criterion benchmarks for Tier-0 interpreter throughput (`interp::tier0`).
//
// These benches intentionally use the identity-mapped `FlatTestBus` so results
// primarily reflect Tier-0 fetch/decode/execute overhead rather than paging/MMU
// costs.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::mem::{CpuBus as _, FlatTestBus};
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
fn bench_tier0_throughput(c: &mut Criterion) {
    // Keep benches deterministic and allocation-free inside the measured loops.

    // --- tier0_nop_stream ----------------------------------------------------
    // `INSTS_PER_ITER` should be large enough that per-iteration overhead (resetting RIP,
    // criterion bookkeeping) is negligible compared to Tier-0 execution.
    const INSTS_PER_ITER: u64 = 100_000;
    // Add padding so Tier-0 can always fetch 15 bytes of lookahead at the end of the stream.
    let nop_stream = vec![0x90u8; (INSTS_PER_ITER as usize) + 16];

    let mut nop_bus = FlatTestBus::new(nop_stream.len());
    nop_bus.load(0, &nop_stream);

    let mut nop_state = CpuState::new(CpuMode::Long);
    nop_state.set_rip(0);

    // --- tier0_rep_movsb -----------------------------------------------------
    const CODE_ADDR: u64 = 0x0000;
    const SRC_ADDR: u64 = 0x10_000;
    const DST_ADDR: u64 = 0x20_000;
    const LEN: usize = 64 * 1024;

    // rep movsb
    let mut rep_code = vec![0xF3, 0xA4];
    rep_code.extend(std::iter::repeat_n(0x90, 16));

    let bus_len = (DST_ADDR + LEN as u64 + 0x1000) as usize;
    let mut rep_bus = FlatTestBus::new(bus_len);
    rep_bus.load(CODE_ADDR, &rep_code);

    // Deterministic source pattern (one-time init).
    let mut src = Vec::with_capacity(LEN);
    for i in 0..LEN {
        src.push((i as u8).wrapping_mul(3) ^ 0x5A);
    }
    rep_bus.load(SRC_ADDR, &src);

    let mut rep_state = CpuState::new(CpuMode::Long);
    rep_state.set_rip(CODE_ADDR);
    rep_state.gpr[gpr::RSI] = SRC_ADDR;
    rep_state.gpr[gpr::RDI] = DST_ADDR;
    rep_state.gpr[gpr::RCX] = LEN as u64;

    // Sanity-check once outside measurement: should complete without assist/exception.
    let res = run_batch(&mut rep_state, &mut rep_bus, 1);
    assert_eq!(res.exit, BatchExit::Completed);
    // Verify that the copy did something observable.
    assert_eq!(rep_bus.read_u8(DST_ADDR).unwrap(), src[0]);

    let mut group = c.benchmark_group("tier0_throughput");

    group.throughput(Throughput::Elements(INSTS_PER_ITER));
    group.bench_function("tier0_nop_stream", |b| {
        b.iter(|| {
            nop_state.set_rip(0);
            let res = run_batch(black_box(&mut nop_state), black_box(&mut nop_bus), INSTS_PER_ITER);
            debug_assert!(matches!(res.exit, BatchExit::Completed));
            black_box(res.executed);
        })
    });

    // One REP MOVSB copies `LEN` bytes. Report bytes/sec.
    group.throughput(Throughput::Bytes(LEN as u64));
    group.bench_function("tier0_rep_movsb", |b| {
        b.iter(|| {
            rep_state.set_rip(CODE_ADDR);
            rep_state.rflags = RFLAGS_RESERVED1;
            rep_state.lazy_flags = Default::default();
            rep_state.gpr[gpr::RSI] = SRC_ADDR;
            rep_state.gpr[gpr::RDI] = DST_ADDR;
            rep_state.gpr[gpr::RCX] = LEN as u64;

            let res = run_batch(black_box(&mut rep_state), black_box(&mut rep_bus), 1);
            debug_assert!(matches!(res.exit, BatchExit::Completed));

            // Prevent the optimizer from treating the copy as dead.
            black_box(rep_bus.read_u8(DST_ADDR + (LEN as u64 - 1)).unwrap());
        })
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_tier0_throughput
}

#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
