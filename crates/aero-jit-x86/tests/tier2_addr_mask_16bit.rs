mod tier1_common;

use aero_jit_x86::tier2::interp::{run_function, RunExit, RuntimeEnv, T2State};
use aero_jit_x86::tier2::{build_function_from_x86, CfgBuildConfig};
use aero_types::Gpr;
use tier1_common::{pick_invalid_opcode, SimpleBus};

#[test]
fn tier2_masks_16bit_effective_addresses_for_memory_operands() {
    // mov ax, word ptr [di]
    // <invalid>
    //
    // Seed DI with high bits set; 16-bit addressing should ignore them.
    let entry = 0x1000u64;
    let invalid = pick_invalid_opcode(16);
    let code = [0x8b, 0x05, invalid];

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);
    bus.load(0, &0x1122u16.to_le_bytes());

    let func = build_function_from_x86(&bus, entry, 16, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 16-bit mov-load"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rdi.as_u8() as usize] = 0x1_0000; // masked to 0

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 2
        }
    );
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x1122);
}

#[test]
fn tier2_masks_16bit_effective_addresses_for_memory_stores() {
    // mov word ptr [di], ax
    // <invalid>
    //
    // Seed DI with high bits set; 16-bit addressing should ignore them.
    let entry = 0x1800u64;
    let invalid = pick_invalid_opcode(16);
    let code = [0x89, 0x05, invalid];

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);

    let func = build_function_from_x86(&bus, entry, 16, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 16-bit mov-store"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0xdead_beefu64;
    state.cpu.gpr[Gpr::Rdi.as_u8() as usize] = 0x1_0000; // masked to 0

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 2
        }
    );
    assert_eq!(&bus.mem()[0..2], &0xbeef_u16.to_le_bytes());
}

#[test]
fn tier2_masks_16bit_stack_pointer_for_pop() {
    // pop ax
    // <invalid>
    //
    // Place a 16-bit value at address 0; if SP is masked, POP reads it.
    let entry = 0x2000u64;
    let invalid = pick_invalid_opcode(16);
    let code = [0x58, invalid];

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);
    bus.load(0, &0x3344u16.to_le_bytes());

    let func = build_function_from_x86(&bus, entry, 16, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 16-bit pop"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0000; // masked to 0

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 1
        }
    );
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x3344);
    assert_eq!(state.cpu.gpr[Gpr::Rsp.as_u8() as usize], 2);
}

#[test]
fn tier2_masks_16bit_stack_pointer_for_push() {
    // push ax
    // <invalid>
    //
    // Place the stack at address 2 (architecturally), but seed SP with high bits set; 16-bit stack
    // semantics should ignore them and push the value at address 0.
    let entry = 0x2800u64;
    let invalid = pick_invalid_opcode(16);
    let code = [0x50, invalid];

    let mut bus = SimpleBus::new(0x4000);
    bus.load(entry, &code);

    let func = build_function_from_x86(&bus, entry, 16, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 16-bit push"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1122_3344u64;
    state.cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0002; // masked to 2

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 1
        }
    );
    assert_eq!(state.cpu.gpr[Gpr::Rsp.as_u8() as usize], 0);
    assert_eq!(&bus.mem()[0..2], &0x3344u16.to_le_bytes());
}
#[test]
fn tier2_masks_16bit_stack_pointer_wraps_on_push() {
    // push ax
    // <invalid>
    //
    // If SP is 0, a 16-bit PUSH should wrap to SP=0xFFFE (not underflow into 64-bit space).
    let entry = 0x3000u64;
    let invalid = pick_invalid_opcode(16);
    let code = [0x50, invalid];

    let mut bus = SimpleBus::new(0x10000);
    bus.load(entry, &code);

    let func = build_function_from_x86(&bus, entry, 16, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 16-bit push"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rax.as_u8() as usize] = 0x1122_3344u64;
    state.cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_0000; // masked to 0

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 1
        }
    );
    assert_eq!(state.cpu.gpr[Gpr::Rsp.as_u8() as usize], 0xfffe);
    assert_eq!(&bus.mem()[0xfffe..0x10000], &0x3344u16.to_le_bytes());
}

#[test]
fn tier2_masks_16bit_stack_pointer_wraps_on_pop() {
    // pop ax
    // <invalid>
    //
    // If SP is 0xFFFE, a 16-bit POP should wrap to SP=0.
    let entry = 0x3800u64;
    let invalid = pick_invalid_opcode(16);
    let code = [0x58, invalid];

    let mut bus = SimpleBus::new(0x10000);
    bus.load(entry, &code);
    bus.load(0xfffe, &0x5566u16.to_le_bytes());

    let func = build_function_from_x86(&bus, entry, 16, CfgBuildConfig::default());
    assert!(
        !func.block(func.entry).instrs.is_empty(),
        "unexpected deopt-at-entry when lowering 16-bit pop"
    );

    let env = RuntimeEnv::default();
    let mut state = T2State::default();
    state.cpu.rip = entry;
    state.cpu.gpr[Gpr::Rsp.as_u8() as usize] = 0x1_fffe; // masked to 0xfffe

    let exit = run_function(&func, &env, &mut bus, &mut state, 8);
    assert_eq!(
        exit,
        RunExit::SideExit {
            next_rip: entry + 1
        }
    );
    assert_eq!(state.cpu.gpr[Gpr::Rax.as_u8() as usize], 0x5566);
    assert_eq!(state.cpu.gpr[Gpr::Rsp.as_u8() as usize], 0);
}
