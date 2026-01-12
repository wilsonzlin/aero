mod tier1_common;

use aero_jit_x86::tier2::interp::{run_function, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_types::Gpr;
use tier1_common::SimpleBus;

fn run_x86(code: &[u8]) -> (RunExit, T2State) {
    let mut bus = SimpleBus::new(64);
    bus.load(0, code);
    let func = build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default());

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    let exit = run_function(&func, &env, &mut bus, &mut state, 10);
    (exit, state)
}

fn assert_side_exit_at_int3(exit: RunExit, code: &[u8]) {
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: (code.len() - 1) as u64
        }
    );
}

#[test]
fn tier2_masks_shift_count_for_32bit_operands_like_x86() {
    // mov eax, 1
    // shl eax, 33
    // int3
    const CODE: &[u8] = &[
        0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1
        0xC1, 0xE0, 0x21, // shl eax, 33
        0xCC, // int3 (decoded as Invalid => ExitToInterpreter at RIP=8)
    ];

    let (exit, state) = run_x86(CODE);
    assert_side_exit_at_int3(exit, CODE);
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 2);
}

#[test]
fn tier2_masks_shift_count_for_32bit_shr_like_x86() {
    // mov eax, 0x80000000
    // shr eax, 33
    // int3
    const CODE: &[u8] = &[
        0xB8, 0x00, 0x00, 0x00, 0x80, // mov eax, 0x80000000
        0xC1, 0xE8, 0x21, // shr eax, 33
        0xCC, // int3
    ];

    let (exit, state) = run_x86(CODE);
    assert_side_exit_at_int3(exit, CODE);
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x4000_0000);
}

#[test]
fn tier2_masks_shift_count_for_32bit_sar_like_x86() {
    // mov eax, 0x80000000
    // sar eax, 33
    // int3
    const CODE: &[u8] = &[
        0xB8, 0x00, 0x00, 0x00, 0x80, // mov eax, 0x80000000
        0xC1, 0xF8, 0x21, // sar eax, 33
        0xCC, // int3
    ];

    let (exit, state) = run_x86(CODE);
    assert_side_exit_at_int3(exit, CODE);
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0xC000_0000);
}

#[test]
fn tier2_masks_shift_count_for_16bit_operands_like_x86() {
    // 66 mov ax, 1
    // 66 shl ax, 33
    // int3
    const CODE: &[u8] = &[
        0x66, 0xB8, 0x01, 0x00, // mov ax, 1
        0x66, 0xC1, 0xE0, 0x21, // shl ax, 33
        0xCC, // int3 (decoded as Invalid => ExitToInterpreter at RIP=8)
    ];

    let (exit, state) = run_x86(CODE);
    assert_side_exit_at_int3(exit, CODE);
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 2);
}
