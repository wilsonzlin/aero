use std::time::Instant;

use aero_tier0::{CpuBus, CpuState, LegacyInterpreter, MemoryBus, Reg, Tier0Interpreter};

fn main() {
    println!("Tier-0 microbench (debug build timings are meaningless; use --release)");

    bench_dec_jnz();
    bench_rep_movsb();
}

fn bench_dec_jnz() {
    // A simple counted loop: (NOP...); dec rcx; jnz loop; hlt.
    //
    // The NOP body ensures the basic block is large enough to amortize block
    // lookup overhead, making the decoded-block interpreter's benefits obvious
    // even for this tiny instruction subset.
    let nops = 200usize;
    // For a larger body we use a near JNZ (rel32) to keep the encoding valid.
    let jnz_disp = -(nops as i32 + 9); // jump back to RIP=0 from next RIP.

    let mut prog = Vec::with_capacity(nops + 3 + 6 + 1);
    prog.extend(std::iter::repeat(0x90).take(nops));
    prog.extend([0x48, 0xFF, 0xC9]); // dec rcx
    prog.extend([0x0F, 0x85]); // jnz rel32
    prog.extend(i32::to_le_bytes(jnz_disp));
    prog.push(0xF4); // hlt

    let mut bus = MemoryBus::new(0x10000);
    bus.load(0, &prog).unwrap();

    let iterations: u64 = 500_000;

    let mut cpu = CpuState::default();
    cpu.set_reg(Reg::Rcx, iterations);

    let mut legacy = LegacyInterpreter::new(cpu.clone());
    let start = Instant::now();
    let instructions_per_iter = nops as u64 + 2;
    let instruction_limit = iterations * instructions_per_iter + 10;
    let _ = legacy
        .run(&mut bus.clone(), instruction_limit)
        .unwrap_or_else(|_| {
            panic!("legacy exited unexpectedly");
        });
    let legacy_dur = start.elapsed();

    let mut tier0 = Tier0Interpreter::new(cpu);
    let start = Instant::now();
    let _ = tier0.run(&mut bus, instruction_limit).unwrap_or_else(|_| {
        panic!("tier0 exited unexpectedly");
    });
    let tier0_dur = start.elapsed();

    println!(
        "dec/jnz: legacy={:?}, tier0={:?}, speedup={:.2}x",
        legacy_dur,
        tier0_dur,
        legacy_dur.as_secs_f64() / tier0_dur.as_secs_f64()
    );
}

fn bench_rep_movsb() {
    // rep movsb; hlt
    let prog = [0xF3, 0xA4, 0xF4];

    let len: usize = 4 * 1024 * 1024;
    // Choose non-overlapping ranges so the Tier-0 fast path can use a bulk copy.
    let src_addr: u64 = 0x10000;
    let dst_addr: u64 = src_addr + len as u64 + 0x10000;

    let mem_size = (dst_addr + len as u64 + 0x10000) as usize;
    let mut init_bus = MemoryBus::new(mem_size);
    init_bus.load(0, &prog).unwrap();
    for i in 0..len {
        init_bus
            .write_u8(src_addr + i as u64, (i as u8).wrapping_mul(13))
            .unwrap();
    }

    let mut cpu = CpuState::default();
    cpu.set_reg(Reg::Rsi, src_addr);
    cpu.set_reg(Reg::Rdi, dst_addr);
    cpu.set_reg(Reg::Rcx, len as u64);

    let mut legacy = LegacyInterpreter::new(cpu.clone());
    let start = Instant::now();
    let _ = legacy.run(&mut init_bus.clone(), 10_000_000).unwrap();
    let legacy_dur = start.elapsed();

    let mut tier0 = Tier0Interpreter::new(cpu);
    let start = Instant::now();
    let _ = tier0.run(&mut init_bus, 10_000_000).unwrap();
    let tier0_dur = start.elapsed();

    println!(
        "rep movsb ({} bytes): legacy={:?}, tier0={:?}, speedup={:.2}x",
        len,
        legacy_dur,
        tier0_dur,
        legacy_dur.as_secs_f64() / tier0_dur.as_secs_f64()
    );
}
