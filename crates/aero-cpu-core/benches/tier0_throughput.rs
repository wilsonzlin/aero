// Criterion bench for the canonical Tier-0 interpreter (no `legacy-interp`).
//
// Keep this bench self-contained and deterministic: it executes short guest code
// sequences from an in-memory `FlatTestBus` and does not rely on I/O.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::exec::{Interpreter as _, Tier0Interpreter, Vcpu};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::interrupts::CpuCore;
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::state::CpuMode;
#[cfg(not(target_arch = "wasm32"))]
use aero_x86::Register;
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
fn make_alu_loop_code(unroll_adds: usize) -> Vec<u8> {
    // loop:
    //   add rax, 1        (repeated `unroll_adds` times)
    //   dec rcx
    //   jnz loop
    //   hlt
    //
    // Notes:
    // - Use a rel32 JNZ (0F 85 cd) so we can freely change `unroll_adds`.
    // - Keep the program small and self-contained; the host sets RCX.
    let mut code = Vec::with_capacity(unroll_adds * 4 + 3 + 6 + 1);
    for _ in 0..unroll_adds {
        // 48 83 C0 01: add rax, imm8(1)
        code.extend_from_slice(&[0x48, 0x83, 0xC0, 0x01]);
    }
    // 48 FF C9: dec rcx
    code.extend_from_slice(&[0x48, 0xFF, 0xC9]);

    // 0F 85 <disp32>: jnz rel32
    code.extend_from_slice(&[0x0F, 0x85]);
    let after_jnz = (code.len() + 4) as i64; // +4 for disp32 itself
    let disp = -after_jnz;
    code.extend_from_slice(&(disp as i32).to_le_bytes());

    // F4: hlt
    code.push(0xF4);

    code
}

