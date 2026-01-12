mod tier1_common;

use aero_jit_x86::tier1::ir::IrTerminator;
use aero_jit_x86::{discover_block, translate_block, BlockEndKind, BlockLimits};
use tier1_common::SimpleBus;

#[test]
fn invalid_instruction_does_not_advance_rip() {
    // add eax, 1
    // int3  (unsupported in the Tier-1 decoder -> InstKind::Invalid)
    let code = [0x83, 0xC0, 0x01, 0xCC];

    let entry_rip = 0x1000u64;
    let mut bus = SimpleBus::new(0x2000);
    bus.load(entry_rip, &code);

    let block = discover_block(&bus, entry_rip, BlockLimits::default());
    assert_eq!(
        block.end_kind,
        BlockEndKind::ExitToInterpreter { next_rip: 0x1003 }
    );

    let ir = translate_block(&block);
    assert_eq!(
        ir.terminator,
        IrTerminator::ExitToInterpreter { next_rip: 0x1003 }
    );
}
