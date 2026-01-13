use std::time::Duration;

use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{gpr, CpuMode, CpuState, RFLAGS_RESERVED1};
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

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

fn make_repeated_code(pattern: &[u8], insts: usize) -> Vec<u8> {
    let mut code = Vec::with_capacity(pattern.len() * insts + 16);
    for _ in 0..insts {
        code.extend_from_slice(pattern);
    }
    // Padding so the last instruction fetch always has lookahead bytes.
    code.extend(std::iter::repeat_n(0x90, 16));
    code
}

fn bench_tier0_add_rax_rbx(c: &mut Criterion) {
    // add rax, rbx
    const ADD_RAX_RBX: &[u8] = &[0x48, 0x01, 0xD8];
    // Keep each Criterion iteration reasonably short so the CI profile (400ms target time)
    // doesn't need to auto-extend the measurement window.
    const INSTS_PER_ITER: u64 = 25_000;

    let code = make_repeated_code(ADD_RAX_RBX, INSTS_PER_ITER as usize);

    let mut bus = FlatTestBus::new(code.len());
    bus.load(0, &code);

    let mut state = CpuState::new(CpuMode::Long);
    state.set_rip(0);
    state.gpr[gpr::RAX] = 0;
    state.gpr[gpr::RBX] = 1;

    // Sanity-check the setup once outside the measurement loop.
    let res = run_batch(&mut state, &mut bus, 1);
    assert_eq!(res.exit, BatchExit::Completed);

    let mut group = c.benchmark_group("tier0_alu");
    group.throughput(Throughput::Elements(INSTS_PER_ITER));
    group.bench_function("add_rax_rbx", |b| {
        b.iter(|| {
            state.set_rip(0);
            state.rflags = RFLAGS_RESERVED1;
            state.lazy_flags = Default::default();
            state.gpr[gpr::RAX] = 0;
            state.gpr[gpr::RBX] = 1;

            let res = run_batch(black_box(&mut state), black_box(&mut bus), INSTS_PER_ITER);
            debug_assert!(matches!(res.exit, BatchExit::Completed));
            black_box(res.executed);
            black_box(state.gpr[gpr::RAX]);
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_tier0_add_rax_rbx
}
criterion_main!(benches);
