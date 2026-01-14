mod tier1_common;

use aero_jit_x86::tier2::interp::{run_function, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::ir::Function;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_types::Gpr;
use tier1_common::SimpleBus;

fn run_x86(code: &[u8]) -> (Function, RunExit, T2State) {
    let mut bus = SimpleBus::new(64);
    bus.load(0, code);
    let func = build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default());

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    let exit = run_function(&func, &env, &mut bus, &mut state, 10);
    (func, exit, state)
}

#[test]
fn tier2_shift_flag_updates_drive_jc_taken() {
    // mov al, 0x80
    // shl al, 1
    // jc +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // `jc` depends on CF written by `shl`. Tier-2 lowers x86 shift flag updates explicitly, so the
    // branch should be taken and we should hit the second int3.
    const CODE: &[u8] = &[
        0xB0, 0x80, // mov al, 0x80
        0xD0, 0xE0, // shl al, 1
        0x72, 0x03, // jc +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let (func, exit, state) = run_x86(CODE);

    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering shl-with-flags"
    );
    assert_eq!(exit, RunExit::SideExit { next_rip: 11 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 2);
}
