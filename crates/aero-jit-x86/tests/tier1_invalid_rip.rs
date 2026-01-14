mod tier1_common;

use aero_jit_x86::tier1::ir::IrTerminator;
use aero_jit_x86::{discover_block_mode, translate_block, BlockEndKind, BlockLimits};
use tier1_common::SimpleBus;

#[test]
fn invalid_instruction_does_not_advance_rip() {
    let entry_rip = 0x1000u64;
    for bitness in [16u32, 32, 64] {
        // add ax/eax, 1
        // <unsupported opcode>  (decoded as InstKind::Invalid)
        let invalid = tier1_common::pick_invalid_opcode(bitness);
        let code = [0x83, 0xC0, 0x01, invalid];

        let mut bus = SimpleBus::new(0x2000);
        bus.load(entry_rip, &code);

        let block = discover_block_mode(&bus, entry_rip, BlockLimits::default(), bitness);
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
}
