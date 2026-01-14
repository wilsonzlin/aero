use std::collections::{HashMap, VecDeque};

use aero_types::{Cond, Flag, FlagSet, Width};
use aero_x86::tier1::InstKind;

use crate::tier1::ir::{
    BinOp as T1BinOp, GuestReg, IrBlock, IrInst, IrTerminator, ValueId as T1ValueId,
};
use crate::tier1::{discover_block_mode, translate_block, BasicBlock, BlockEndKind, BlockLimits};
use crate::Tier1Bus;

use super::ir::{BinOp, Block, BlockId, Function, Instr, Operand, Terminator, ValueId};
#[derive(Clone, Copy, Debug)]
pub struct CfgBuildConfig {
    /// Maximum number of basic blocks to discover before stopping exploration.
    pub max_blocks: usize,
    /// Per-block decoding limits (instruction + byte budget).
    pub block_limits: BlockLimits,
}

impl Default for CfgBuildConfig {
    fn default() -> Self {
        Self {
            max_blocks: 1024,
            block_limits: BlockLimits::default(),
        }
    }
}

/// Build a Tier-2 CFG by discovering x86 basic blocks starting at `entry_rip`, translating them
/// through Tier-1 IR, and lowering into [`Function`].
///
/// Invariants:
/// - Every [`ValueId`] in the resulting [`Function`] is globally unique across all blocks (Tier-1
///   blocks start value numbering at 0, so we offset per block during lowering).
/// - CFG-level bailouts (unsupported IR, indirect jumps, decode failures) are represented via
///   [`Terminator::SideExit`] rather than in-block [`Instr::SideExit`].
#[must_use]
pub fn build_function_from_x86<B: Tier1Bus>(
    bus: &B,
    entry_rip: u64,
    bitness: u32,
    cfg: CfgBuildConfig,
) -> Function {
    Tier2CfgBuilder::new(bus, bitness, cfg).build(entry_rip)
}

struct Tier2CfgBuilder<'a, B: Tier1Bus> {
    bus: &'a B,
    bitness: u32,
    ip_mask: u64,
    cfg: CfgBuildConfig,
    rip_to_block: HashMap<u64, BlockId>,
    blocks: Vec<Option<Block>>,
    queue: VecDeque<u64>,
    next_value: u32,
}

impl<'a, B: Tier1Bus> Tier2CfgBuilder<'a, B> {
    fn new(bus: &'a B, bitness: u32, cfg: CfgBuildConfig) -> Self {
        let ip_mask = match bitness {
            32 => 0xffff_ffff,
            64 => u64::MAX,
            16 => 0xffff,
            other => panic!("invalid x86 bitness {other}"),
        };
        Self {
            bus,
            bitness,
            ip_mask,
            cfg,
            rip_to_block: HashMap::new(),
            blocks: Vec::new(),
            queue: VecDeque::new(),
            next_value: 0,
        }
    }

    fn build(mut self, entry_rip: u64) -> Function {
        let entry_rip = entry_rip & self.ip_mask;
        let entry = match self.get_or_create_block(entry_rip) {
            Some(id) => id,
            None => {
                // Degenerate case: max_blocks == 0.
                let id = BlockId(0);
                return Function {
                    blocks: vec![Block {
                        id,
                        start_rip: entry_rip,
                        code_len: 0,
                        instrs: Vec::new(),
                        term: Terminator::SideExit {
                            exit_rip: entry_rip,
                        },
                    }],
                    entry: id,
                };
            }
        };

        while let Some(rip) = self.queue.pop_front() {
            let id = self.rip_to_block[&rip];
            if self.blocks[id.index()].is_some() {
                continue;
            }

            let bb = discover_block_mode(self.bus, rip, self.cfg.block_limits, self.bitness);
            let t2_block = self.lower_block(id, &bb);
            self.blocks[id.index()] = Some(t2_block);
        }

        let blocks = self
            .blocks
            .into_iter()
            .map(|b| b.expect("missing block"))
            .collect();
        Function { blocks, entry }
    }

    fn get_or_create_block(&mut self, rip: u64) -> Option<BlockId> {
        let rip = rip & self.ip_mask;
        if let Some(id) = self.rip_to_block.get(&rip).copied() {
            return Some(id);
        }
        if self.blocks.len() >= self.cfg.max_blocks {
            return None;
        }
        let id = BlockId(self.blocks.len() as u32);
        self.rip_to_block.insert(rip, id);
        self.blocks.push(None);
        self.queue.push_back(rip);
        Some(id)
    }

