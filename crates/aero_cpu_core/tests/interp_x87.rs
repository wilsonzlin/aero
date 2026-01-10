use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::interp::x87::X87;
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::Exception;
use aero_x86::Register;

const FSW_C0: u16 = 1 << 8;
const FSW_C2: u16 = 1 << 10;
const FSW_C3: u16 = 1 << 14;

fn run_to_halt(state: &mut CpuState, bus: &mut FlatTestBus, max_steps: u64) {
    let mut steps = 0u64;
    while steps < max_steps {
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
fn tier0_executes_basic_x87_mem32_arithmetic() {
    // fld dword ptr [0x100]
    // fadd dword ptr [0x104]
    // fstp dword ptr [0x108]
    // hlt
    let code = [
        0xD9, 0x05, 0x00, 0x01, 0x00, 0x00, // fld dword ptr [0x100]
        0xD8, 0x05, 0x04, 0x01, 0x00, 0x00, // fadd dword ptr [0x104]
        0xD9, 0x1D, 0x08, 0x01, 0x00, 0x00, // fstp dword ptr [0x108]
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x2000);
    bus.load(0, &code);
    bus.load(0x100, &1.5f32.to_bits().to_le_bytes());
    bus.load(0x104, &2.25f32.to_bits().to_le_bytes());

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 100);

    let out = f32::from_bits(bus.read_u32(0x108).unwrap());
    assert_eq!(out, 3.75);
    let mut x87 = X87::default();
    x87.load_from_fpu_state(&state.fpu);
    assert_eq!(x87.tag_word(), 0xFFFF);
    assert_eq!(x87.st(0), None);
}

#[test]
fn tier0_executes_fcom_and_fnstsw_ax() {
    // fld dword ptr [0x100]
    // fcom dword ptr [0x104]
    // fnstsw ax
    // hlt
    let code = [
        0xD9, 0x05, 0x00, 0x01, 0x00, 0x00, // fld dword ptr [0x100]
        0xD8, 0x15, 0x04, 0x01, 0x00, 0x00, // fcom dword ptr [0x104]
        0xDF, 0xE0, // fnstsw ax
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x2000);
    bus.load(0, &code);
    bus.load(0x100, &1.0f32.to_bits().to_le_bytes());
    bus.load(0x104, &2.0f32.to_bits().to_le_bytes());

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 100);

    let ax = state.read_reg(Register::AX) as u16;
    assert_eq!(ax & (FSW_C0 | FSW_C2 | FSW_C3), FSW_C0);
}

#[test]
fn tier0_fucomi_sets_eflags_for_setcc() {
    // fld dword ptr [0x100]      ; 2.0
    // fld dword ptr [0x104]      ; 1.0 (ST0=1.0, ST1=2.0)
    // fucomi st0, st1            ; 1.0 < 2.0 => CF=1
    // setb al
    // hlt
    let code = [
        0xD9, 0x05, 0x00, 0x01, 0x00, 0x00, // fld dword ptr [0x100]
        0xD9, 0x05, 0x04, 0x01, 0x00, 0x00, // fld dword ptr [0x104]
        0xDB, 0xE9, // fucomi st0, st1
        0x0F, 0x92, 0xC0, // setb al
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x2000);
    bus.load(0, &code);
    bus.load(0x100, &2.0f32.to_bits().to_le_bytes());
    bus.load(0x104, &1.0f32.to_bits().to_le_bytes());

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 100);

    assert_eq!(state.read_reg(Register::AL), 1);
}

#[test]
fn tier0_fcomi_sets_eflags_for_setcc() {
    // fld dword ptr [0x100]      ; 2.0
    // fld dword ptr [0x104]      ; 1.0 (ST0=1.0, ST1=2.0)
    // fcomi st0, st1             ; 1.0 < 2.0 => CF=1
    // setb al
    // hlt
    let code = [
        0xD9, 0x05, 0x00, 0x01, 0x00, 0x00, // fld dword ptr [0x100]
        0xD9, 0x05, 0x04, 0x01, 0x00, 0x00, // fld dword ptr [0x104]
        0xDB, 0xF1, // fcomi st0, st1
        0x0F, 0x92, 0xC0, // setb al
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x2000);
    bus.load(0, &code);
    bus.load(0x100, &2.0f32.to_bits().to_le_bytes());
    bus.load(0x104, &1.0f32.to_bits().to_le_bytes());

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 100);

    assert_eq!(state.read_reg(Register::AL), 1);
}

