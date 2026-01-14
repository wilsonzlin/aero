mod tier1_common;

use aero_jit_x86::tier2::interp::{run_function, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_types::Gpr;
use tier1_common::SimpleBus;

#[test]
fn tier2_does_not_silently_drop_shift_flags_used_by_jcc() {
    // mov al, 0x81
    // shl al, 1
    // jc taken
    // mov al, 0
    // int3
    // taken:
    // mov al, 1
    // int3
    //
    // For the given input, SHL sets CF=1, so JC must be taken.
    const CODE: &[u8] = &[
        0xB0, 0x81, // mov al, 0x81
        0xC0, 0xE0, 0x01, // shl al, 1
        0x72, 0x03, // jc +3 (to mov al, 1)
        0xB0, 0x00, // mov al, 0
        0xCC, // int3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
    ];

    let mut bus = SimpleBus::new(64);
    bus.load(0, CODE);
    let func = build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default());

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    let exit = run_function(&func, &env, &mut bus, &mut state, 10);

    // Tier-2 lowers x86 shift flag updates explicitly, so it must take the carry branch and reach
    // the final int3.
    assert_eq!(exit, RunExit::SideExit { next_rip: 12 });
    assert_eq!(
        state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff,
        1,
        "expected JC taken and AL=1"
    );
}
