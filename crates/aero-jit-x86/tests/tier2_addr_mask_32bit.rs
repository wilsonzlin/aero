mod tier1_common;

use aero_jit_x86::tier2::interp::{run_function, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_types::Gpr;
use tier1_common::{pick_invalid_opcode, SimpleBus};

#[test]
fn tier2_masks_32bit_effective_addresses_for_memory_operands() {
    // mov eax, dword ptr [edi]
    // <invalid>
    //
    // Seed EDI with high bits set; 32-bit addressing should ignore them.
    let entry = 0x1000u64;
    let invalid = pick_invalid_opcode(32);
    let code = [0x8b, 0x07, invalid];

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);
    bus.load(0, &0x1122_3344u32.to_le_bytes());

    let func = build_function_from_x86(&bus, entry, 32, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 32-bit mov-load"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rdi.as_u8() as usize] = 0x1_0000_0000; // masked to 0

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 2
        }
    );
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1122_3344);
}

#[test]
fn tier2_masks_32bit_effective_addresses_for_memory_stores() {
    // mov dword ptr [edi], eax
    // <invalid>
    //
    // Seed EDI with high bits set; 32-bit addressing should ignore them.
    let entry = 0x1800u64;
    let invalid = pick_invalid_opcode(32);
    let code = [0x89, 0x07, invalid];

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);

    let func = build_function_from_x86(&bus, entry, 32, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 32-bit mov-store"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0xdead_beefu64;
    state.cpu.gpr[Gpr::Rdi.as_u8() as usize] = 0x1_0000_0000; // masked to 0

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 2
        }
    );
    assert_eq!(&bus.mem()[0..4], &0xdead_beefu32.to_le_bytes());
}

#[test]
fn tier2_masks_32bit_stack_pointer_for_pop() {
    // pop eax
    // <invalid>
    //
    // Place a 32-bit value at address 0; if ESP is masked, POP reads it.
    let entry = 0x2000u64;
    let invalid = pick_invalid_opcode(32);
    let code = [0x58, invalid];

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);
    bus.load(0, &0x5566_7788u32.to_le_bytes());

    let func = build_function_from_x86(&bus, entry, 32, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 32-bit pop"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0000_0000; // masked to 0

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 1
        }
    );
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x5566_7788);
    assert_eq!(state.cpu.gpr[Gpr::Rsp.as_u8() as usize], 4);
}

#[test]
fn tier2_masks_32bit_stack_pointer_for_push() {
    // push eax
    // <invalid>
    //
    // Place the stack at address 4 (architecturally), but seed ESP with high bits set; 32-bit stack
    // semantics should ignore them and push the value at address 0.
    let entry = 0x2800u64;
    let invalid = pick_invalid_opcode(32);
    let code = [0x50, invalid];

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);

    let func = build_function_from_x86(&bus, entry, 32, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 32-bit push"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1122_3344u64;
    state.cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0000_0004; // masked to 4

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 1
        }
    );
    assert_eq!(state.cpu.gpr[Gpr::Rsp.as_u8() as usize], 0);
    assert_eq!(&bus.mem()[0..4], &0x1122_3344u32.to_le_bytes());
}
