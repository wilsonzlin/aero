use aero_jit_proto::{Cond, Engine, FuncId, Gpr, JitConfig, Program, Vm};
use std::time::Instant;

fn build_program() -> Program {
    use aero_jit_proto::microvm::{Block, Function, Instr, Terminator, Xmm};

    let r0 = Gpr(0);
    let r1 = Gpr(1);
    let r2 = Gpr(2);
    let r3 = Gpr(3);
    let r4 = Gpr(4);
    let r5 = Gpr(5);
    let r6 = Gpr(6);
    let r7 = Gpr(7);

    let x0 = Xmm(0);
    let x1 = Xmm(1);

    let entry = Block {
        instrs: vec![
            Instr::Imm { dst: r1, imm: 0 },
            Instr::Imm { dst: r2, imm: 0 },
            Instr::Imm { dst: r3, imm: 0 },
            Instr::VImm {
                dst: x0,
                imm: 0x3f8000003f8000003f8000003f800000,
            }, // 1.0 lanes
            Instr::VImm {
                dst: x1,
                imm: 0x40000000400000004000000040000000,
            }, // 2.0 lanes
        ],
        term: Terminator::Jmp(1),
    };

    let header = Block {
        instrs: vec![Instr::Cmp { a: r0, b: r3 }],
        term: Terminator::Br {
            cond: Cond::Zero,
            then_tgt: 3,
            else_tgt: 2,
        },
    };

    let body = Block {
        instrs: vec![
            Instr::Imm { dst: r4, imm: 8 },
            Instr::Mul {
                dst: r5,
                a: r1,
                b: r4,
            },
            Instr::Mul {
                dst: r6,
                a: r1,
                b: r4,
            },
            Instr::Add {
                dst: r1,
                a: r5,
                b: r6,
            },
            Instr::Load {
                dst: r5,
                base: r2,
                offset: 16,
            },
            Instr::Add {
                dst: r1,
                a: r1,
                b: r5,
            },
            Instr::Store {
                base: r2,
                offset: 24,
                src: r1,
            },
            Instr::Imm { dst: r7, imm: 1 },
            Instr::Sub {
                dst: r0,
                a: r0,
                b: r7,
            },
            Instr::VAddF32x4 {
                dst: x0,
                a: x0,
                b: x1,
            },
            Instr::VMulF32x4 {
                dst: x0,
                a: x0,
                b: x1,
            },
        ],
        term: Terminator::Jmp(1),
    };

    let exit = Block {
        instrs: vec![],
        term: Terminator::Ret { src: r1 },
    };

    Program {
        functions: vec![Function {
            entry: 0,
            blocks: vec![entry, header, body, exit],
            gpr_count: 8,
            xmm_count: 2,
        }],
    }
}

fn init_vm(loop_count: u64) -> Vm {
    let mut vm = Vm::new(8, 2, 4096);
    vm.gprs[0] = loop_count;
    vm.mem.store_u64(16, 1);
    vm
}

fn timed_run(engine: &mut Engine, program: &Program, loop_count: u64) -> (u64, u128) {
    let mut vm = init_vm(loop_count);
    let start = Instant::now();
    let ret = engine.run(&mut vm, program, 0 as FuncId);
    let elapsed = start.elapsed().as_micros();
    (ret, elapsed)
}

fn main() {
    let program = build_program();
    let loop_count = 200_000u64;

    // Tier-1 only.
    let mut tier1 = Engine::new(
        &program,
        JitConfig {
            tier1_threshold: 1,
            tier2_threshold: u64::MAX,
            ..JitConfig::default()
        },
    );
    // Prime rolling metrics window.
    let _ = tier1.telemetry_snapshot();
    // Warm-up compile.
    let _ = timed_run(&mut tier1, &program, 10_000);
    let (ret1, us1) = timed_run(&mut tier1, &program, loop_count);
    let tier1_jit = tier1.telemetry_snapshot();

    // Tier-2 enabled.
    let mut tier2 = Engine::new(
        &program,
        JitConfig {
            tier1_threshold: 1,
            tier2_threshold: 5,
            ..JitConfig::default()
        },
    );
    let _ = tier2.telemetry_snapshot();
    // Warm-up to compile tier2 regions.
    let _ = timed_run(&mut tier2, &program, 10_000);
    let (ret2, us2) = timed_run(&mut tier2, &program, loop_count);
    let tier2_jit = tier2.telemetry_snapshot();

    println!("microbench: loop_count={loop_count}");
    println!(
        "tier1-only: ret={ret1} time={}us stats={:?}",
        us1,
        tier1.stats()
    );
    println!("           {}", tier1_jit.jit_hud_line());
    println!(
        "tier2:      ret={ret2} time={}us stats={:?}",
        us2,
        tier2.stats()
    );
    println!("           {}", tier2_jit.jit_hud_line());
    if us2 > 0 {
        println!("speedup: {:.2}x", us1 as f64 / us2 as f64);
    }
}
