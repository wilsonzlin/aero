use aero_jit_x86::tier1::ir::{BinOp as T1BinOp, GuestReg, IrBuilder, IrTerminator};
use aero_jit_x86::tier2::builder::lower_tier1_ir_block_for_test;
use aero_jit_x86::tier2::ir::Terminator;
use aero_types::{FlagSet, Gpr, Width};

#[test]
fn tier2_deopts_shift_with_flags_when_count_is_not_constant() {
    let entry_rip = 0x1000;
    let mut b = IrBuilder::new(entry_rip);

    let lhs = b.const_int(Width::W64, 0x1234);
    let rhs = b.read_reg(GuestReg::Gpr {
        reg: Gpr::Rcx,
        width: Width::W64,
        high8: false,
    });

    // Request ALU flags to force the Tier-2 lowering path that requires a constant shift count.
    let _ = b.binop(
        T1BinOp::Shl,
        Width::W64,
        lhs,
        rhs,
        FlagSet::ALU.without(FlagSet::AF),
    );

    let ir = b.finish(IrTerminator::ExitToInterpreter {
        next_rip: entry_rip,
    });
    let block = lower_tier1_ir_block_for_test(&ir);

    assert!(
        block.instrs.is_empty(),
        "expected unsupported shift-with-flags lowering to deopt at block entry"
    );
    match block.term {
        Terminator::SideExit { exit_rip } => assert_eq!(exit_rip, entry_rip),
        other => panic!("expected SideExit terminator, got {other:?}"),
    }
}
