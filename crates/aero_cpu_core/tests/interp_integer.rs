use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CpuState, FLAG_CF, FLAG_ZF};

fn run_to_halt(state: &mut CpuState, bus: &mut FlatTestBus, max: u64) {
    let mut steps = 0;
    while steps < max {
        let res = run_batch(state, bus, 1024);
        steps += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => return,
            BatchExit::Assist(r) => panic!("unexpected assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception: {e:?}"),
        }
    }
    panic!("program did not halt");
}

#[test]
fn mov_add_sub_flags() {
    // mov eax,1; add eax,-1; hlt
    let code = [
        0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x83, 0xC0, 0xFF, // add eax,-1 (imm8 sign-extended)
        0xF4, // hlt
    ];
    let mut bus = FlatTestBus::new(0x1000);
    bus.load(0, &code);
    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 100);
    assert_eq!(state.read_reg(aero_x86::Register::EAX), 0);
    assert!(state.get_flag(FLAG_ZF));
}

#[test]
fn call_ret_stack() {
    // 0: mov eax, 5
    // 5: call 0x0B
    // A: hlt
    // B: inc eax
    // C: ret
    let code = [
        0xB8, 0x05, 0x00, 0x00, 0x00, // mov eax,5
        0xE8, 0x01, 0x00, 0x00, 0x00, // call +1 (to 0x0B)
        0xF4, // hlt
        0x40, // inc eax
        0xC3, // ret
    ];
    let mut bus = FlatTestBus::new(0x1000);
    bus.load(0, &code);
    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.write_reg(aero_x86::Register::ESP, 0x800);
    run_to_halt(&mut state, &mut bus, 100);
    assert_eq!(state.read_reg(aero_x86::Register::EAX), 6);
}

#[test]
fn bsf_bsr_bt() {
    // mov eax,0x10; bsf ecx,eax; bsr edx,eax; bt eax,4; hlt
    let code = [
        0xB8, 0x10, 0x00, 0x00, 0x00, // mov eax,0x10
        0x0F, 0xBC, 0xC8, // bsf ecx,eax
        0x0F, 0xBD, 0xD0, // bsr edx,eax
        0x0F, 0xA3, 0xE0, // bt eax,esp? actually modrm E0 => /4? fix below
        0xF4,
    ];
    let mut bus = FlatTestBus::new(0x1000);
    // Fix bt eax,4 using imm8 form: 0F BA /4 ib; modrm E0 means mod=11 reg=4 rm=0 (EAX)
    let mut prog = Vec::new();
    prog.extend_from_slice(&code[..11]);
    prog.extend_from_slice(&[0x0F, 0xBA, 0xE0, 0x04]); // bt eax,4
    prog.push(0xF4);
    bus.load(0, &prog);
    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 100);
    assert_eq!(state.read_reg(aero_x86::Register::ECX), 4);
    assert_eq!(state.read_reg(aero_x86::Register::EDX), 4);
    assert!(state.get_flag(FLAG_CF));
}

#[test]
fn fuzz_lite_no_panics() {
    // Build a small random-ish program made of safe encodings and make sure
    // the interpreter never panics and always makes forward progress.
    let mut seed = 0x1234_5678_u64;
    let mut code: Vec<u8> = Vec::new();
    for _ in 0..200 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let choice = (seed >> 32) % 5;
        let reg = ((seed >> 40) & 7) as u8;
        match choice {
            0 => {
                // mov r32, imm32
                code.push(0xB8 + reg);
                code.extend_from_slice(&(seed as u32).to_le_bytes());
            }
            1 => {
                // add r32, imm8  (83 /0 ib)
                code.extend_from_slice(&[0x83, 0xC0 | reg, (seed >> 8) as u8]);
            }
            2 => {
                // sub r32, imm8  (83 /5 ib)
                code.extend_from_slice(&[0x83, 0xE8 | reg, (seed >> 8) as u8]);
            }
            3 => {
                // xor r32, r32 (31 /r)
                code.extend_from_slice(&[0x31, 0xC0 | (reg << 3) | reg]);
            }
            _ => {
                // jz +0 (always in range)
                code.extend_from_slice(&[0x74, 0x00]);
            }
        }
    }
    code.push(0xF4); // hlt

    let mut bus = FlatTestBus::new(0x4000);
    bus.load(0, &code);
    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    state.write_reg(aero_x86::Register::ESP, 0x2000);

    let mut prev_ip = state.rip();
    for _ in 0..5000 {
        let res = run_batch(&mut state, &mut bus, 1);
        if matches!(res.exit, BatchExit::Exception(_)) {
            panic!("unexpected exception");
        }
        let ip = state.rip();
        assert_ne!(ip, prev_ip, "IP did not progress");
        prev_ip = ip;
        if matches!(res.exit, BatchExit::Halted) {
            return;
        }
    }
    panic!("did not halt");
}