    fn lower_block(&mut self, id: BlockId, bb: &BasicBlock) -> Block {
        // `code_len` tracks the byte span of *guest code bytes that are executed by the block* for
        // code version guards.
        //
        // If the last decoded instruction is `Invalid`, Tier-1 side-exits to the interpreter at
        // that instruction's RIP and does not execute it, so we exclude it from the covered byte
        // range (mirroring `compiler/tier1.rs`).
        let mut code_len: u32 = bb
            .insts
            .iter()
            .fold(0u32, |acc, inst| acc.saturating_add(inst.len as u32));
        if let Some(last) = bb.insts.last() {
            if matches!(last.kind, InstKind::Invalid) {
                // Corner case: if the block is invalid at entry, code_len becomes 0.
                code_len = code_len.saturating_sub(last.len as u32);
            }
        }
        let ir = translate_block(bb);

        let value_count: u32 = ir
            .value_types
            .len()
            .try_into()
            .expect("Tier-1 IR value count overflows u32");
        let base = self.next_value;
        self.next_value = self
            .next_value
            .checked_add(value_count)
            .expect("Tier-2 ValueId space exhausted");

        let (instrs, unsupported) = {
            let mut lower = BlockLowerer::new(bb.entry_rip, base, &mut self.next_value);
            lower.lower_block(&ir);
            (lower.instrs, lower.unsupported)
        };

        let term = lower_terminator(self, bb, &ir, base);

        // If we hit an unsupported operation (or could not represent the terminator within the
        // current CFG budget), conservatively side-exit at the *start* of the block so that the
        // interpreter can re-execute it from a clean architectural state.
        if unsupported || matches!(term, TerminatorLowering::DeoptAtEntry) {
            return Block {
                id,
                start_rip: bb.entry_rip,
                code_len,
                instrs: Vec::new(),
                term: Terminator::SideExit {
                    exit_rip: bb.entry_rip,
                },
            };
        }

        let TerminatorLowering::Term(term) = term else {
            unreachable!();
        };

        Block {
            id,
            start_rip: bb.entry_rip,
            code_len,
            instrs,
            term,
        }
    }
}

enum TerminatorLowering {
    Term(Terminator),
    /// The block must side-exit at its entry RIP, and must not execute any lowered instructions.
    DeoptAtEntry,
}

fn lower_terminator<B: Tier1Bus>(
    builder: &mut Tier2CfgBuilder<'_, B>,
    bb: &BasicBlock,
    ir: &IrBlock,
    base: u32,
) -> TerminatorLowering {
    match ir.terminator {
        IrTerminator::Jump { target } => {
            let target = target & builder.ip_mask;
            match builder.get_or_create_block(target) {
                Some(id) => TerminatorLowering::Term(Terminator::Jump(id)),
                // We can always side-exit to a known absolute RIP.
                None => TerminatorLowering::Term(Terminator::SideExit { exit_rip: target }),
            }
        }
        IrTerminator::CondJump {
            cond,
            target,
            fallthrough,
        } => {
            let target = target & builder.ip_mask;
            let fallthrough = fallthrough & builder.ip_mask;
            match (
                builder.get_or_create_block(target),
                builder.get_or_create_block(fallthrough),
            ) {
                (Some(then_bb), Some(else_bb)) => TerminatorLowering::Term(Terminator::Branch {
                    cond: Operand::Value(ValueId(
                        base.checked_add(cond.0)
                            .expect("Tier-2 ValueId space exhausted"),
                    )),
                    then_bb,
                    else_bb,
                }),
                // Can't represent a conditional transfer to unknown blocks; conservatively deopt at
                // the block entry.
                _ => TerminatorLowering::DeoptAtEntry,
            }
        }
        // Dynamic targets can't be represented in the Tier-2 CFG. Deopt and let the interpreter
        // re-execute the block (including the control-flow instruction).
        IrTerminator::IndirectJump { .. } => TerminatorLowering::DeoptAtEntry,
        IrTerminator::ExitToInterpreter { next_rip } => {
            let next_rip = next_rip & builder.ip_mask;
            match bb.end_kind {
                BlockEndKind::Limit {
                    next_rip: limit_rip,
                } => {
                    debug_assert_eq!(next_rip, limit_rip);
                    match builder.get_or_create_block(next_rip) {
                        Some(id) => TerminatorLowering::Term(Terminator::Jump(id)),
                        None => {
                            TerminatorLowering::Term(Terminator::SideExit { exit_rip: next_rip })
                        }
                    }
                }
                _ => TerminatorLowering::Term(Terminator::SideExit { exit_rip: next_rip }),
            }
        }
    }
}

struct BlockLowerer<'a> {
    entry_rip: u64,
    base: u32,
    next_value: &'a mut u32,
    instrs: Vec<Instr>,
    unsupported: bool,
    /// Per-block Tier-1 `Const` values keyed by the Tier-1 `ValueId`.
    const_values: HashMap<T1ValueId, u64>,
}

impl<'a> BlockLowerer<'a> {
    fn new(entry_rip: u64, base: u32, next_value: &'a mut u32) -> Self {
        Self {
            entry_rip,
            base,
            next_value,
            instrs: Vec::new(),
            unsupported: false,
            const_values: HashMap::new(),
        }
    }

    fn map_value(&self, v: T1ValueId) -> ValueId {
        ValueId(
            self.base
                .checked_add(v.0)
                .expect("Tier-2 ValueId space exhausted"),
        )
    }

    fn value(&self, v: T1ValueId) -> Operand {
        Operand::Value(self.map_value(v))
    }

    fn fresh_temp(&mut self) -> ValueId {
        let id = ValueId(*self.next_value);
        *self.next_value = self
            .next_value
            .checked_add(1)
            .expect("Tier-2 ValueId space exhausted");
        id
    }

