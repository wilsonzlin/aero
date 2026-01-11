use std::collections::{HashMap, VecDeque};

use aero_cpu::CpuBus;
use aero_types::{Cond, Flag, FlagSet, Gpr, Width};

use crate::tier1_ir::{BinOp as T1BinOp, GuestReg, IrBlock, IrInst, IrTerminator, ValueId as T1ValueId};
use crate::t2_ir::{
    BinOp, Block, BlockId, FlagMask, Function, Instr, Operand, Terminator, ValueId,
};
use crate::{discover_block, translate_block, BlockLimits};

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

#[derive(Clone, Debug)]
enum DraftTerminator {
    Jump(u64),
    Branch {
        cond: Operand,
        then_rip: u64,
        else_rip: u64,
    },
    Return,
}

#[derive(Clone, Debug)]
struct DraftBlock {
    id: BlockId,
    start_rip: u64,
    instrs: Vec<Instr>,
    term: DraftTerminator,
}

struct LowerCtx {
    instrs: Vec<Instr>,
    next_value: u32,
    entry_rip: u64,
    bailed: bool,
}

impl LowerCtx {
    fn new(entry_rip: u64, initial_value_count: u32) -> Self {
        Self {
            instrs: Vec::new(),
            next_value: initial_value_count,
            entry_rip,
            bailed: false,
        }
    }

    fn v(&self, id: T1ValueId) -> ValueId {
        ValueId(id.0)
    }

    fn op(&self, id: T1ValueId) -> Operand {
        Operand::Value(self.v(id))
    }

    fn fresh(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }

    fn bail_to_interp(&mut self, exit_rip: u64) {
        if self.bailed {
            return;
        }
        self.instrs.push(Instr::SideExit { exit_rip });
        self.bailed = true;
    }

    fn emit_not(&mut self, dst: ValueId, val: Operand) {
        // `val != 0` is represented as a 0/1 u64 in most of our IR. We still canonicalize in case
        // the input is not strictly boolean: `not(val) = (val == 0)`.
        self.instrs.push(Instr::BinOp {
            dst,
            op: BinOp::Eq,
            lhs: val,
            rhs: Operand::Const(0),
            flags: FlagMask::EMPTY,
        });
    }

