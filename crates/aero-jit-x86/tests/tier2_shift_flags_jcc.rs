mod tier1_common;

use aero_cpu_core::state::{RFLAGS_CF, RFLAGS_OF};
use aero_jit_x86::tier2::interp::{run_function, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::ir::Function;
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_types::Gpr;
use tier1_common::SimpleBus;

fn run_x86_inner(code: &[u8], init_rflags: u64) -> (Function, RunExit, T2State) {
    let mut bus = SimpleBus::new(64);
    bus.load(0, code);
    let func = build_function_from_x86(&bus, 0, 64, CfgBuildConfig::default());

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rflags = init_rflags;
    state.cpu.rip = 0;

    let exit = run_function(&func, &env, &mut bus, &mut state, 16);
    (func, exit, state)
}

fn run_x86(code: &[u8]) -> (Function, RunExit, T2State) {
    // Make the initial flags explicit so the branch outcome depends on the shift.
    run_x86_inner(code, aero_jit_x86::abi::RFLAGS_RESERVED1)
}

fn run_x86_with_rflags(code: &[u8], init_rflags: u64) -> (Function, RunExit, T2State) {
    run_x86_inner(code, init_rflags)
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

// mov al, 0x81
// shl al, 1         ; CF=1
// jc taken          ; must take
// mov al, 0
// int3
// taken: mov al, 1
// int3
const SHL8_JC_CODE: &[u8] = &[
    0xB0, 0x81, // mov al, 0x81
    0xC0, 0xE0, 0x01, // shl al, 1
    0x72, 0x03, // jc +3
    0xB0, 0x00, // mov al, 0
    0xCC, // int3
    0xB0, 0x01, // mov al, 1
    0xCC, // int3
];

// Same as above, but branch on OF instead of CF.
// For `shl` with count==1, OF = CF XOR MSB(result). For 0x81 << 1, OF=1.
const SHL8_JO_CODE: &[u8] = &[
    0xB0, 0x81, // mov al, 0x81
    0xC0, 0xE0, 0x01, // shl al, 1
    0x70, 0x03, // jo +3
    0xB0, 0x00, // mov al, 0
    0xCC, // int3
    0xB0, 0x01, // mov al, 1
    0xCC, // int3
];

#[test]
fn tier2_shl8_updates_cf_observed_by_jc() {
    let (func, exit, state) = run_x86(SHL8_JC_CODE);

    // If Tier-2 can't lower the shift-with-flags form, it deopts at the block entry.
    assert!(!func.block(func.entry).instrs.is_empty());

    assert_eq!(exit, RunExit::SideExit { next_rip: 0x0c });
    assert_eq!(state.cpu.rip, 0x0c);
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 1);
}

#[test]
fn tier2_shl8_updates_of_observed_by_jo() {
    let (func, exit, state) = run_x86(SHL8_JO_CODE);
    assert!(!func.block(func.entry).instrs.is_empty());

    assert_eq!(exit, RunExit::SideExit { next_rip: 0x0c });
    assert_eq!(state.cpu.rip, 0x0c);
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 1);
}