    fn lower_block(&mut self, block: &IrBlock) {
        for inst in &block.insts {
            self.lower_inst(inst);
            if self.unsupported {
                return;
            }
        }
    }

    fn lower_inst(&mut self, inst: &IrInst) {
        match *inst {
            IrInst::Const { dst, value, .. } => {
                self.instrs.push(Instr::Const {
                    dst: self.map_value(dst),
                    value,
                });
                self.const_values.insert(dst, value);
            }
            IrInst::ReadReg { dst, reg } => self.lower_read_reg(dst, reg),
            IrInst::WriteReg { reg, src } => self.lower_write_reg(reg, src),
            IrInst::Trunc { dst, src, width } => self.lower_trunc(dst, src, width),
            IrInst::Load { dst, addr, width } => {
                self.instrs.push(Instr::LoadMem {
                    dst: self.map_value(dst),
                    addr: self.value(addr),
                    width,
                });
            }
            IrInst::Store { addr, src, width } => {
                self.instrs.push(Instr::StoreMem {
                    addr: self.value(addr),
                    src: self.value(src),
                    width,
                });
            }
            IrInst::BinOp {
                dst,
                op,
                lhs,
                rhs,
                width,
                flags,
            } => self.lower_binop(dst, op, lhs, rhs, width, flags),
            IrInst::CmpFlags {
                lhs,
                rhs,
                width,
                flags,
            } => self.lower_flag_op(BinOp::Sub, lhs, rhs, width, flags),
            IrInst::TestFlags {
                lhs,
                rhs,
                width,
                flags,
            } => self.lower_flag_op(BinOp::And, lhs, rhs, width, flags),
            IrInst::EvalCond { dst, cond } => self.lower_eval_cond(dst, cond),
            IrInst::Select {
                dst,
                cond,
                if_true,
                if_false,
                width,
            } => self.lower_select(dst, cond, if_true, if_false, width),
            IrInst::CallHelper { .. } => self.unsupported = true,
        }
    }

