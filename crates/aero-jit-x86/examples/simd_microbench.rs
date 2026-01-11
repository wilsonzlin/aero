use aero_jit_x86::simd::{
    compile_wasm_simd, interpret, Inst, Operand, Program, SseState, XmmReg, DEFAULT_WASM_LAYOUT,
};
use std::time::Instant;
use wasmtime::{Config, Engine, Instance, Module, Store};

fn main() {
    // A tiny demo that shows how to execute the SIMD-lowered wasm repeatedly.
    //
    // This is *not* intended to be a rigorous benchmark (wasmtime call overhead, debug builds,
    // host CPU scaling, etc), but it demonstrates the intended usage and should show a noticeable
    // gap compared to the pure Rust interpreter for sufficiently large iteration counts.
    let xmm0 = XmmReg::new(0).unwrap();
    let xmm1 = XmmReg::new(1).unwrap();

    let mut insts = Vec::new();
    for _ in 0..256 {
        insts.push(Inst::Addps {
            dst: xmm0,
            src: Operand::Reg(xmm1),
        });
        insts.push(Inst::Mulps {
            dst: xmm0,
            src: Operand::Reg(xmm1),
        });
        insts.push(Inst::Subps {
            dst: xmm0,
            src: Operand::Reg(xmm1),
        });
    }

    let program = Program { insts };
    let mem = vec![0u8; 4096];

    let mut state = SseState::default();
    state.xmm[xmm0.index()] = pack_f32x4([1.0; 4]);
    state.xmm[xmm1.index()] = pack_f32x4([2.0; 4]);

    let iterations = 50_000;

    let mut interp_state = state.clone();
    let mut interp_mem = mem.clone();
    let t0 = Instant::now();
    for _ in 0..iterations {
        interpret(&program, &mut interp_state, &mut interp_mem).unwrap();
    }
    let interp_elapsed = t0.elapsed();

    let wasm = compile_wasm_simd(&program, Default::default(), DEFAULT_WASM_LAYOUT).unwrap();
    let mut config = Config::new();
    config.wasm_simd(true);
    let engine = Engine::new(&config).unwrap();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).unwrap();
    let memory = instance.get_memory(&mut store, "mem").unwrap();
    let run = instance
        .get_typed_func::<(), ()>(&mut store, "run")
        .unwrap();

    let mut state_bytes = vec![0u8; aero_jit_x86::simd::STATE_SIZE_BYTES];
    state.write_to_bytes(&mut state_bytes).unwrap();
    memory
        .write(
            &mut store,
            DEFAULT_WASM_LAYOUT.state_base as usize,
            &state_bytes,
        )
        .unwrap();
    memory
        .write(
            &mut store,
            DEFAULT_WASM_LAYOUT.guest_mem_base as usize,
            &mem,
        )
        .unwrap();

    let t1 = Instant::now();
    for _ in 0..iterations {
        run.call(&mut store, ()).unwrap();
    }
    let jit_elapsed = t1.elapsed();

    println!("iterations: {iterations}");
    println!("interpreter: {interp_elapsed:?}");
    println!("wasm simd:   {jit_elapsed:?}");
}

fn pack_f32x4(lanes: [f32; 4]) -> u128 {
    let mut bytes = [0u8; 16];
    for i in 0..4 {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&lanes[i].to_bits().to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}
