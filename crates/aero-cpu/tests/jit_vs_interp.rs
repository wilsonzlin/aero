use aero_cpu::baseline::{CpuWorker, Interpreter, JitConfig, Memory};
use aero_jit::cpu::{CpuState, Reg};

fn run_interpreter(code: &[u8], mut cpu: CpuState, max_steps: u64) -> CpuState {
    let mut mem = Memory::new(64 * 1024);
    mem.load(0, code);
    let interp = Interpreter::default();
    for _ in 0..max_steps {
        if cpu.is_halted() {
            break;
        }
        interp.step(&mut cpu, &mut mem).unwrap();
    }
    cpu
}

fn run_jit(code: &[u8], cpu: CpuState, max_steps: u64) -> (CpuState, CpuWorker) {
    let mut mem = Memory::new(64 * 1024);
    mem.load(0, code);
    let worker = CpuWorker::new(mem).with_config(JitConfig {
        hot_threshold: 1,
        max_block_insts: 64,
        max_block_bytes: 512,
    });
    let mut worker = worker;
    worker.cpu = cpu;
    worker.run(max_steps).unwrap();
    (worker.cpu, worker)
}

#[test]
fn jit_matches_interpreter_decrement_loop() {
    // mov rcx, N
    // loop: sub rcx, 1
    //       jne loop
    //       hlt
    fn program(n: u64) -> Vec<u8> {
        let mut code = Vec::new();
        code.extend_from_slice(&[0x48, 0xB9]); // mov rcx, imm64
        code.extend_from_slice(&n.to_le_bytes());
        code.extend_from_slice(&[0x48, 0x83, 0xE9, 0x01]); // sub rcx, 1
        code.extend_from_slice(&[0x75, 0xFA]); // jne -6 (to sub)
        code.push(0xF4); // hlt
        code
    }

    for &n in &[1u64, 2, 3, 10, 100] {
        let code = program(n);
        let mut init = CpuState::default();
        init.rip = 0;

        let interp = run_interpreter(&code, init, 1_000_000);
        let (jit, _worker) = run_jit(&code, init, 1_000_000);

        assert_eq!(jit, interp, "N={n}");
        assert_eq!(jit.reg(Reg::Rcx), 0);
        assert!(jit.is_halted());
    }
}

#[test]
fn jit_matches_interpreter_flag_branch() {
    // mov rax, 5
    // cmp rax, 5
    // je label
    // mov rbx, 1
    // jmp end
    // label: mov rbx, 2
    // end: hlt
    let mut code = Vec::new();
    code.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
    code.extend_from_slice(&5u64.to_le_bytes());
    code.extend_from_slice(&[0x48, 0x83, 0xF8, 0x05]); // cmp rax, 5
    code.extend_from_slice(&[0x74, 0x0C]); // je +12 (to label)
    code.extend_from_slice(&[0x48, 0xBB]); // mov rbx, imm64
    code.extend_from_slice(&1u64.to_le_bytes());
    code.extend_from_slice(&[0xEB, 0x0A]); // jmp +10 (to end)
    code.extend_from_slice(&[0x48, 0xBB]); // label: mov rbx, imm64
    code.extend_from_slice(&2u64.to_le_bytes());
    code.push(0xF4); // hlt

    let mut init = CpuState::default();
    init.rip = 0;

    let interp = run_interpreter(&code, init, 1_000_000);
    let (jit, _worker) = run_jit(&code, init, 1_000_000);

    assert_eq!(jit, interp);
    assert_eq!(jit.reg(Reg::Rbx), 2);
    assert!(jit.is_halted());
}

#[test]
fn jit_matches_interpreter_mem_sib_rsp() {
    // mov rax, imm64
    // mov [rsp], rax
    // mov rbx, [rsp]
    // hlt
    let mut code = Vec::new();
    code.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
    code.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    code.extend_from_slice(&[0x48, 0x89, 0x04, 0x24]); // mov [rsp], rax
    code.extend_from_slice(&[0x48, 0x8B, 0x1C, 0x24]); // mov rbx, [rsp]
    code.push(0xF4); // hlt

    let mut init = CpuState::default();
    init.rip = 0;
    init.set_reg(Reg::Rsp, 0x2000);

    let interp = run_interpreter(&code, init, 1_000_000);
    let (jit, _worker) = run_jit(&code, init, 1_000_000);

    assert_eq!(jit, interp);
    assert_eq!(jit.reg(Reg::Rbx), 0x1122_3344_5566_7788);
    assert!(jit.is_halted());
}

#[test]
fn jit_is_deterministic() {
    // A tiny loop that exercises flags and control flow.
    let code = {
        let mut code = Vec::new();
        code.extend_from_slice(&[0x48, 0xB9]); // mov rcx, imm64
        code.extend_from_slice(&50u64.to_le_bytes());
        code.extend_from_slice(&[0x48, 0x83, 0xE9, 0x01]); // sub rcx, 1
        code.extend_from_slice(&[0x75, 0xFA]); // jne -6
        code.push(0xF4); // hlt
        code
    };

    let mut init = CpuState::default();
    init.rip = 0;

    let (a, _worker_a) = run_jit(&code, init, 1_000_000);
    let (b, _worker_b) = run_jit(&code, init, 1_000_000);
    assert_eq!(a, b);
}

#[test]
fn self_modifying_code_invalidates_cache() {
    // mov rsp, 0
    // mov rax, imm64
    // mov [rsp], rax   ; overwrite first bytes of code page
    // hlt
    let mut code = Vec::new();
    code.extend_from_slice(&[0x48, 0xBC]); // mov rsp, imm64
    code.extend_from_slice(&0u64.to_le_bytes());
    code.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
    code.extend_from_slice(&0xF4u64.to_le_bytes()); // some bytes (HLT opcode in low byte)
    code.extend_from_slice(&[0x48, 0x89, 0x04, 0x24]); // mov [rsp], rax
    code.push(0xF4); // hlt

    let mut init = CpuState::default();
    init.rip = 0;

    let (_final_cpu, worker) = run_jit(&code, init, 1_000_000);

    // The block executes and writes to a code page, which should trigger cache flush.
    assert_eq!(worker.code_cache_len(), 0);
}

#[test]
fn wasm_is_emitted_for_compiled_blocks() {
    // Simple hlt block: should compile and produce a valid wasm header.
    let code = [0xF4u8];
    let mut init = CpuState::default();
    init.rip = 0;

    let (_final_cpu, worker) = run_jit(&code, init, 10);
    let wasm = worker.compiled_wasm(0).expect("block should be compiled");
    assert!(wasm.starts_with(&[0x00, 0x61, 0x73, 0x6D]));
}