#[cfg(not(target_arch = "wasm32"))]
fn run_to_halt<B: CpuBus>(cpu: &mut Vcpu<B>, interp: &mut Tier0Interpreter, max_blocks: u64) {
    for _ in 0..max_blocks {
        if let Some(exit) = cpu.exit {
            panic!("unexpected CPU exit: {exit:?}");
        }
        if cpu.cpu.state.halted {
            return;
        }
        interp.exec_block(cpu);
    }
    panic!("program did not halt");
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_tier0_alu_tight_loop(c: &mut Criterion) {
    let code_base = 0u64;

    // "Tight" loop with a branch every 3 guest instructions.
    let unroll_adds = 1usize;
    let loops_rcx = 50_000u64;
    let total_insts = loops_rcx * (unroll_adds as u64 + 2) + 1;
    let expected_rax = loops_rcx * unroll_adds as u64;

    let code = make_alu_loop_code(unroll_adds);
    let mut bus = FlatTestBus::new(0x4000);
    bus.load(code_base, &code);

    let mut interp = Tier0Interpreter::new(64);
    let mut cpu = Vcpu::new_with_mode(CpuMode::Long, bus);

    // Pre-flight sanity check so the benchmark doesn't silently run garbage / hang.
    cpu.cpu = CpuCore::new(CpuMode::Long);
    cpu.exit = None;
    cpu.cpu.state.set_rip(code_base);
    cpu.cpu.state.write_reg(Register::RAX, 0);
    cpu.cpu.state.write_reg(Register::RCX, loops_rcx);
    run_to_halt(&mut cpu, &mut interp, loops_rcx + 4);
    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.read_reg(Register::RAX), expected_rax);

    let mut group = c.benchmark_group("tier0/alu_tight");
    group.throughput(Throughput::Elements(total_insts));
    group.bench_function("add_dec_jnz", |b| {
        b.iter(|| {
            cpu.cpu = CpuCore::new(CpuMode::Long);
            cpu.exit = None;
            cpu.cpu.state.set_rip(code_base);
            cpu.cpu.state.write_reg(Register::RAX, 0);
            cpu.cpu.state.write_reg(Register::RCX, loops_rcx);

            run_to_halt(&mut cpu, &mut interp, loops_rcx + 4);
            black_box(cpu.cpu.state.read_reg(Register::RAX));
        });
    });
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_tier0_alu_unrolled_loop(c: &mut Criterion) {
    let code_base = 0u64;

    // A lower-branch-density loop to approximate steady-state ALU throughput while
    // still exercising basic-block termination on JNZ.
    let unroll_adds = 256usize;
    let loops_rcx = 1024u64;
    let total_insts = loops_rcx * (unroll_adds as u64 + 2) + 1;
    let expected_rax = loops_rcx * unroll_adds as u64;

    let code = make_alu_loop_code(unroll_adds);
    let mut bus = FlatTestBus::new(0x8000);
    bus.load(code_base, &code);

    let mut interp = Tier0Interpreter::new(1024);
    let mut cpu = Vcpu::new_with_mode(CpuMode::Long, bus);

    // Pre-flight sanity check.
    cpu.cpu = CpuCore::new(CpuMode::Long);
    cpu.exit = None;
    cpu.cpu.state.set_rip(code_base);
    cpu.cpu.state.write_reg(Register::RAX, 0);
    cpu.cpu.state.write_reg(Register::RCX, loops_rcx);
    run_to_halt(&mut cpu, &mut interp, loops_rcx + 8);
    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.read_reg(Register::RAX), expected_rax);

    let mut group = c.benchmark_group("tier0/alu_unrolled");
    group.throughput(Throughput::Elements(total_insts));
    group.bench_function("add_x256_dec_jnz", |b| {
        b.iter(|| {
            cpu.cpu = CpuCore::new(CpuMode::Long);
            cpu.exit = None;
            cpu.cpu.state.set_rip(code_base);
            cpu.cpu.state.write_reg(Register::RAX, 0);
            cpu.cpu.state.write_reg(Register::RCX, loops_rcx);

            run_to_halt(&mut cpu, &mut interp, loops_rcx + 8);
            black_box(cpu.cpu.state.read_reg(Register::RAX));
        });
    });
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_tier0_rep_movsb(c: &mut Criterion) {
    let code_base = 0u64;
    // rep movsb; hlt
    let code = [0xF3, 0xA4, 0xF4];

    let copy_len = 4 * 1024u64;
    let src = 0x1000u64;
    let dst = 0x4000u64;

    let mut bus = FlatTestBus::new(0x10000);
    bus.load(code_base, &code);
    // Deterministic source buffer contents.
    for i in 0..copy_len {
        bus.write_u8(src + i, (i as u8).wrapping_mul(31)).unwrap();
    }

    let mut interp = Tier0Interpreter::new(64);
    let mut cpu = Vcpu::new_with_mode(CpuMode::Long, bus);

    // Pre-flight sanity check.
    let check_idx = 123u64;
    cpu.cpu = CpuCore::new(CpuMode::Long);
    cpu.exit = None;
    cpu.cpu.state.set_rip(code_base);
    cpu.cpu.state.write_reg(Register::RSI, src);
    cpu.cpu.state.write_reg(Register::RDI, dst);
    cpu.cpu.state.write_reg(Register::RCX, copy_len);
    run_to_halt(&mut cpu, &mut interp, 8);
    assert!(cpu.cpu.state.halted);

    // Verify a single byte so we know the copy actually happened.
    assert_eq!(
        cpu.bus.read_u8(dst + check_idx).unwrap(),
        (check_idx as u8).wrapping_mul(31)
    );

    let mut group = c.benchmark_group("tier0/rep_movsb");
    group.throughput(Throughput::Bytes(copy_len));
    group.bench_function("4kib", |b| {
        b.iter(|| {
            cpu.cpu = CpuCore::new(CpuMode::Long);
            cpu.exit = None;
            cpu.cpu.state.set_rip(code_base);
            cpu.cpu.state.write_reg(Register::RSI, src);
            cpu.cpu.state.write_reg(Register::RDI, dst);
            cpu.cpu.state.write_reg(Register::RCX, copy_len);

            run_to_halt(&mut cpu, &mut interp, 8);
            black_box(cpu.bus.read_u8(dst + check_idx).unwrap());
        });
    });
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_tier0_alu_tight_loop, bench_tier0_alu_unrolled_loop, bench_tier0_rep_movsb
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
