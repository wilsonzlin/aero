use aero_jit_x86::tier1::ir::{GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier2::builder::lower_tier1_ir_block_for_test;
use aero_jit_x86::tier2::ir::{Instr, Terminator};
use aero_types::{Gpr, Width};

#[test]
fn tier2_tolerates_write_rip() {
    let entry_rip = 0x1000u64;

    // Construct a minimal Tier-1 IR block that writes to RIP (as Tier-1 per-instruction RIP
    // tracking does) and then performs a supported operation.
    let mut b = IrBuilder::new(entry_rip);

    // Tier-1 IR represents RIP as a normal 64-bit value.
    let new_rip = b.const_int(Width::W64, entry_rip + 1);
    b.write_reg(GuestReg::Rip, new_rip);

    // Do something Tier-2 can lower so we can detect "deopt at entry" (which produces an empty
    // Tier-2 block).
    let val = b.const_int(Width::W64, 0x1234);
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rax,
            width: Width::W64,
            high8: false,
        },
        val,
    );

    let ir = b.finish(IrTerminator::ExitToInterpreter {
        next_rip: entry_rip + 2,
    });
    ir.validate().unwrap();

    let block = lower_tier1_ir_block_for_test(&ir);

    assert!(
        !matches!(block.term, Terminator::SideExit { exit_rip } if exit_rip == block.start_rip),
        "expected Tier-2 lowering to tolerate write.rip (no deopt-at-entry), but got SideExit at entry rip"
    );
    assert!(
        !block.instrs.is_empty(),
        "expected Tier-2 lowering to produce instructions (no deopt-at-entry), but block was empty"
    );
    assert!(
        block
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::StoreReg { reg, .. } if *reg == Gpr::Rax)),
        "expected lowered Tier-2 block to contain a StoreReg to RAX"
    );
}
