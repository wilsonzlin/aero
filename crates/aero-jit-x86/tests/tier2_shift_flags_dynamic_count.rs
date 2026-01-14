use aero_jit_x86::tier1::ir::{BinOp as T1BinOp, GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier2::builder::lower_tier1_ir_block_for_test;
use aero_jit_x86::tier2::ir::Terminator;
use aero_types::{FlagSet, Gpr, Width};

#[test]
fn tier2_deopts_shift_with_flags_when_count_is_not_constant() {
    let entry_rip = 0x1000u64;

    // Tier-2 can only lower x86 shift flags when the shift count is a constant. Tier-1 currently
    // only produces constant shift counts (immediate-count forms), but if that ever changes we
    // should deopt-at-entry rather than silently miscompiling shift flag updates.
    let mut b = IrBuilder::new(entry_rip);

    let lhs = b.read_reg(GuestReg::Gpr {
        reg: Gpr::Rax,
        width: Width::W64,
        high8: false,
    });
    // Non-constant shift count.
    let rhs = b.read_reg(GuestReg::Gpr {
        reg: Gpr::Rcx,
        width: Width::W64,
        high8: false,
    });

    let res = b.binop(
        T1BinOp::Shl,
        Width::W64,
        lhs,
        rhs,
        // x86 shifts update CF/PF/ZF/SF/OF (AF is architecturally undefined).
        FlagSet::ALU.without(FlagSet::AF),
    );
    b.write_reg(
        GuestReg::Gpr {
            reg: Gpr::Rbx,
            width: Width::W64,
            high8: false,
        },
        res,
    );

    let ir = b.finish(IrTerminator::ExitToInterpreter {
        next_rip: entry_rip + 1,
    });
    ir.validate().unwrap();

    let block = lower_tier1_ir_block_for_test(&ir);

    assert_eq!(block.start_rip, entry_rip);
    assert!(
        block.instrs.is_empty(),
        "expected deopt-at-entry (empty block), but got lowered instructions"
    );
    assert!(
        matches!(block.term, Terminator::SideExit { exit_rip } if exit_rip == entry_rip),
        "expected deopt-at-entry SideExit at {entry_rip:#x}, got {:?}",
        block.term
    );
}
