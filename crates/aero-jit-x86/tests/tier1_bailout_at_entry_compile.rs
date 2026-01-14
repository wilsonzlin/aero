mod tier1_common;

use aero_jit_x86::compiler::tier1::{compile_tier1_block_with_options, Tier1CompileError};
use aero_jit_x86::{BlockLimits, Tier1WasmOptions};
use tier1_common::SimpleBus;

#[test]
fn compile_tier1_bails_out_at_entry_for_unsupported_first_instruction() {
    let entry = 0x1000u64;

    // 0xF5 = CMC, which is currently unsupported by the Tier-1 decoder and is decoded as
    // `InstKind::Invalid`.
    let code = [0xF5];

    let mut bus = SimpleBus::new(0x2000);
    bus.load(entry, &code);

    let res = compile_tier1_block_with_options(
        &bus,
        entry,
        64,
        BlockLimits::default(),
        Tier1WasmOptions::default(),
    );
    assert!(
        matches!(res, Err(Tier1CompileError::BailoutAtEntry { entry_rip }) if entry_rip == entry),
        "expected BailoutAtEntry, got {res:?}"
    );
}
