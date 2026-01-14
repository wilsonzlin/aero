mod tier1_common;

use aero_jit_x86::tier2::interp::{run_function, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_types::Gpr;
use tier1_common::SimpleBus;

// mov eax, 1
// xor eax, 0       ; result=1 => PF=0
// jp label         ; must NOT take
// mov eax, 2
// int3
// label: mov eax, 3
// int3
const XOR_JP_CODE: &[u8] = &[
    0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1
    0x83, 0xf0, 0x00, // xor eax, 0
    0x7a, 0x06, // jp +6
    0xb8, 0x02, 0x00, 0x00, 0x00, // mov eax, 2
    0xcc, // int3
    0xb8, 0x03, 0x00, 0x00, 0x00, // mov eax, 3
    0xcc, // int3
];

// mov eax, 1
// test eax, eax    ; result=1 => PF=0
// jp label         ; must NOT take
// mov eax, 2
// int3
// label: mov eax, 3
// int3
const TEST_JP_CODE: &[u8] = &[
    0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1
    0x85, 0xc0, // test eax, eax
    0x7a, 0x06, // jp +6
    0xb8, 0x02, 0x00, 0x00, 0x00, // mov eax, 2
    0xcc, // int3
    0xb8, 0x03, 0x00, 0x00, 0x00, // mov eax, 3
    0xcc, // int3
];

#[test]
fn jp_observes_pf_after_alu_xor32() {
    let mut bus = SimpleBus::new(64);
    bus.load(0, XOR_JP_CODE);
    let func = build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default());

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = 0;

    let exit = run_function(&func, &env, &mut bus, &mut state, 16);
    assert_eq!(exit, RunExit::SideExit { next_rip: 0x0f });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 2);
}

#[test]
fn jp_observes_pf_after_testflags_and32() {
    let mut bus = SimpleBus::new(64);
    bus.load(0, TEST_JP_CODE);
    let func = build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default());

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = 0;

    let exit = run_function(&func, &env, &mut bus, &mut state, 16);
    assert_eq!(exit, RunExit::SideExit { next_rip: 0x0e });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 2);
}