#[test]
fn tier2_shift_count_0_leaves_cf_unchanged() {
    // mov al, 0x12
    // shl al, 0
    // jc +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // x86 shifts with count==0 do not update any flags. Ensure CF stays set and `jc` is taken.
    const CODE: &[u8] = &[
        0xB0, 0x12, // mov al, 0x12
        0xC0, 0xE0, 0x00, // shl al, 0
        0x72, 0x03, // jc +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let init_rflags = aero_jit_x86::abi::RFLAGS_RESERVED1 | RFLAGS_CF;
    let (_func, exit, state) = run_x86_with_rflags(CODE, init_rflags);

    assert_eq!(exit, RunExit::SideExit { next_rip: 12 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 2);
}

#[test]
fn tier2_shift_count_gt_1_leaves_of_unchanged() {
    // mov al, 1
    // shl al, 2
    // jo +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // x86 defines OF only for shift count==1. For count>1 it is undefined; Tier-1/Tier-2 conservatively
    // leave it unchanged. Seed OF=1 and ensure `jo` is taken.
    const CODE: &[u8] = &[
        0xB0, 0x01, // mov al, 1
        0xC0, 0xE0, 0x02, // shl al, 2
        0x70, 0x03, // jo +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let init_rflags = aero_jit_x86::abi::RFLAGS_RESERVED1 | RFLAGS_OF;
    let (_func, exit, state) = run_x86_with_rflags(CODE, init_rflags);

    assert_eq!(exit, RunExit::SideExit { next_rip: 12 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 2);
}

#[test]
fn tier2_shift_count_gt_operand_width_leaves_cf_unchanged() {
    // mov al, 1
    // shl al, 9
    // jc +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // x86 defines CF only for shift counts in [1, width.bits()]. For larger counts it is undefined;
    // Tier-1/Tier-2 conservatively leave it unchanged. Seed CF=1 and ensure `jc` is taken.
    const CODE: &[u8] = &[
        0xB0, 0x01, // mov al, 1
        0xC0, 0xE0, 0x09, // shl al, 9
        0x72, 0x03, // jc +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let init_rflags = aero_jit_x86::abi::RFLAGS_RESERVED1 | RFLAGS_CF;
    let (_func, exit, state) = run_x86_with_rflags(CODE, init_rflags);

    assert_eq!(exit, RunExit::SideExit { next_rip: 12 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 2);
}

#[test]
fn tier2_sar_count_1_sets_of_to_0() {
    // mov al, 0x81
    // sar al, 1
    // jo +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // For SAR count==1, x86 defines OF=0. Seed OF=1 and ensure `jo` is *not* taken.
    const CODE: &[u8] = &[
        0xB0, 0x81, // mov al, 0x81
        0xD0, 0xF8, // sar al, 1
        0x70, 0x03, // jo +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let init_rflags = aero_jit_x86::abi::RFLAGS_RESERVED1 | RFLAGS_OF;
    let (_func, exit, state) = run_x86_with_rflags(CODE, init_rflags);

    // Branch should not be taken; we should stop at the first int3.
    assert_eq!(exit, RunExit::SideExit { next_rip: 8 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 1);
}

#[test]
fn tier2_shr_count_1_sets_cf_from_old_lsb() {
    // mov al, 1
    // shr al, 1
    // jc +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // For SHR count==1, x86 defines CF=old LSB. With old LSB=1, ensure `jc` is taken.
    const CODE: &[u8] = &[
        0xB0, 0x01, // mov al, 1
        0xD0, 0xE8, // shr al, 1
        0x72, 0x03, // jc +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let (_func, exit, state) = run_x86(CODE);

    assert_eq!(exit, RunExit::SideExit { next_rip: 11 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 2);
}

#[test]
fn tier2_shr_count_1_sets_of_from_old_msb() {
    // mov al, 0x81
    // shr al, 1
    // jo +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // For SHR count==1, x86 defines OF=old MSB. With old MSB=1, ensure `jo` is taken.
    const CODE: &[u8] = &[
        0xB0, 0x81, // mov al, 0x81
        0xD0, 0xE8, // shr al, 1
        0x70, 0x03, // jo +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let (_func, exit, state) = run_x86(CODE);

    assert_eq!(exit, RunExit::SideExit { next_rip: 11 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 2);
}

#[test]
fn tier2_shl_updates_pf_observed_by_jp() {
    // mov al, 0x03
    // shl al, 1
    // jp +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // 0x03 << 1 = 0x06 (0b0000_0110) has even parity => PF=1 => JP taken.
    const CODE: &[u8] = &[
        0xB0, 0x03, // mov al, 0x03
        0xD0, 0xE0, // shl al, 1
        0x7A, 0x03, // jp +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let (_func, exit, state) = run_x86(CODE);
    assert_eq!(exit, RunExit::SideExit { next_rip: 11 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 2);
}

#[test]
fn tier2_shl_updates_zf_observed_by_jz() {
    // mov al, 0x00
    // shl al, 1
    // jz +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // 0x00 << 1 = 0 => ZF=1 => JZ taken.
    const CODE: &[u8] = &[
        0xB0, 0x00, // mov al, 0x00
        0xD0, 0xE0, // shl al, 1
        0x74, 0x03, // jz +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let (_func, exit, state) = run_x86(CODE);
    assert_eq!(exit, RunExit::SideExit { next_rip: 11 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 2);
}

#[test]
fn tier2_shl_updates_sf_observed_by_js() {
    // mov al, 0x40
    // shl al, 1
    // js +3
    // mov al, 1
    // int3
    // mov al, 2
    // int3
    //
    // 0x40 << 1 = 0x80 => SF=1 => JS taken.
    const CODE: &[u8] = &[
        0xB0, 0x40, // mov al, 0x40
        0xD0, 0xE0, // shl al, 1
        0x78, 0x03, // js +3
        0xB0, 0x01, // mov al, 1
        0xCC, // int3
        0xB0, 0x02, // mov al, 2
        0xCC, // int3
    ];

    let (_func, exit, state) = run_x86(CODE);
    assert_eq!(exit, RunExit::SideExit { next_rip: 11 });
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize] & 0xff, 2);
}
