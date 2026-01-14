mod tier1_common;

use aero_jit_x86::compiler::tier1::{compile_tier1_block_with_options, Tier1CompileError};
use aero_jit_x86::{BlockLimits, Tier1WasmOptions};
use tier1_common::SimpleBus;

#[test]
fn compile_tier1_bails_out_at_entry_for_unsupported_first_instruction() {
    let entry = 0x1000u64;

    // Ensure `BailoutAtEntry` detection works across all supported guest bitnesses.
    for bitness in [16u32, 32, 64] {
        // Pick an opcode that the Tier-1 minimal decoder treats as `InstKind::Invalid` so
        // compilation produces an "exit to interpreter at entry RIP" block.
        let invalid = tier1_common::pick_invalid_opcode(bitness);
        let code = [invalid];

        let mut bus = SimpleBus::new(0x2000);
        bus.load(entry, &code);

        let res = compile_tier1_block_with_options(
            &bus,
            entry,
            bitness,
            BlockLimits::default(),
            Tier1WasmOptions::default(),
        );
        assert!(
            matches!(res, Err(Tier1CompileError::BailoutAtEntry { entry_rip }) if entry_rip == entry),
            "expected BailoutAtEntry for bitness={bitness}, got {res:?}"
        );
    }
}
