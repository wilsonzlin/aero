mod tier1_common;

use aero_jit_x86::compiler::tier1::compile_tier1_block;
use aero_jit_x86::BlockLimits;
use tier1_common::SimpleBus;

#[test]
fn tier1_compilation_byte_len_excludes_trailing_invalid() {
    // push rbx; nop (unsupported by Tier-1 decoder)
    //
    // Tier-1 should compile a block that executes the PUSH and then side-exits to the interpreter
    // at the unsupported NOP instruction. The `byte_len` metadata should cover only the executed
    // bytes (ie. exclude the trailing Invalid instruction).
    let entry = 0x1000u64;
    let mut bus = SimpleBus::new(0x2000);
    bus.load(entry, &[0x53, 0x90]);

    let compilation =
        compile_tier1_block(&bus, entry, 64, BlockLimits::default()).expect("compile_tier1_block");

    assert!(compilation.exit_to_interpreter);
    assert_eq!(compilation.instruction_count, 1);
    assert_eq!(compilation.byte_len, 1);
}