    fn lower_read_reg(&mut self, dst: T1ValueId, reg: GuestReg) {
        let dst = self.map_value(dst);
        match reg {
            GuestReg::Rip => {
                self.instrs.push(Instr::Const {
                    dst,
                    value: self.entry_rip,
                });
            }
            GuestReg::Gpr { reg, width, high8 } => {
                if width == Width::W64 && !high8 {
                    self.instrs.push(Instr::LoadReg { dst, reg });
                    return;
                }

                let full = self.fresh_temp();
                self.instrs.push(Instr::LoadReg { dst: full, reg });

                if width == Width::W8 && high8 {
                    let shifted = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: shifted,
                        op: BinOp::Shr,
                        lhs: Operand::Value(full),
                        rhs: Operand::Const(8),
                        flags: FlagSet::EMPTY,
                    });
                    self.instrs.push(Instr::BinOp {
                        dst,
                        op: BinOp::And,
                        lhs: Operand::Value(shifted),
                        rhs: Operand::Const(0xff),
                        flags: FlagSet::EMPTY,
                    });
                    return;
                }

                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(full),
                    rhs: Operand::Const(width.mask()),
                    flags: FlagSet::EMPTY,
                });
            }
            GuestReg::Flag(flag) => {
                self.instrs.push(Instr::LoadFlag { dst, flag });
            }
        }
    }

    fn lower_write_reg(&mut self, reg: GuestReg, src: T1ValueId) {
        match reg {
            GuestReg::Rip => {
                // Tier-2 does not currently model RIP as a first-class SSA value.
                //
                // Tier-1 may emit `write.rip` as per-instruction metadata (e.g. to report the
                // current guest RIP on runtime exits). Tier-2 treats those writes as non-semantic
                // and ignores them so they don't force CFG-level deopts.
            }
            GuestReg::Flag(_) => {
                // Tier-2 IR does not currently model direct flag writes; they should be expressed
                // via `BinOp`/`CmpFlags`/`TestFlags` flag updates.
                self.unsupported = true;
            }
            GuestReg::Gpr { reg, width, high8 } => {
                let src = self.value(src);

                if width == Width::W64 && !high8 {
                    self.instrs.push(Instr::StoreReg { reg, src });
                    return;
                }

                if width == Width::W32 {
                    // 32-bit writes zero-extend into the full 64-bit register.
                    let masked = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: masked,
                        op: BinOp::And,
                        lhs: src,
                        rhs: Operand::Const(width.mask()),
                        flags: FlagSet::EMPTY,
                    });
                    self.instrs.push(Instr::StoreReg {
                        reg,
                        src: Operand::Value(masked),
                    });
                    return;
                }

                // 8/16-bit writes preserve the upper bits (or bits 8..15 for AH..BH).
                let shift: u32 = if width == Width::W8 && high8 { 8 } else { 0 };
                let field_mask = if shift == 0 {
                    width.mask()
                } else {
                    0xffu64 << shift
                };
                let preserve_mask = !field_mask;

                let old = self.fresh_temp();
                self.instrs.push(Instr::LoadReg { dst: old, reg });

                let cleared = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: cleared,
                    op: BinOp::And,
                    lhs: Operand::Value(old),
                    rhs: Operand::Const(preserve_mask),
                    flags: FlagSet::EMPTY,
                });

                let masked_src = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: masked_src,
                    op: BinOp::And,
                    lhs: src,
                    rhs: Operand::Const(width.mask()),
                    flags: FlagSet::EMPTY,
                });

                let part = if shift == 0 {
                    Operand::Value(masked_src)
                } else {
                    let shifted = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: shifted,
                        op: BinOp::Shl,
                        lhs: Operand::Value(masked_src),
                        rhs: Operand::Const(shift as u64),
                        flags: FlagSet::EMPTY,
                    });
                    Operand::Value(shifted)
                };

                let new_val = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: new_val,
                    op: BinOp::Or,
                    lhs: Operand::Value(cleared),
                    rhs: part,
                    flags: FlagSet::EMPTY,
                });

                self.instrs.push(Instr::StoreReg {
                    reg,
                    src: Operand::Value(new_val),
                });
            }
        }
    }

    fn lower_trunc(&mut self, dst: T1ValueId, src: T1ValueId, width: Width) {
        self.instrs.push(Instr::BinOp {
            dst: self.map_value(dst),
            op: BinOp::And,
            lhs: self.value(src),
            rhs: Operand::Const(width.mask()),
            flags: FlagSet::EMPTY,
        });
    }

    fn lower_binop(
        &mut self,
        dst: T1ValueId,
        op: T1BinOp,
        lhs: T1ValueId,
        rhs: T1ValueId,
        width: Width,
        flags: FlagSet,
    ) {
        let Some(op) = map_binop(op) else {
            self.unsupported = true;
            return;
        };
        let dst = self.map_value(dst);
        let flags = map_flagset(flags);

        // Tier-2 `eval_binop` does not model x86 shift flag semantics.
        //
        // When Tier-1 requests flag updates for a shift, expand it into an explicit sequence of
        // Tier-2 ops that compute the architecturally correct flags when the shift count is a
        // constant. If the shift count is not a constant, conservatively mark the block as
        // unsupported (deopt at the block entry).
        if matches!(op, BinOp::Shl | BinOp::Shr | BinOp::Sar) && !flags.is_empty() {
            self.lower_shift_with_flags(dst, op, lhs, rhs, width, flags);
            return;
        }

        if width == Width::W64 {
            self.instrs.push(Instr::BinOp {
                dst,
                op,
                lhs: self.value(lhs),
                rhs: self.value(rhs),
                flags,
            });
            return;
        }

        match op {
            BinOp::Add | BinOp::Sub | BinOp::And | BinOp::Or | BinOp::Xor => {
                let shift = 64 - width.bits();
                let pf_af_mask = FlagSet::PF.union(FlagSet::AF);
                let flags_shift = flags.without(pf_af_mask);
                let flags_pf_af = flags.without(flags_shift);

                let lhs_s = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: lhs_s,
                    op: BinOp::Shl,
                    lhs: self.value(lhs),
                    rhs: Operand::Const(shift as u64),
                    flags: FlagSet::EMPTY,
                });
                let rhs_s = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: rhs_s,
                    op: BinOp::Shl,
                    lhs: self.value(rhs),
                    rhs: Operand::Const(shift as u64),
                    flags: FlagSet::EMPTY,
                });

                let res_s = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: res_s,
                    op,
                    lhs: Operand::Value(lhs_s),
                    rhs: Operand::Value(rhs_s),
                    // For narrow ops, the shift-left trick yields correct CF/OF/SF/ZF semantics,
                    // but it breaks PF/AF because the low byte is forced to 0. Compute PF/AF using
                    // an unshifted operation below.
                    flags: flags_shift,
                });

                if !flags_pf_af.is_empty() {
                    let tmp = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: tmp,
                        op,
                        lhs: self.value(lhs),
                        rhs: self.value(rhs),
                        flags: flags_pf_af,
                    });
                }

                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Shr,
                    lhs: Operand::Value(res_s),
                    rhs: Operand::Const(shift as u64),
                    flags: FlagSet::EMPTY,
                });
            }
            BinOp::Shl | BinOp::Shr => {
                debug_assert!(
                    flags.is_empty(),
                    "shift ops with flags should be handled by lower_shift_with_flags"
                );

                let mask = width.mask();
                let lhs_masked = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: lhs_masked,
                    op: BinOp::And,
                    lhs: self.value(lhs),
                    rhs: Operand::Const(mask),
                    flags: FlagSet::EMPTY,
                });

                // x86 shift counts for 8/16/32-bit operations are masked to 5 bits (mod 32).
                // Tier-2 IR shift ops mask by 6 bits (mod 64), so we must apply the x86 mask here.
                let rhs_masked = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: rhs_masked,
                    op: BinOp::And,
                    lhs: self.value(rhs),
                    rhs: Operand::Const(31),
                    flags: FlagSet::EMPTY,
                });

                let shifted = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: shifted,
                    op,
                    lhs: Operand::Value(lhs_masked),
                    rhs: Operand::Value(rhs_masked),
                    flags: FlagSet::EMPTY,
                });

                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(shifted),
                    rhs: Operand::Const(mask),
                    flags: FlagSet::EMPTY,
                });
            }
            BinOp::Sar => {
                debug_assert!(
                    flags.is_empty(),
                    "shift ops with flags should be handled by lower_shift_with_flags"
                );

                let mask = width.mask();

                // 1) Mask to the operand width.
                let lhs_masked = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: lhs_masked,
                    op: BinOp::And,
                    lhs: self.value(lhs),
                    rhs: Operand::Const(mask),
                    flags: FlagSet::EMPTY,
                });

                // 2) Sign-extend to 64 bits using (x << shift) >>_arith shift.
                let shift = 64 - width.bits();
                let lhs_shl = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: lhs_shl,
                    op: BinOp::Shl,
                    lhs: Operand::Value(lhs_masked),
                    rhs: Operand::Const(shift as u64),
                    flags: FlagSet::EMPTY,
                });
                let lhs_sext = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: lhs_sext,
                    op: BinOp::Sar,
                    lhs: Operand::Value(lhs_shl),
                    rhs: Operand::Const(shift as u64),
                    flags: FlagSet::EMPTY,
                });

                // x86 shift counts for 8/16/32-bit operations are masked to 5 bits (mod 32).
                // Tier-2 IR shift ops mask by 6 bits (mod 64), so we must apply the x86 mask here.
                let rhs_masked = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: rhs_masked,
                    op: BinOp::And,
                    lhs: self.value(rhs),
                    rhs: Operand::Const(31),
                    flags: FlagSet::EMPTY,
                });

                // 3) Shift arithmetically by the dynamic rhs.
                let shifted = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: shifted,
                    op: BinOp::Sar,
                    lhs: Operand::Value(lhs_sext),
                    rhs: Operand::Value(rhs_masked),
                    flags: FlagSet::EMPTY,
                });

                // 4) Mask result back to the operand width.
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(shifted),
                    rhs: Operand::Const(mask),
                    flags: FlagSet::EMPTY,
                });
            }
            _ => {
                self.unsupported = true;
            }
        }
    }

    fn lower_shift_with_flags(
        &mut self,
        dst: ValueId,
        op: BinOp,
        lhs: T1ValueId,
        rhs: T1ValueId,
        width: Width,
        mut flags: FlagSet,
    ) {
        debug_assert!(matches!(op, BinOp::Shl | BinOp::Shr | BinOp::Sar));

        // Tier-1 IR uses a constant rhs for shifts (the Tier-1 front-end only decodes the
        // immediate-count shift forms). If that ever changes, conservatively deopt.
        let Some(shift_imm) = self.const_values.get(&rhs).copied() else {
            self.unsupported = true;
            return;
        };

        // x86 shift counts are masked to 5 bits for 8/16/32-bit shifts and 6 bits for 64-bit shifts.
        let shift_mask: u32 = if width == Width::W64 { 63 } else { 31 };
        let shift_amt: u32 = (shift_imm as u32) & shift_mask;

        // x86 shifts do not update any flags when the shift count is 0.
        if shift_amt == 0 {
            flags = FlagSet::EMPTY;
        } else {
            // CF is undefined for counts > operand width; leave unchanged.
            if shift_amt > width.bits() {
                flags = flags.without(FlagSet::CF);
            }
            // OF is only defined for count == 1; leave unchanged otherwise.
            if shift_amt != 1 {
                flags = flags.without(FlagSet::OF);
            }
        }

        // Mask the input to the operand width (no-op for 64-bit).
        let lhs_masked = if width == Width::W64 {
            self.map_value(lhs)
        } else {
            let tmp = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: tmp,
                op: BinOp::And,
                lhs: self.value(lhs),
                rhs: Operand::Const(width.mask()),
                flags: FlagSet::EMPTY,
            });
            tmp
        };

        // Compute the shift result (masked to operand width) into `dst`.
        if op == BinOp::Sar && width != Width::W64 {
            // For SAR on narrow operands, sign-extend to 64 bits first.
            let mask = width.mask();
            let sext_shift = 64 - width.bits();

            let lhs_shl = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: lhs_shl,
                op: BinOp::Shl,
                lhs: Operand::Value(lhs_masked),
                rhs: Operand::Const(sext_shift as u64),
                flags: FlagSet::EMPTY,
            });
            let lhs_sext = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: lhs_sext,
                op: BinOp::Sar,
                lhs: Operand::Value(lhs_shl),
                rhs: Operand::Const(sext_shift as u64),
                flags: FlagSet::EMPTY,
            });

            let shifted = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: shifted,
                op: BinOp::Sar,
                lhs: Operand::Value(lhs_sext),
                rhs: Operand::Const(shift_amt as u64),
                flags: FlagSet::EMPTY,
            });
            self.instrs.push(Instr::BinOp {
                dst,
                op: BinOp::And,
                lhs: Operand::Value(shifted),
                rhs: Operand::Const(mask),
                flags: FlagSet::EMPTY,
            });
        } else if width == Width::W64 {
            self.instrs.push(Instr::BinOp {
                dst,
                op,
                lhs: Operand::Value(lhs_masked),
                rhs: Operand::Const(shift_amt as u64),
                flags: FlagSet::EMPTY,
            });
        } else {
            let mask = width.mask();
            let shifted = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: shifted,
                op,
                lhs: Operand::Value(lhs_masked),
                rhs: Operand::Const(shift_amt as u64),
                flags: FlagSet::EMPTY,
            });
            self.instrs.push(Instr::BinOp {
                dst,
                op: BinOp::And,
                lhs: Operand::Value(shifted),
                rhs: Operand::Const(mask),
                flags: FlagSet::EMPTY,
            });
        }

        // If x86 defines no flag updates (or the caller requested none), stop.
        if flags.is_empty() {
            return;
        }

        // ZF/SF/PF are computed from the result. For narrow operands, sign-extend to 64 bits so SF
        // reads the operand-width sign bit.
        let res_for_pzs = if width == Width::W64 {
            dst
        } else {
            let sext_shift = 64 - width.bits();
            let res_shl = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: res_shl,
                op: BinOp::Shl,
                lhs: Operand::Value(dst),
                rhs: Operand::Const(sext_shift as u64),
                flags: FlagSet::EMPTY,
            });
            let res_sext = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: res_sext,
                op: BinOp::Sar,
                lhs: Operand::Value(res_shl),
                rhs: Operand::Const(sext_shift as u64),
                flags: FlagSet::EMPTY,
            });
            res_sext
        };

        let mut pzs = FlagSet::EMPTY;
        if flags.contains(FlagSet::PF) {
            pzs = pzs.union(FlagSet::PF);
        }
        if flags.contains(FlagSet::ZF) {
            pzs = pzs.union(FlagSet::ZF);
        }
        if flags.contains(FlagSet::SF) {
            pzs = pzs.union(FlagSet::SF);
        }
        if !pzs.is_empty() {
            // Use AND as a "test result" op to compute PF/ZF/SF from the result without affecting
            // other flags.
            let tmp = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: tmp,
                op: BinOp::And,
                lhs: Operand::Value(res_for_pzs),
                rhs: Operand::Value(res_for_pzs),
                flags: pzs,
            });
        }

        // CF is defined for shift counts in the range [1, width.bits()]. For larger counts it is
        // undefined and we leave it unchanged (handled above by removing CF from `flags`).
        if flags.contains(FlagSet::CF) {
            let bit_shift: u32 = match op {
                BinOp::Shl => width.bits() - shift_amt,
                BinOp::Shr | BinOp::Sar => shift_amt - 1,
                _ => unreachable!(),
            };
            let tmp_shift = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: tmp_shift,
                op: BinOp::Shr,
                lhs: Operand::Value(lhs_masked),
                rhs: Operand::Const(bit_shift as u64),
                flags: FlagSet::EMPTY,
            });
            let cf_val = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: cf_val,
                op: BinOp::And,
                lhs: Operand::Value(tmp_shift),
                rhs: Operand::Const(1),
                flags: FlagSet::EMPTY,
            });
            // Update CF using a subtract that borrows iff cf_val != 0.
            let tmp_cf = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: tmp_cf,
                op: BinOp::Sub,
                lhs: Operand::Const(0),
                rhs: Operand::Value(cf_val),
                flags: FlagSet::CF,
            });
        }

        // OF is only defined for shift_amt == 1 (enforced above by masking flags).
        if flags.contains(FlagSet::OF) {
            debug_assert_eq!(shift_amt, 1);
            let sign_bit = 1u64 << (width.bits() - 1);

            // Compute of_val as 0/1.
            let of_val = match op {
                BinOp::Sar => Operand::Const(0),
                BinOp::Shr => {
                    let tmp_and = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: tmp_and,
                        op: BinOp::And,
                        lhs: Operand::Value(lhs_masked),
                        rhs: Operand::Const(sign_bit),
                        flags: FlagSet::EMPTY,
                    });
                    let tmp = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: tmp,
                        op: BinOp::Shr,
                        lhs: Operand::Value(tmp_and),
                        rhs: Operand::Const((width.bits() - 1) as u64),
                        flags: FlagSet::EMPTY,
                    });
                    Operand::Value(tmp)
                }
                BinOp::Shl => {
                    let tmp_xor = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: tmp_xor,
                        op: BinOp::Xor,
                        lhs: Operand::Value(lhs_masked),
                        rhs: Operand::Value(dst),
                        flags: FlagSet::EMPTY,
                    });
                    let tmp_and = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: tmp_and,
                        op: BinOp::And,
                        lhs: Operand::Value(tmp_xor),
                        rhs: Operand::Const(sign_bit),
                        flags: FlagSet::EMPTY,
                    });
                    let tmp = self.fresh_temp();
                    self.instrs.push(Instr::BinOp {
                        dst: tmp,
                        op: BinOp::Shr,
                        lhs: Operand::Value(tmp_and),
                        rhs: Operand::Const((width.bits() - 1) as u64),
                        flags: FlagSet::EMPTY,
                    });
                    Operand::Value(tmp)
                }
                _ => unreachable!(),
            };

            // Set OF to `of_val` by constructing an add that overflows iff of_val != 0:
            //   x = of_val << 63
            //   add x, x updates OF=1 when x is the sign bit.
            let x = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: x,
                op: BinOp::Shl,
                lhs: of_val,
                rhs: Operand::Const(63),
                flags: FlagSet::EMPTY,
            });
            let tmp_of = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: tmp_of,
                op: BinOp::Add,
                lhs: Operand::Value(x),
                rhs: Operand::Value(x),
                flags: FlagSet::OF,
            });
        }
    }

    fn lower_flag_op(
        &mut self,
        op: BinOp,
        lhs: T1ValueId,
        rhs: T1ValueId,
        width: Width,
        flags: FlagSet,
    ) {
        let flags = map_flagset(flags);
        if flags.is_empty() {
            return;
        }

        let dst = self.fresh_temp();

        if width == Width::W64 {
            self.instrs.push(Instr::BinOp {
                dst,
                op,
                lhs: self.value(lhs),
                rhs: self.value(rhs),
                flags,
            });
            return;
        }

        let shift = 64 - width.bits();
        let pf_af_mask = FlagSet::PF.union(FlagSet::AF);
        let flags_shift = flags.without(pf_af_mask);
        let flags_pf_af = flags.without(flags_shift);

        if !flags_shift.is_empty() {
            let lhs_s = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: lhs_s,
                op: BinOp::Shl,
                lhs: self.value(lhs),
                rhs: Operand::Const(shift as u64),
                flags: FlagSet::EMPTY,
            });
            let rhs_s = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: rhs_s,
                op: BinOp::Shl,
                lhs: self.value(rhs),
                rhs: Operand::Const(shift as u64),
                flags: FlagSet::EMPTY,
            });

            self.instrs.push(Instr::BinOp {
                dst,
                op,
                lhs: Operand::Value(lhs_s),
                rhs: Operand::Value(rhs_s),
                // PF/AF are computed from the low byte / nibble of the *truncated* result; the
                // shift-left trick breaks those computations. Update PF/AF in a second BinOp using
                // unshifted operands below.
                flags: flags_shift,
            });
        }

        if !flags_pf_af.is_empty() {
            let tmp = self.fresh_temp();
            self.instrs.push(Instr::BinOp {
                dst: tmp,
                op,
                lhs: self.value(lhs),
                rhs: self.value(rhs),
                flags: flags_pf_af,
            });
        }
    }

    fn lower_eval_cond(&mut self, dst: T1ValueId, cond: Cond) {
        let dst = self.map_value(dst);
        match cond {
            Cond::O => self.emit_load_flag(dst, Flag::Of),
            Cond::No => {
                let of = self.load_flag(Flag::Of);
                self.emit_not(dst, Operand::Value(of));
            }
            Cond::B => self.emit_load_flag(dst, Flag::Cf),
            Cond::Ae => {
                let cf = self.load_flag(Flag::Cf);
                self.emit_not(dst, Operand::Value(cf));
            }
            Cond::E => self.emit_load_flag(dst, Flag::Zf),
            Cond::Ne => {
                let zf = self.load_flag(Flag::Zf);
                self.emit_not(dst, Operand::Value(zf));
            }
            Cond::Be => {
                let cf = self.load_flag(Flag::Cf);
                let zf = self.load_flag(Flag::Zf);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Or,
                    lhs: Operand::Value(cf),
                    rhs: Operand::Value(zf),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::A => {
                let cf = self.load_flag(Flag::Cf);
                let zf = self.load_flag(Flag::Zf);
                let not_cf = self.fresh_temp();
                self.emit_not(not_cf, Operand::Value(cf));
                let not_zf = self.fresh_temp();
                self.emit_not(not_zf, Operand::Value(zf));
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(not_cf),
                    rhs: Operand::Value(not_zf),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::S => self.emit_load_flag(dst, Flag::Sf),
            Cond::Ns => {
                let sf = self.load_flag(Flag::Sf);
                self.emit_not(dst, Operand::Value(sf));
            }
            Cond::P => self.emit_load_flag(dst, Flag::Pf),
            Cond::Np => {
                let pf = self.load_flag(Flag::Pf);
                self.emit_not(dst, Operand::Value(pf));
            }
            Cond::L => {
                let sf = self.load_flag(Flag::Sf);
                let of = self.load_flag(Flag::Of);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Xor,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::Ge => {
                let sf = self.load_flag(Flag::Sf);
                let of = self.load_flag(Flag::Of);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Eq,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::Le => {
                let zf = self.load_flag(Flag::Zf);
                let sf = self.load_flag(Flag::Sf);
                let of = self.load_flag(Flag::Of);
                let sfo = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: sfo,
                    op: BinOp::Xor,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagSet::EMPTY,
                });
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Or,
                    lhs: Operand::Value(zf),
                    rhs: Operand::Value(sfo),
                    flags: FlagSet::EMPTY,
                });
            }
            Cond::G => {
                let zf = self.load_flag(Flag::Zf);
                let sf = self.load_flag(Flag::Sf);
                let of = self.load_flag(Flag::Of);
                let eq = self.fresh_temp();
                self.instrs.push(Instr::BinOp {
                    dst: eq,
                    op: BinOp::Eq,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagSet::EMPTY,
                });
                let not_zf = self.fresh_temp();
                self.emit_not(not_zf, Operand::Value(zf));
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(not_zf),
                    rhs: Operand::Value(eq),
                    flags: FlagSet::EMPTY,
                });
            }
        }
    }

    fn lower_select(
        &mut self,
        dst: T1ValueId,
        cond: T1ValueId,
        if_true: T1ValueId,
        if_false: T1ValueId,
        width: Width,
    ) {
        // Branchless select with booleanization:
        //   cond_is_zero = (cond == 0)
        //   cond_bool    = (cond_is_zero == 0)  // 1 if cond != 0
        //   dst          = if_true * cond_bool + if_false * cond_is_zero
        let dst = self.map_value(dst);

        let cond_is_zero = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: cond_is_zero,
            op: BinOp::Eq,
            lhs: self.value(cond),
            rhs: Operand::Const(0),
            flags: FlagSet::EMPTY,
        });

        let cond_bool = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: cond_bool,
            op: BinOp::Eq,
            lhs: Operand::Value(cond_is_zero),
            rhs: Operand::Const(0),
            flags: FlagSet::EMPTY,
        });

        let then_val = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: then_val,
            op: BinOp::Mul,
            lhs: self.value(if_true),
            rhs: Operand::Value(cond_bool),
            flags: FlagSet::EMPTY,
        });

        let else_val = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: else_val,
            op: BinOp::Mul,
            lhs: self.value(if_false),
            rhs: Operand::Value(cond_is_zero),
            flags: FlagSet::EMPTY,
        });

        if width == Width::W64 {
            self.instrs.push(Instr::BinOp {
                dst,
                op: BinOp::Add,
                lhs: Operand::Value(then_val),
                rhs: Operand::Value(else_val),
                flags: FlagSet::EMPTY,
            });
            return;
        }

        let sum = self.fresh_temp();
        self.instrs.push(Instr::BinOp {
            dst: sum,
            op: BinOp::Add,
            lhs: Operand::Value(then_val),
            rhs: Operand::Value(else_val),
            flags: FlagSet::EMPTY,
        });

        self.instrs.push(Instr::BinOp {
            dst,
            op: BinOp::And,
            lhs: Operand::Value(sum),
            rhs: Operand::Const(width.mask()),
            flags: FlagSet::EMPTY,
        });
    }

    fn load_flag(&mut self, flag: Flag) -> ValueId {
        let dst = self.fresh_temp();
        self.emit_load_flag(dst, flag);
        dst
    }

    fn emit_load_flag(&mut self, dst: ValueId, flag: Flag) {
        self.instrs.push(Instr::LoadFlag { dst, flag });
    }

    fn emit_not(&mut self, dst: ValueId, src: Operand) {
        // Canonicalize boolean NOT: `not(x) = (x == 0)`.
        self.instrs.push(Instr::BinOp {
            dst,
            op: BinOp::Eq,
            lhs: src,
            rhs: Operand::Const(0),
            flags: FlagSet::EMPTY,
        });
    }
}

