#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::mem::FlatTestBus;
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
fn make_repeated_code(pattern: &[u8], insts: usize) -> Vec<u8> {
    let mut code = Vec::with_capacity(pattern.len() * insts + 16);
    for _ in 0..insts {
        code.extend_from_slice(pattern);
    }
    // Padding so the last instruction fetch always has lookahead bytes.
    code.extend(std::iter::repeat_n(0x90, 16));
    code
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_tier0_alu(c: &mut Criterion) {
    // add rax, rbx (64-bit)
    const ADD_RAX_RBX: &[u8] = &[0x48, 0x01, 0xD8];
    // add eax, ebx (32-bit)
    const ADD_EAX_EBX: &[u8] = &[0x01, 0xD8];
    // Keep each Criterion iteration reasonably short so the CI profile (400ms target time)
    // doesn't need to auto-extend the measurement window.
    const INSTS_PER_ITER: u64 = 5_000;

    let code64 = make_repeated_code(ADD_RAX_RBX, INSTS_PER_ITER as usize);
    let code32 = make_repeated_code(ADD_EAX_EBX, INSTS_PER_ITER as usize);

    let mut bus64 = FlatTestBus::new(code64.len());
    bus64.load(0, &code64);

    let mut bus32 = FlatTestBus::new(code32.len());
    bus32.load(0, &code32);

    // Sanity-check the setup once outside the measurement loop.
    let mut state64 = CpuState::new(CpuMode::Long);
    state64.set_rip(0);
    state64.gpr[gpr::RAX] = 0;
    state64.gpr[gpr::RBX] = 1;
    let res = run_batch(&mut state64, &mut bus64, 1);
    assert_eq!(res.exit, BatchExit::Completed);

    let mut state32 = CpuState::new(CpuMode::Bit32);
    state32.set_rip(0);
    state32.gpr[gpr::RAX] = 0;
    state32.gpr[gpr::RBX] = 1;
    let res = run_batch(&mut state32, &mut bus32, 1);
    assert_eq!(res.exit, BatchExit::Completed);

    let mut group = c.benchmark_group("tier0_alu");
    group.throughput(Throughput::Elements(INSTS_PER_ITER));
    group.bench_function("add_rax_rbx", |b| {
        b.iter(|| {
            state64.set_rip(0);
            state64.rflags = RFLAGS_RESERVED1;
            state64.lazy_flags = Default::default();
            state64.gpr[gpr::RAX] = 0;
            state64.gpr[gpr::RBX] = 1;

            let res = run_batch(
                black_box(&mut state64),
                black_box(&mut bus64),
                INSTS_PER_ITER,
            );
            debug_assert!(matches!(res.exit, BatchExit::Completed));
            black_box(res.executed);
            black_box(state64.gpr[gpr::RAX]);
        });
    });
    group.bench_function("add_eax_ebx", |b| {
        b.iter(|| {
            state32.set_rip(0);
            state32.rflags = RFLAGS_RESERVED1;
            state32.lazy_flags = Default::default();
            state32.gpr[gpr::RAX] = 0;
            state32.gpr[gpr::RBX] = 1;

            let res = run_batch(
                black_box(&mut state32),
                black_box(&mut bus32),
                INSTS_PER_ITER,
            );
            debug_assert!(matches!(res.exit, BatchExit::Completed));
            black_box(res.executed);
            black_box(state32.gpr[gpr::RAX]);
        });
    });
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_tier0_alu
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