    fn emit_eval_cond(&mut self, dst: ValueId, cond: Cond) -> Result<(), ()> {
        let load_flag = |this: &mut Self, flag: Flag| -> ValueId {
            let v = this.fresh();
            this.instrs.push(Instr::LoadFlag { dst: v, flag });
            v
        };

        match cond {
            Cond::O => {
                self.instrs
                    .push(Instr::LoadFlag { dst, flag: Flag::Of });
            }
            Cond::No => {
                let of = load_flag(self, Flag::Of);
                self.emit_not(dst, Operand::Value(of));
            }
            Cond::B => {
                self.instrs
                    .push(Instr::LoadFlag { dst, flag: Flag::Cf });
            }
            Cond::Ae => {
                let cf = load_flag(self, Flag::Cf);
                self.emit_not(dst, Operand::Value(cf));
            }
            Cond::E => {
                self.instrs
                    .push(Instr::LoadFlag { dst, flag: Flag::Zf });
            }
            Cond::Ne => {
                let zf = load_flag(self, Flag::Zf);
                self.emit_not(dst, Operand::Value(zf));
            }
            Cond::Be => {
                let cf = load_flag(self, Flag::Cf);
                let zf = load_flag(self, Flag::Zf);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Or,
                    lhs: Operand::Value(cf),
                    rhs: Operand::Value(zf),
                    flags: FlagMask::EMPTY,
                });
            }
            Cond::A => {
                let cf = load_flag(self, Flag::Cf);
                let zf = load_flag(self, Flag::Zf);
                let not_cf = self.fresh();
                self.emit_not(not_cf, Operand::Value(cf));
                let not_zf = self.fresh();
                self.emit_not(not_zf, Operand::Value(zf));
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(not_cf),
                    rhs: Operand::Value(not_zf),
                    flags: FlagMask::EMPTY,
                });
            }
            Cond::S => {
                self.instrs
                    .push(Instr::LoadFlag { dst, flag: Flag::Sf });
            }
            Cond::Ns => {
                let sf = load_flag(self, Flag::Sf);
                self.emit_not(dst, Operand::Value(sf));
            }
            Cond::P | Cond::Np => {
                // Parity flag is not tracked in Tier-2 yet.
                return Err(());
            }
            Cond::L => {
                let sf = load_flag(self, Flag::Sf);
                let of = load_flag(self, Flag::Of);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Xor,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagMask::EMPTY,
                });
            }
            Cond::Ge => {
                let sf = load_flag(self, Flag::Sf);
                let of = load_flag(self, Flag::Of);
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Eq,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagMask::EMPTY,
                });
            }
            Cond::Le => {
                let zf = load_flag(self, Flag::Zf);
                let sf = load_flag(self, Flag::Sf);
                let of = load_flag(self, Flag::Of);
                let sf_xor_of = self.fresh();
                self.instrs.push(Instr::BinOp {
                    dst: sf_xor_of,
                    op: BinOp::Xor,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagMask::EMPTY,
                });
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::Or,
                    lhs: Operand::Value(zf),
                    rhs: Operand::Value(sf_xor_of),
                    flags: FlagMask::EMPTY,
                });
            }
            Cond::G => {
                let zf = load_flag(self, Flag::Zf);
                let sf = load_flag(self, Flag::Sf);
                let of = load_flag(self, Flag::Of);
                let zf_is_zero = self.fresh();
                self.emit_not(zf_is_zero, Operand::Value(zf));
                let sf_eq_of = self.fresh();
                self.instrs.push(Instr::BinOp {
                    dst: sf_eq_of,
                    op: BinOp::Eq,
                    lhs: Operand::Value(sf),
                    rhs: Operand::Value(of),
                    flags: FlagMask::EMPTY,
                });
                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(zf_is_zero),
                    rhs: Operand::Value(sf_eq_of),
                    flags: FlagMask::EMPTY,
                });
            }
        }

        Ok(())
    }

    fn lower_read_reg(&mut self, dst: ValueId, reg: GuestReg) {
        match reg {
            GuestReg::Rip => {
                // Tier-2 blocks don't model RIP as a register. For now treat RIP reads as the
                // block entry RIP (this is sufficient for unit tests and rip-relative address
                // computations are already materialized as constants by Tier-1 lowering).
                self.instrs.push(Instr::Const {
                    dst,
                    value: self.entry_rip,
                });
            }
            GuestReg::Gpr { reg, width, high8 } => {
                let reg = map_gpr(reg);
                if width == Width::W64 && !high8 {
                    self.instrs.push(Instr::LoadReg { dst, reg });
                    return;
                }

                // Load full 64-bit register then extract the requested subrange.
                let full = self.fresh();
                self.instrs.push(Instr::LoadReg { dst: full, reg });

                let mask = match width {
                    Width::W8 => 0xff,
                    Width::W16 => 0xffff,
                    Width::W32 => 0xffff_ffff,
                    Width::W64 => u64::MAX,
                };

                if high8 {
                    // Extract AH/CH/DH/BH: (reg >> 8) & 0xff
                    let shifted = self.fresh();
                    self.instrs.push(Instr::BinOp {
                        dst: shifted,
                        op: BinOp::Shr,
                        lhs: Operand::Value(full),
                        rhs: Operand::Const(8),
                        flags: FlagMask::EMPTY,
                    });
                    self.instrs.push(Instr::BinOp {
                        dst,
                        op: BinOp::And,
                        lhs: Operand::Value(shifted),
                        rhs: Operand::Const(mask),
                        flags: FlagMask::EMPTY,
                    });
                    return;
                }

                self.instrs.push(Instr::BinOp {
                    dst,
                    op: BinOp::And,
                    lhs: Operand::Value(full),
                    rhs: Operand::Const(mask),
                    flags: FlagMask::EMPTY,
                });
            }
            GuestReg::Flag(flag) => {
                if let Some(flag) = map_flag(flag) {
                    self.instrs.push(Instr::LoadFlag { dst, flag });
                } else {
                    self.bail_to_interp(self.entry_rip);
                }
            }
        }
    }

    fn lower_write_reg(&mut self, reg: GuestReg, src: ValueId) {
        match reg {
            GuestReg::Gpr { reg, width, high8 } => {
                let reg = map_gpr(reg);
                match width {
                    Width::W64 if !high8 => {
                        self.instrs
                            .push(Instr::StoreReg { reg, src: Operand::Value(src) });
                    }
                    Width::W32 if !high8 => {
                        // x86-64: 32-bit writes zero-extend into 64-bit.
                        let masked = self.fresh();
                        self.instrs.push(Instr::BinOp {
                            dst: masked,
                            op: BinOp::And,
                            lhs: Operand::Value(src),
                            rhs: Operand::Const(0xffff_ffff),
                            flags: FlagMask::EMPTY,
                        });
                        self.instrs.push(Instr::StoreReg {
                            reg,
                            src: Operand::Value(masked),
                        });
                    }
                    Width::W16 | Width::W8 => {
                        let (mask, shift) = match (width, high8) {
                            (Width::W16, _) => (0xffffu64, 0u32),
                            (Width::W8, true) => (0xffu64, 8u32),
                            (Width::W8, false) => (0xffu64, 0u32),
                            _ => unreachable!(),
                        };

                        let old = self.fresh();
                        self.instrs.push(Instr::LoadReg { dst: old, reg });

                        let clear_mask = !(mask << shift);
                        let cleared = self.fresh();
                        self.instrs.push(Instr::BinOp {
                            dst: cleared,
                            op: BinOp::And,
                            lhs: Operand::Value(old),
                            rhs: Operand::Const(clear_mask),
                            flags: FlagMask::EMPTY,
                        });

                        let src_masked = self.fresh();
                        self.instrs.push(Instr::BinOp {
                            dst: src_masked,
                            op: BinOp::And,
                            lhs: Operand::Value(src),
                            rhs: Operand::Const(mask),
                            flags: FlagMask::EMPTY,
                        });

                        let inserted = if shift == 0 {
                            src_masked
                        } else {
                            let shifted = self.fresh();
                            self.instrs.push(Instr::BinOp {
                                dst: shifted,
                                op: BinOp::Shl,
                                lhs: Operand::Value(src_masked),
                                rhs: Operand::Const(shift as u64),
                                flags: FlagMask::EMPTY,
                            });
                            shifted
                        };

                        let merged = self.fresh();
                        self.instrs.push(Instr::BinOp {
                            dst: merged,
                            op: BinOp::Or,
                            lhs: Operand::Value(cleared),
                            rhs: Operand::Value(inserted),
                            flags: FlagMask::EMPTY,
                        });
                        self.instrs.push(Instr::StoreReg {
                            reg,
                            src: Operand::Value(merged),
                        });
                    }
                    _ => {
                        self.bail_to_interp(self.entry_rip);
                    }
                }
            }
            GuestReg::Rip | GuestReg::Flag(_) => {
                // Tier-2 doesn't model RIP or direct flag writes yet. Conservatively deopt.
                self.bail_to_interp(self.entry_rip);
            }
        }
    }
}

