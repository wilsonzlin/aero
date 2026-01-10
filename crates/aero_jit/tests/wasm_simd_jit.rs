use aero_jit::simd::{
    compile_wasm_simd, interpret, Inst, JitOptions, Operand, Program, SseState, XmmReg,
    DEFAULT_WASM_LAYOUT, MXCSR_DEFAULT, STATE_SIZE_BYTES,
};

use rand::{rngs::StdRng, Rng, SeedableRng};
use wasmparser::{Operator, Parser, Payload};
use wasmtime::{Config, Engine, Instance, Module, Store, Trap};

fn run_jit(program: &Program, state: &SseState, mem: &[u8]) -> (SseState, Vec<u8>, Vec<u8>) {
    let wasm = compile_wasm_simd(program, JitOptions::default(), DEFAULT_WASM_LAYOUT).unwrap();

    let mut config = Config::new();
    config.wasm_simd(true);
    let engine = Engine::new(&config).unwrap();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();

    let memory = instance.get_memory(&mut store, "mem").unwrap();
    let run = instance.get_typed_func::<(), ()>(&mut store, "run").unwrap();

    let mut state_bytes = vec![0u8; STATE_SIZE_BYTES];
    state.write_to_bytes(&mut state_bytes).unwrap();
    memory
        .write(&mut store, DEFAULT_WASM_LAYOUT.state_base as usize, &state_bytes)
        .unwrap();

    memory
        .write(
            &mut store,
            DEFAULT_WASM_LAYOUT.guest_mem_base as usize,
            mem,
        )
        .unwrap();

    run.call(&mut store, ()).unwrap();

    let mut out_state_bytes = vec![0u8; STATE_SIZE_BYTES];
    memory
        .read(
            &mut store,
            DEFAULT_WASM_LAYOUT.state_base as usize,
            &mut out_state_bytes,
        )
        .unwrap();
    let mut out_state = SseState::default();
    out_state.read_from_bytes(&out_state_bytes).unwrap();

    let mut out_mem = vec![0u8; mem.len()];
    memory
        .read(
            &mut store,
            DEFAULT_WASM_LAYOUT.guest_mem_base as usize,
            &mut out_mem,
        )
        .unwrap();

    (out_state, out_mem, wasm)
}

fn assert_jit_matches_interp(program: Program, state: SseState, mem: Vec<u8>) {
    let mut interp_state = state.clone();
    let mut interp_mem = mem.clone();
    interpret(&program, &mut interp_state, &mut interp_mem).unwrap();

    let (jit_state, jit_mem, _) = run_jit(&program, &state, &mem);

    assert_eq!(jit_state, interp_state);
    assert_eq!(jit_mem, interp_mem);
}

fn assert_wasm_contains_op(wasm: &[u8], predicate: impl Fn(&Operator<'_>) -> bool) {
    for payload in Parser::new(0).parse_all(wasm) {
        let payload = payload.unwrap();
        if let Payload::CodeSectionEntry(body) = payload {
            let mut reader = body.get_operators_reader().unwrap();
            while !reader.eof() {
                let op = reader.read().unwrap();
                if predicate(&op) {
                    return;
                }
            }
        }
    }
    panic!("did not find expected wasm operator");
}

fn random_f32(rng: &mut impl Rng) -> f32 {
    rng.gen_range(-1000.0f32..1000.0f32)
}

fn random_f64(rng: &mut impl Rng) -> f64 {
    rng.gen_range(-1000.0f64..1000.0f64)
}

fn pack_f32x4(lanes: [f32; 4]) -> u128 {
    let mut bytes = [0u8; 16];
    for i in 0..4 {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&lanes[i].to_bits().to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}

fn pack_f64x2(lanes: [f64; 2]) -> u128 {
    let mut bytes = [0u8; 16];
    for i in 0..2 {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&lanes[i].to_bits().to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}

#[test]
fn wasm_simd_movdqu_load_store() {
    let xmm0 = XmmReg::new(0).unwrap();
    let xmm1 = XmmReg::new(1).unwrap();

    let program = Program {
        insts: vec![
            Inst::MovdquLoad { dst: xmm0, addr: 0 },
            Inst::MovdquStore { addr: 16, src: xmm0 },
            Inst::MovdquLoad { dst: xmm1, addr: 16 },
        ],
    };

    let mut state = SseState::default();
    state.xmm[xmm0.index()] = 0;
    state.xmm[xmm1.index()] = 0;

    let mut mem = vec![0u8; 64];
    for (i, b) in mem[0..16].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3);
    }

    assert_jit_matches_interp(program.clone(), state.clone(), mem.clone());

    let (_, _, wasm) = run_jit(&program, &state, &mem);
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::V128Load { .. }));
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::V128Store { .. }));
}

