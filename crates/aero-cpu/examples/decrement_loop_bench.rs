use std::time::Instant;

use aero_cpu::baseline::{CpuWorker, Interpreter, JitConfig, Memory};
use aero_jit_proto::cpu::{CpuState, Reg};

fn main() {
    // mov rcx, N
    // loop: sub rcx, 1
    //       jne loop
    //       hlt
    //
    // Run with:
    //   cargo run -p aero-cpu --example decrement_loop_bench --release
    let n: u64 = 25_000_000;
    let code = build_decrement_loop(n);

    let interp_time = {
        let mut cpu = CpuState::default();
        cpu.rip = 0;
        let mut mem = Memory::new(64 * 1024);
        mem.load(0, &code);
        let interp = Interpreter::default();

        let start = Instant::now();
        let mut steps = 0u64;
        while !cpu.is_halted() {
            interp.step(&mut cpu, &mut mem).unwrap();
            steps += 1;
        }
        let elapsed = start.elapsed();
        eprintln!(
            "interpreter: {:?} ({} steps, rcx={})",
            elapsed,
            steps,
            cpu.reg(Reg::Rcx)
        );
        elapsed
    };

    let jit_time = {
        let mut mem = Memory::new(64 * 1024);
        mem.load(0, &code);
        let mut worker = CpuWorker::new(mem).with_config(JitConfig {
            hot_threshold: 1,
            max_block_insts: 64,
            max_block_bytes: 512,
        });
        worker.cpu.rip = 0;

        let start = Instant::now();
        worker.run(n + 10).unwrap();
        let elapsed = start.elapsed();
        eprintln!(
            "jit (baseline): {:?} ({} blocks, rcx={})",
            elapsed,
            n + 10,
            worker.cpu.reg(Reg::Rcx)
        );
        elapsed
    };

    let speedup = interp_time.as_secs_f64() / jit_time.as_secs_f64();
    eprintln!("speedup: {:.2}x", speedup);
}

fn build_decrement_loop(n: u64) -> Vec<u8> {
    let mut code = Vec::new();
    code.extend_from_slice(&[0x48, 0xB9]); // mov rcx, imm64
    code.extend_from_slice(&n.to_le_bytes());
    code.extend_from_slice(&[0x48, 0x83, 0xE9, 0x01]); // sub rcx, 1
    code.extend_from_slice(&[0x75, 0xFA]); // jne -6
    code.push(0xF4); // hlt
    code
}