fn map_gpr(reg: Gpr) -> Gpr {
    reg
}

fn map_flag(flag: Flag) -> Option<Flag> {
    match flag {
        Flag::Cf | Flag::Zf | Flag::Sf | Flag::Of => Some(flag),
        Flag::Pf | Flag::Af => None,
    }
}

fn map_flag_set(flags: FlagSet) -> FlagMask {
    let mut mask = FlagMask::EMPTY;
    for flag in flags.iter() {
        match flag {
            aero_types::Flag::Cf => mask.insert(FlagMask::CF),
            aero_types::Flag::Zf => mask.insert(FlagMask::ZF),
            aero_types::Flag::Sf => mask.insert(FlagMask::SF),
            aero_types::Flag::Of => mask.insert(FlagMask::OF),
            aero_types::Flag::Pf | aero_types::Flag::Af => {}
        }
    }
    mask
}

fn map_binop(op: T1BinOp) -> Option<BinOp> {
    Some(match op {
        T1BinOp::Add => BinOp::Add,
        T1BinOp::Sub => BinOp::Sub,
        T1BinOp::And => BinOp::And,
        T1BinOp::Or => BinOp::Or,
        T1BinOp::Xor => BinOp::Xor,
        T1BinOp::Shl => BinOp::Shl,
        T1BinOp::Shr => BinOp::Shr,
        T1BinOp::Sar => return None,
    })
}