#[test]
fn wasm_simd_f32x4_add_mul_sub() {
    let mut rng = StdRng::seed_from_u64(1);
    let xmm0 = XmmReg::new(0).unwrap();
    let xmm1 = XmmReg::new(1).unwrap();
    let xmm2 = XmmReg::new(2).unwrap();

    let mut state = SseState::default();
    state.xmm[xmm0.index()] = pack_f32x4([
        random_f32(&mut rng),
        random_f32(&mut rng),
        random_f32(&mut rng),
        random_f32(&mut rng),
    ]);
    state.xmm[xmm1.index()] = pack_f32x4([
        random_f32(&mut rng),
        random_f32(&mut rng),
        random_f32(&mut rng),
        random_f32(&mut rng),
    ]);
    state.xmm[xmm2.index()] = pack_f32x4([
        random_f32(&mut rng),
        random_f32(&mut rng),
        random_f32(&mut rng),
        random_f32(&mut rng),
    ]);

    let program = Program {
        insts: vec![
            Inst::Addps {
                dst: xmm0,
                src: Operand::Reg(xmm1),
            },
            Inst::Mulps {
                dst: xmm0,
                src: Operand::Reg(xmm2),
            },
            Inst::Subps {
                dst: xmm0,
                src: Operand::Reg(xmm1),
            },
        ],
    };

    let mem = vec![0u8; 64];
    assert_jit_matches_interp(program.clone(), state.clone(), mem.clone());

    let (_, _, wasm) = run_jit(&program, &state, &mem);
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::F32x4Add));
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::F32x4Mul));
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::F32x4Sub));
}

#[test]
fn wasm_simd_f64x2_add_mul_sub() {
    let mut rng = StdRng::seed_from_u64(2);
    let xmm0 = XmmReg::new(0).unwrap();
    let xmm1 = XmmReg::new(1).unwrap();
    let xmm2 = XmmReg::new(2).unwrap();

    let mut state = SseState::default();
    state.xmm[xmm0.index()] = pack_f64x2([random_f64(&mut rng), random_f64(&mut rng)]);
    state.xmm[xmm1.index()] = pack_f64x2([random_f64(&mut rng), random_f64(&mut rng)]);
    state.xmm[xmm2.index()] = pack_f64x2([random_f64(&mut rng), random_f64(&mut rng)]);

    let program = Program {
        insts: vec![
            Inst::Addpd {
                dst: xmm0,
                src: Operand::Reg(xmm1),
            },
            Inst::Mulpd {
                dst: xmm0,
                src: Operand::Reg(xmm2),
            },
            Inst::Subpd {
                dst: xmm0,
                src: Operand::Reg(xmm1),
            },
        ],
    };

    let mem = vec![0u8; 64];
    assert_jit_matches_interp(program.clone(), state.clone(), mem.clone());

    let (_, _, wasm) = run_jit(&program, &state, &mem);
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::F64x2Add));
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::F64x2Mul));
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::F64x2Sub));
}

