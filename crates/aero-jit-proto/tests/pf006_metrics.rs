use aero_jit_proto::{Engine, JitConfig, Program, Vm};

fn build_program() -> Program {
    use aero_jit_proto::microvm::{Block, Cond, Function, Gpr, Instr, Terminator, Xmm};

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
            },
            Instr::VImm {
                dst: x1,
                imm: 0x40000000400000004000000040000000,
            },
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

#[test]
fn pf006_metrics_nonzero_when_jit_enabled() {
    let program = build_program();
    let mut vm = init_vm(20_000);

    let mut engine = Engine::new(
        &program,
        JitConfig {
            tier1_threshold: 1,
            tier2_threshold: 5,
            ..JitConfig::default()
        },
    );

    // Prime rolling window.
    let _ = engine.telemetry_snapshot();

    let _ = engine.run(&mut vm, &program, 0);
    let snapshot = engine.telemetry_snapshot();

    assert!(snapshot.jit.enabled);
    assert!(snapshot.jit.totals.tier1.blocks_compiled > 0);
    assert!(snapshot.jit.totals.tier2.blocks_compiled > 0);
    assert!(snapshot.jit.totals.tier1.compile_ms > 0.0);
    assert!(snapshot.jit.totals.tier2.compile_ms > 0.0);
    assert!(snapshot.jit.totals.cache.used_bytes > 0);
    assert!(snapshot.jit.totals.cache.capacity_bytes > 0);
    assert!(snapshot.jit.totals.cache.lookup_hit + snapshot.jit.totals.cache.lookup_miss > 0);
    assert!(snapshot.jit.rolling.window_ms > 0);
}

#[test]
fn pf006_metrics_zero_when_jit_disabled() {
    let program = build_program();
    let mut vm = init_vm(1_000);

    let mut engine = Engine::new(
        &program,
        JitConfig {
            tier1_threshold: u64::MAX,
            tier2_threshold: u64::MAX,
            ..JitConfig::default()
        },
    );

    let _ = engine.telemetry_snapshot();
    let _ = engine.run(&mut vm, &program, 0);
    let snapshot = engine.telemetry_snapshot();

    assert!(!snapshot.jit.enabled);
    assert_eq!(snapshot.jit.totals.tier1.blocks_compiled, 0);
    assert_eq!(snapshot.jit.totals.tier2.blocks_compiled, 0);
    assert_eq!(snapshot.jit.totals.cache.lookup_hit, 0);
    assert_eq!(snapshot.jit.totals.cache.lookup_miss, 0);
    assert_eq!(snapshot.jit.totals.cache.used_bytes, 0);
    assert_eq!(snapshot.jit.totals.cache.capacity_bytes, 0);
    assert_eq!(snapshot.jit.totals.tier1.compile_ms, 0.0);
    assert_eq!(snapshot.jit.totals.tier2.compile_ms, 0.0);
    assert_eq!(snapshot.jit.totals.deopt.count, 0);
    assert_eq!(snapshot.jit.totals.deopt.guard_fail, 0);
}