fn lower_block(ir: &IrBlock) -> DraftBlock {
    let id = BlockId(0); // overwritten by caller
    let mut ctx = LowerCtx::new(ir.entry_rip, ir.value_types.len() as u32);

    for inst in &ir.insts {
        if ctx.bailed {
            break;
        }

        match inst {
            IrInst::Const { dst, value, .. } => ctx.instrs.push(Instr::Const {
                dst: ctx.v(*dst),
                value: *value,
            }),
            IrInst::ReadReg { dst, reg } => ctx.lower_read_reg(ctx.v(*dst), *reg),
            IrInst::WriteReg { reg, src } => ctx.lower_write_reg(*reg, ctx.v(*src)),
            IrInst::Trunc { dst, src, width } => {
                let mask = width.mask();
                ctx.instrs.push(Instr::BinOp {
                    dst: ctx.v(*dst),
                    op: BinOp::And,
                    lhs: ctx.op(*src),
                    rhs: Operand::Const(mask),
                    flags: FlagMask::EMPTY,
                });
            }
            IrInst::Load { .. } | IrInst::Store { .. } => {
                // Tier-2 doesn't model memory yet. Bail out conservatively.
                ctx.bail_to_interp(ir.entry_rip);
            }
            IrInst::BinOp {
                dst,
                op,
                lhs,
                rhs,
                flags,
                ..
            } => {
                let Some(op) = map_binop(*op) else {
                    ctx.bail_to_interp(ir.entry_rip);
                    continue;
                };
                ctx.instrs.push(Instr::BinOp {
                    dst: ctx.v(*dst),
                    op,
                    lhs: ctx.op(*lhs),
                    rhs: ctx.op(*rhs),
                    flags: map_flag_set(*flags),
                });
            }
            IrInst::CmpFlags { lhs, rhs, flags, .. } => {
                let tmp = ctx.fresh();
                ctx.instrs.push(Instr::BinOp {
                    dst: tmp,
                    op: BinOp::Sub,
                    lhs: ctx.op(*lhs),
                    rhs: ctx.op(*rhs),
                    flags: map_flag_set(*flags),
                });
            }
            IrInst::TestFlags { lhs, rhs, flags, .. } => {
                let tmp = ctx.fresh();
                ctx.instrs.push(Instr::BinOp {
                    dst: tmp,
                    op: BinOp::And,
                    lhs: ctx.op(*lhs),
                    rhs: ctx.op(*rhs),
                    flags: map_flag_set(*flags),
                });
            }
            IrInst::EvalCond { dst, cond } => {
                if ctx.emit_eval_cond(ctx.v(*dst), *cond).is_err() {
                    ctx.bail_to_interp(ir.entry_rip);
                }
            }
            IrInst::Select {
                dst,
                cond,
                if_true,
                if_false,
                ..
            } => {
                // Branchless select:
                //   cond_is_zero = (cond == 0)
                //   cond_bool    = (cond_is_zero == 0)  // 1 if cond != 0
                //   dst          = if_true * cond_bool + if_false * cond_is_zero
                let cond_is_zero = ctx.fresh();
                ctx.instrs.push(Instr::BinOp {
                    dst: cond_is_zero,
                    op: BinOp::Eq,
                    lhs: ctx.op(*cond),
                    rhs: Operand::Const(0),
                    flags: FlagMask::EMPTY,
                });

                let cond_bool = ctx.fresh();
                ctx.instrs.push(Instr::BinOp {
                    dst: cond_bool,
                    op: BinOp::Eq,
                    lhs: Operand::Value(cond_is_zero),
                    rhs: Operand::Const(0),
                    flags: FlagMask::EMPTY,
                });

                let then_val = ctx.fresh();
                ctx.instrs.push(Instr::BinOp {
                    dst: then_val,
                    op: BinOp::Mul,
                    lhs: ctx.op(*if_true),
                    rhs: Operand::Value(cond_bool),
                    flags: FlagMask::EMPTY,
                });

                let else_val = ctx.fresh();
                ctx.instrs.push(Instr::BinOp {
                    dst: else_val,
                    op: BinOp::Mul,
                    lhs: ctx.op(*if_false),
                    rhs: Operand::Value(cond_is_zero),
                    flags: FlagMask::EMPTY,
                });

                ctx.instrs.push(Instr::BinOp {
                    dst: ctx.v(*dst),
                    op: BinOp::Add,
                    lhs: Operand::Value(then_val),
                    rhs: Operand::Value(else_val),
                    flags: FlagMask::EMPTY,
                });
            }
            IrInst::CallHelper { .. } => {
                // Helpers are a Tier-1 escape hatch. Tier-2 blocks should conservatively deopt
                // until we have a way to represent helper calls.
                ctx.bail_to_interp(ir.entry_rip);
            }
        }
    }

    let term = if ctx.bailed {
        DraftTerminator::Return
    } else {
        match ir.terminator {
            IrTerminator::Jump { target } => DraftTerminator::Jump(target),
            IrTerminator::CondJump {
                cond,
                target,
                fallthrough,
            } => DraftTerminator::Branch {
                cond: Operand::Value(ctx.v(cond)),
                then_rip: target,
                else_rip: fallthrough,
            },
            IrTerminator::IndirectJump { .. } => {
                // Dynamic targets can't be represented in the Tier-2 CFG yet. Deopt to interpreter
                // at the start of this block (which will re-execute the original x86 branch).
                ctx.instrs.push(Instr::SideExit {
                    exit_rip: ir.entry_rip,
                });
                DraftTerminator::Return
            }
            IrTerminator::ExitToInterpreter { next_rip } => {
                ctx.instrs.push(Instr::SideExit { exit_rip: next_rip });
                DraftTerminator::Return
            }
        }
    };

    DraftBlock {
        id,
        start_rip: ir.entry_rip,
        instrs: ctx.instrs,
        term,
    }
}