fn map_flagset(flags: FlagSet) -> FlagSet {
    flags
}

fn map_binop(op: T1BinOp) -> Option<BinOp> {
    match op {
        T1BinOp::Add => Some(BinOp::Add),
        T1BinOp::Sub => Some(BinOp::Sub),
        T1BinOp::And => Some(BinOp::And),
        T1BinOp::Or => Some(BinOp::Or),
        T1BinOp::Xor => Some(BinOp::Xor),
        T1BinOp::Shl => Some(BinOp::Shl),
        T1BinOp::Shr => Some(BinOp::Shr),
        T1BinOp::Sar => Some(BinOp::Sar),
    }
}

/// Lowers a pre-built Tier-1 IR block into a single Tier-2 [`Block`].
///
/// This is a lightweight helper primarily intended for integration tests that want to exercise
/// Tier-2 lowering without going through x86 decode + Tier-1 translation.
///
/// Note: The returned block uses [`Terminator::Return`] on success and models "deopt at entry"
/// as an empty block that [`Terminator::SideExit`]s at `entry_rip` (matching the CFG builder's
/// conservative bailout behaviour).
#[doc(hidden)]
pub fn lower_tier1_ir_block_for_test(ir: &IrBlock) -> Block {
    let mut next_value: u32 = ir
        .value_types
        .len()
        .try_into()
        .expect("Tier-1 IR value count overflows u32");
    let base = 0u32;

    let mut lower = BlockLowerer::new(ir.entry_rip, base, &mut next_value);
    lower.lower_block(ir);

    if lower.unsupported {
        Block {
            id: BlockId(0),
            start_rip: ir.entry_rip,
            code_len: 0,
            instrs: Vec::new(),
            term: Terminator::SideExit {
                exit_rip: ir.entry_rip,
            },
        }
    } else {
        Block {
            id: BlockId(0),
            start_rip: ir.entry_rip,
            code_len: 0,
            instrs: lower.instrs,
            term: Terminator::Return,
        }
    }
}