#[test]
fn wasm_simd_bitwise_and_or_xor() {
    let mut rng = StdRng::seed_from_u64(3);
    let xmm0 = XmmReg::new(0).unwrap();
    let xmm1 = XmmReg::new(1).unwrap();

    let mut state = SseState::default();
    state.xmm[xmm0.index()] = rng.gen::<u128>();
    state.xmm[xmm1.index()] = rng.gen::<u128>();

    let program = Program {
        insts: vec![
            Inst::Pand {
                dst: xmm0,
                src: Operand::Reg(xmm1),
            },
            Inst::Por {
                dst: xmm0,
                src: Operand::Reg(xmm1),
            },
            Inst::Pxor {
                dst: xmm0,
                src: Operand::Reg(xmm1),
            },
        ],
    };

    let mem = vec![0u8; 64];
    assert_jit_matches_interp(program.clone(), state.clone(), mem.clone());

    let (_, _, wasm) = run_jit(&program, &state, &mem);
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::V128And));
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::V128Or));
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::V128Xor));
}

#[test]
fn wasm_simd_pshufb_swizzle() {
    let mut rng = StdRng::seed_from_u64(4);
    let xmm0 = XmmReg::new(0).unwrap();
    let xmm1 = XmmReg::new(1).unwrap();

    let mut state = SseState::default();
    state.xmm[xmm0.index()] = rng.gen::<u128>();
    state.xmm[xmm1.index()] = rng.gen::<u128>();

    let program = Program {
        insts: vec![Inst::Pshufb {
            dst: xmm0,
            src: Operand::Reg(xmm1),
        }],
    };

    let mem = vec![0u8; 64];
    assert_jit_matches_interp(program.clone(), state.clone(), mem.clone());

    let (_, _, wasm) = run_jit(&program, &state, &mem);
    assert_wasm_contains_op(&wasm, |op| matches!(op, Operator::I8x16Swizzle));
}

#[test]
fn mxcsr_gate_traps_for_float_ops() {
    let xmm0 = XmmReg::new(0).unwrap();
    let xmm1 = XmmReg::new(1).unwrap();

    let program = Program {
        insts: vec![Inst::Addps {
            dst: xmm0,
            src: Operand::Reg(xmm1),
        }],
    };

    let wasm = compile_wasm_simd(&program, JitOptions::default(), DEFAULT_WASM_LAYOUT).unwrap();

    let mut config = Config::new();
    config.wasm_simd(true);
    let engine = Engine::new(&config).unwrap();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "mem").unwrap();
    let run = instance.get_typed_func::<(), ()>(&mut store, "run").unwrap();

    let mut state = SseState::default();
    state.mxcsr = MXCSR_DEFAULT ^ 0x2000; // change rounding mode bits

    let mut state_bytes = vec![0u8; STATE_SIZE_BYTES];
    state.write_to_bytes(&mut state_bytes).unwrap();
    memory
        .write(&mut store, DEFAULT_WASM_LAYOUT.state_base as usize, &state_bytes)
        .unwrap();

    let mem = vec![0u8; 64];
    memory
        .write(
            &mut store,
            DEFAULT_WASM_LAYOUT.guest_mem_base as usize,
            &mem,
        )
        .unwrap();

    let err = run.call(&mut store, ()).unwrap_err();
    assert!(
        err.downcast_ref::<Trap>().is_some(),
        "expected trap, got: {err:?}"
    );
}

#[test]
fn pshufb_requires_ssse3_option() {
    let xmm0 = XmmReg::new(0).unwrap();
    let xmm1 = XmmReg::new(1).unwrap();

    let program = Program {
        insts: vec![Inst::Pshufb {
            dst: xmm0,
            src: Operand::Reg(xmm1),
        }],
    };

    let err = compile_wasm_simd(
        &program,
        JitOptions {
            enable_ssse3: false,
        },
        DEFAULT_WASM_LAYOUT,
    )
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "requires SSSE3 (PSHUFB) but JIT options have SSSE3 disabled"
    );
}