/// Build a Tier-2 CFG by discovering x86 basic blocks starting at `entry_rip`,
/// translating them through Tier-1 IR, and lowering into [`crate::t2_ir::Function`].
pub fn build_function_from_x86<B: CpuBus>(bus: &B, entry_rip: u64, cfg: CfgBuildConfig) -> Function {
    let mut rip_to_id: HashMap<u64, BlockId> = HashMap::new();
    let mut drafts: Vec<DraftBlock> = Vec::new();
    let mut worklist: VecDeque<u64> = VecDeque::new();
    worklist.push_back(entry_rip);

    while let Some(rip) = worklist.pop_front() {
        if rip_to_id.contains_key(&rip) {
            continue;
        }
        if drafts.len() >= cfg.max_blocks {
            break;
        }

        let id = BlockId(drafts.len() as u32);
        rip_to_id.insert(rip, id);

        let bb = discover_block(bus, rip, cfg.block_limits);
        let ir = translate_block(&bb);
        let mut draft = lower_block(&ir);
        draft.id = id;

        match &draft.term {
            DraftTerminator::Jump(target) => worklist.push_back(*target),
            DraftTerminator::Branch {
                then_rip,
                else_rip,
                ..
            } => {
                worklist.push_back(*then_rip);
                worklist.push_back(*else_rip);
            }
            DraftTerminator::Return => {}
        }

        drafts.push(draft);
    }

    let resolve = |rip: u64| rip_to_id.get(&rip).copied();

    let blocks: Vec<Block> = drafts
        .into_iter()
        .map(|draft| {
            let term = match draft.term {
                DraftTerminator::Jump(target) => match resolve(target) {
                    Some(id) => Terminator::Jump(id),
                    None => Terminator::Return,
                },
                DraftTerminator::Branch {
                    cond,
                    then_rip,
                    else_rip,
                } => match (resolve(then_rip), resolve(else_rip)) {
                    (Some(then_bb), Some(else_bb)) => Terminator::Branch {
                        cond,
                        then_bb,
                        else_bb,
                    },
                    _ => Terminator::Return,
                },
                DraftTerminator::Return => Terminator::Return,
            };

            Block {
                id: draft.id,
                start_rip: draft.start_rip,
                instrs: draft.instrs,
                term,
            }
        })
        .collect();

    let entry = rip_to_id
        .get(&entry_rip)
        .copied()
        .unwrap_or(BlockId(0));

    Function { blocks, entry }
}