#[test]
fn tier0_fxch_and_fninit_reset_state() {
    // fld dword ptr [0x100]      ; 1.0
    // fld dword ptr [0x104]      ; 2.0 (ST0=2.0, ST1=1.0)
    // fxch st1                   ; ST0=1.0, ST1=2.0
    // fstp dword ptr [0x108]     ; store 1.0, pop -> ST0=2.0
    // fninit                     ; reset FPU
    // fnstcw word ptr [0x10C]
    // fnstsw word ptr [0x10E]
    // hlt
    let code = [
        0xD9, 0x05, 0x00, 0x01, 0x00, 0x00, // fld dword ptr [0x100]
        0xD9, 0x05, 0x04, 0x01, 0x00, 0x00, // fld dword ptr [0x104]
        0xD9, 0xC9, // fxch st1
        0xD9, 0x1D, 0x08, 0x01, 0x00, 0x00, // fstp dword ptr [0x108]
        0xDB, 0xE3, // fninit
        0xD9, 0x3D, 0x0C, 0x01, 0x00, 0x00, // fnstcw word ptr [0x10C]
        0xDD, 0x3D, 0x0E, 0x01, 0x00, 0x00, // fnstsw word ptr [0x10E]
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x2000);
    bus.load(0, &code);
    bus.load(0x100, &1.0f32.to_bits().to_le_bytes());
    bus.load(0x104, &2.0f32.to_bits().to_le_bytes());

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 200);

    let out = f32::from_bits(bus.read_u32(0x108).unwrap());
    assert_eq!(out, 1.0);

    let cw = bus.read_u16(0x10C).unwrap();
    let sw = bus.read_u16(0x10E).unwrap();
    assert_eq!(cw, 0x037F);
    assert_eq!(sw, 0);

    let mut x87 = X87::default();
    x87.load_from_fpu_state(&state.fpu);
    assert_eq!(x87.top(), 0);
    assert_eq!(x87.tag_word(), 0xFFFF);
}

#[test]
fn tier0_unmasked_invalid_raises_mf() {
    // fninit
    // fldcw word ptr [0x200]     ; unmask invalid operation (IM=0)
    // fstp dword ptr [0x208]     ; pop empty stack => invalid op => #MF
    let code = [
        0xDB, 0xE3, // fninit
        0xD9, 0x2D, 0x00, 0x02, 0x00, 0x00, // fldcw word ptr [0x200]
        0xD9, 0x1D, 0x08, 0x02, 0x00, 0x00, // fstp dword ptr [0x208]
    ];

    let mut bus = FlatTestBus::new(0x3000);
    bus.load(0, &code);
    bus.load(0x200, &0x037Eu16.to_le_bytes());

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    let res = run_batch(&mut state, &mut bus, 10);

    assert_eq!(res.executed, 2);
    assert!(matches!(res.exit, BatchExit::Exception(Exception::X87Fpu)));
}

#[test]
fn tier0_fincstp_and_fdecstp_rotate_top() {
    // fld dword ptr [0x100]      ; 1.0
    // fld dword ptr [0x104]      ; 2.0 (ST0=2.0, ST1=1.0)
    // fincstp                    ; ST0=1.0, ST7=2.0
    // fst dword ptr [0x108]      ; store ST0 (1.0)
    // fdecstp                    ; ST0=2.0
    // fst dword ptr [0x10C]      ; store ST0 (2.0)
    // hlt
    let code = [
        0xD9, 0x05, 0x00, 0x01, 0x00, 0x00, // fld dword ptr [0x100]
        0xD9, 0x05, 0x04, 0x01, 0x00, 0x00, // fld dword ptr [0x104]
        0xD9, 0xF7, // fincstp
        0xD9, 0x15, 0x08, 0x01, 0x00, 0x00, // fst dword ptr [0x108]
        0xD9, 0xF6, // fdecstp
        0xD9, 0x15, 0x0C, 0x01, 0x00, 0x00, // fst dword ptr [0x10C]
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x2000);
    bus.load(0, &code);
    bus.load(0x100, &1.0f32.to_bits().to_le_bytes());
    bus.load(0x104, &2.0f32.to_bits().to_le_bytes());

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 200);

    let a = f32::from_bits(bus.read_u32(0x108).unwrap());
    let b = f32::from_bits(bus.read_u32(0x10C).unwrap());
    assert_eq!(a, 1.0);
    assert_eq!(b, 2.0);
}

#[test]
fn tier0_fistp_int64_writes_qword() {
    // fninit
    // fld dword ptr [0x100]
    // fistp qword ptr [0x108]
    // hlt
    let code = [
        0xDB, 0xE3, // fninit
        0xD9, 0x05, 0x00, 0x01, 0x00, 0x00, // fld dword ptr [0x100]
        0xDF, 0x3D, 0x08, 0x01, 0x00, 0x00, // fistp qword ptr [0x108]
        0xF4, // hlt
    ];

    let mut bus = FlatTestBus::new(0x2000);
    bus.load(0, &code);
    bus.load(0x100, &12345.0f32.to_bits().to_le_bytes());

    let mut state = CpuState::new(CpuMode::Bit32);
    state.set_rip(0);
    run_to_halt(&mut state, &mut bus, 200);

    let out = bus.read_u64(0x108).unwrap() as i64;
    assert_eq!(out, 12345);
}
