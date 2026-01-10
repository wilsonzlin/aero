//! Tier-2 region selection + optimization pipeline + "codegen".
//!
//! The real Aero implementation targets WASM; this crate runs the compiled
//! output directly, but keeps the same architectural shape:
//! - select a hot trace/loop region using profile data
//! - lower to an optimization IR
//! - run passes
//! - lower to a compact executable representation (with a WASM SIMD listing)

use crate::microvm::{
    simd_f32x4_add, simd_f32x4_mul, set_flags_add, set_flags_logic, set_flags_sub, BlockId, Cond,
    FlagMask, FuncId, Function, Instr, Program, Terminator, Vm,
};
use crate::profile::FuncProfile;
use crate::tier::JitConfig;
use std::collections::HashMap;

/// A Tier-2 "compiled region".
///
/// The region is a single-entry trace (potentially with a backedge for a loop).
/// All conditional branches in the region are compiled as *guards*; the cold
/// path deoptimizes back to the interpreter.
#[derive(Clone)]
pub(crate) struct CompiledRegion {
    pub entry: BlockId,
    blocks: Vec<CompiledBlock>,
    block_index: HashMap<BlockId, usize>,
    preamble: Vec<CompiledOp>,
    reg_alloc: RegAlloc,
    guard_perm_epoch: u64,
    guard_code_epoch: u64,
    #[allow(dead_code)]
    wasm_simd_listing: Vec<WasmInst>,
}

#[derive(Clone)]
struct CompiledBlock {
    orig: BlockId,
    ops: Vec<CompiledOp>,
    term: CompiledTerm,
}

#[derive(Clone)]
enum CompiledTerm {
    Jump(BlockId),
    Guard { cond: Cond, expected: bool, hot: BlockId, cold: BlockId },
    Ret { src: Slot },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Slot(u16);

#[derive(Clone)]
enum CompiledOp {
    Imm { dst: Slot, imm: u64 },
    Mov { dst: Slot, src: Slot },
    Add { dst: Slot, a: Slot, b: Slot, flags: FlagMask },
    AddImm { dst: Slot, src: Slot, imm: i32, flags: FlagMask },
    Sub { dst: Slot, a: Slot, b: Slot, flags: FlagMask },
    Mul { dst: Slot, a: Slot, b: Slot, flags: FlagMask },
    Shl { dst: Slot, src: Slot, shift: u8, flags: FlagMask },
    Cmp { a: Slot, b: Slot, flags: FlagMask },
    SetFlagsConst { value: u8, mask: FlagMask },

    LoadU64 { dst: Slot, addr: Slot },
    StoreU64 { addr: Slot, value: Slot },

    VImm { dst: Slot, imm: u128 },
    VAddF32x4 { dst: Slot, a: Slot, b: Slot },
    VMulF32x4 { dst: Slot, a: Slot, b: Slot },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecOutcome {
    Continue(BlockId),
    Deopt(BlockId),
    Return(u64),
}

impl CompiledRegion {
    pub(crate) fn execute(&self, vm: &mut Vm) -> ExecOutcome {
        if vm.mem.perm_epoch() != self.guard_perm_epoch || vm.mem.code_epoch() != self.guard_code_epoch
        {
            return ExecOutcome::Deopt(self.entry);
        }

        // Locals/spills are stored in a single linear slot array. `RegAlloc`
        // decides which regs map into the first `*_local_slots` entries (WASM
        // locals) vs the remainder (spills/stack slots).
        let mut gpr_slots = vec![0u64; self.reg_alloc.gpr_slot_count as usize];
        let mut xmm_slots = vec![0u128; self.reg_alloc.xmm_slot_count as usize];
        let mut flags = vm.flags;

        // Load guest regs into slots; temps start as 0.
        for gpr in 0..self.reg_alloc.guest_gpr_count {
            let slot = self.reg_alloc.gpr_slots[gpr as usize];
            gpr_slots[slot.0 as usize] = vm.gprs[gpr as usize];
        }
        for xmm in 0..self.reg_alloc.guest_xmm_count {
            let slot = self.reg_alloc.xmm_slots[xmm as usize];
            xmm_slots[slot.0 as usize] = vm.xmms[xmm as usize];
        }

        // Execute preamble (LICM hoisted code).
        for op in &self.preamble {
            exec_op(op, &mut gpr_slots, &mut xmm_slots, &mut flags, &mut vm.mem);
        }

        let mut cur = self.entry;
        loop {
            let block_idx = match self.block_index.get(&cur) {
                Some(idx) => *idx,
                None => {
                    flush_guest_state(&self.reg_alloc, &gpr_slots, &xmm_slots, flags, vm);
                    return ExecOutcome::Continue(cur);
                }
            };
            let blk = &self.blocks[block_idx];

            for op in &blk.ops {
                exec_op(op, &mut gpr_slots, &mut xmm_slots, &mut flags, &mut vm.mem);
            }

            match &blk.term {
                CompiledTerm::Jump(tgt) => cur = *tgt,
                CompiledTerm::Guard { cond, expected, hot, cold } => {
                    if cond.eval(flags) == *expected {
                        cur = *hot;
                    } else {
                        flush_guest_state(&self.reg_alloc, &gpr_slots, &xmm_slots, flags, vm);
                        return ExecOutcome::Continue(*cold);
                    }
                }
                CompiledTerm::Ret { src } => {
                    let ret = gpr_slots[src.0 as usize];
                    flush_guest_state(&self.reg_alloc, &gpr_slots, &xmm_slots, flags, vm);
                    return ExecOutcome::Return(ret);
                }
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn wasm_simd_listing(&self) -> &[WasmInst] {
        &self.wasm_simd_listing
    }
}

fn flush_guest_state(alloc: &RegAlloc, gpr_slots: &[u64], xmm_slots: &[u128], flags: u8, vm: &mut Vm) {
    for gpr in 0..alloc.guest_gpr_count {
        let slot = alloc.gpr_slots[gpr as usize];
        vm.gprs[gpr as usize] = gpr_slots[slot.0 as usize];
    }
    for xmm in 0..alloc.guest_xmm_count {
        let slot = alloc.xmm_slots[xmm as usize];
        vm.xmms[xmm as usize] = xmm_slots[slot.0 as usize];
    }
    vm.flags = flags;
}

fn exec_op(op: &CompiledOp, gprs: &mut [u64], xmms: &mut [u128], flags: &mut u8, mem: &mut crate::microvm::Memory) {
    match *op {
        CompiledOp::Imm { dst, imm } => gprs[dst.0 as usize] = imm,
        CompiledOp::Mov { dst, src } => gprs[dst.0 as usize] = gprs[src.0 as usize],
        CompiledOp::Add { dst, a, b, flags: f } => {
            let aa = gprs[a.0 as usize];
            let bb = gprs[b.0 as usize];
            let rr = aa.wrapping_add(bb);
            gprs[dst.0 as usize] = rr;
            set_flags_add(rr, aa, bb, f, flags);
        }
        CompiledOp::AddImm { dst, src, imm, flags: f } => {
            let aa = gprs[src.0 as usize];
            let bb = imm as i64 as u64;
            let rr = aa.wrapping_add(bb);
            gprs[dst.0 as usize] = rr;
            set_flags_add(rr, aa, bb, f, flags);
        }
        CompiledOp::Sub { dst, a, b, flags: f } => {
            let aa = gprs[a.0 as usize];
            let bb = gprs[b.0 as usize];
            let rr = aa.wrapping_sub(bb);
            gprs[dst.0 as usize] = rr;
            set_flags_sub(rr, aa, bb, f, flags);
        }
        CompiledOp::Mul { dst, a, b, flags: f } => {
            let aa = gprs[a.0 as usize];
            let bb = gprs[b.0 as usize];
            let rr = aa.wrapping_mul(bb);
            gprs[dst.0 as usize] = rr;
            // We model MUL as setting ZF/SF based on result, and clearing CF/OF.
            set_flags_logic(rr, f, flags);
        }
        CompiledOp::Shl { dst, src, shift, flags: f } => {
            let aa = gprs[src.0 as usize];
            let rr = aa.wrapping_shl(shift as u32);
            gprs[dst.0 as usize] = rr;
            set_flags_logic(rr, f, flags);
        }
        CompiledOp::Cmp { a, b, flags: f } => {
            let aa = gprs[a.0 as usize];
            let bb = gprs[b.0 as usize];
            let rr = aa.wrapping_sub(bb);
            set_flags_sub(rr, aa, bb, f, flags);
        }
        CompiledOp::SetFlagsConst { value, mask } => {
            if mask.is_empty() {
                return;
            }
            let keep = !mask.bits();
            *flags = (*flags & keep) | (value & mask.bits());
        }
        CompiledOp::LoadU64 { dst, addr } => {
            let addr = gprs[addr.0 as usize];
            let addr_usize = addr as usize;
            if addr_usize + 8 > mem.len() {
                // A real implementation would deopt or trap; keep the toy strict.
                panic!("OOB load_u64 at {addr:#x}");
            }
            unsafe {
                let ptr = mem.as_ptr().add(addr_usize) as *const u64;
                gprs[dst.0 as usize] = u64::from_le(ptr.read_unaligned());
            }
        }
        CompiledOp::StoreU64 { addr, value } => {
            let addr = gprs[addr.0 as usize];
            let val = gprs[value.0 as usize];
            let addr_usize = addr as usize;
            if addr_usize + 8 > mem.len() {
                panic!("OOB store_u64 at {addr:#x}");
            }
            mem.store_u64(addr, val);
        }
        CompiledOp::VImm { dst, imm } => xmms[dst.0 as usize] = imm,
        CompiledOp::VAddF32x4 { dst, a, b } => {
            let aa = xmms[a.0 as usize];
            let bb = xmms[b.0 as usize];
            xmms[dst.0 as usize] = simd_f32x4_add(aa, bb);
        }
        CompiledOp::VMulF32x4 { dst, a, b } => {
            let aa = xmms[a.0 as usize];
            let bb = xmms[b.0 as usize];
            xmms[dst.0 as usize] = simd_f32x4_mul(aa, bb);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WasmInst {
    F32x4Add,
    F32x4Mul,
}

#[derive(Clone)]
struct RegAlloc {
    guest_gpr_count: u16,
    guest_xmm_count: u16,
    gpr_slots: Vec<Slot>, // per gpr reg id -> slot
    xmm_slots: Vec<Slot>, // per xmm reg id -> slot
    gpr_slot_count: u16,
    xmm_slot_count: u16,
    _gpr_local_slots: u16,
    _xmm_local_slots: u16,
}

/// Build + optimize + compile a region starting at `entry`.
pub(crate) fn compile_region(
    program: &Program,
    func: FuncId,
    entry: BlockId,
    profile: &FuncProfile,
    config: &JitConfig,
    vm: &Vm,
) -> Option<CompiledRegion> {
    let func_ref = &program.functions[func];
    let selection = select_region(func_ref, entry, profile, config)?;
    let mut region = lower_to_opt_ir(func_ref, &selection);

    // Pass pipeline. Ordering isn't sacred; it's chosen to let flag liveness
    // enable CSE/const-folding on arithmetic that no longer needs flags.
    compute_flag_liveness(&mut region);
    strength_reduction(&mut region);
    constant_folding(&mut region);
    common_subexpression_elimination(&mut region);
    dead_code_elimination(&mut region, func_ref);
    loop_invariant_code_motion(&mut region);
    dead_code_elimination(&mut region, func_ref);

    Some(codegen(region, func_ref, config, vm))
}

// -----------------------------------------------------------------------------
// Region selection
// -----------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct SelectedBlock {
    orig: BlockId,
    hot_succ: Option<BlockId>,
    term: Terminator,
}

fn select_region(
    func: &Function,
    entry: BlockId,
    profile: &FuncProfile,
    config: &JitConfig,
) -> Option<Vec<SelectedBlock>> {
    if entry >= func.blocks.len() {
        return None;
    }

    let mut out = Vec::new();
    let mut seen = HashMap::<BlockId, usize>::new();
    let mut cur = entry;

    while out.len() < config.max_region_blocks {
        if let Some(_idx) = seen.get(&cur) {
            // Backedge to earlier block: close the loop by including the current
            // block once and leaving the terminator intact (will jump back).
            break;
        }
        seen.insert(cur, out.len());

        let blk = &func.blocks[cur];
        if blk.instrs.iter().any(|i| matches!(i, Instr::Call { .. })) {
            // Calls force an exit from the region. (A real implementation would
            // model call inlining, but this prototype keeps Tier-2 regions
            // single-function.)
            break;
        }
        let mut hot_succ = None;
        let term = blk.term.clone();
        match &blk.term {
            Terminator::Jmp(tgt) => {
                hot_succ = Some(*tgt);
            }
            Terminator::Br { cond, then_tgt, else_tgt } => {
                let bp = &profile.blocks[cur];
                let (hot, _cold) = match &bp.branch {
                    Some((_, prof)) => prof.hot_successor(*then_tgt, *else_tgt),
                    None => (*then_tgt, *else_tgt),
                };
                hot_succ = Some(hot);
                let _ = cond;
            }
            Terminator::Ret { .. } => {}
        }

        out.push(SelectedBlock { orig: cur, hot_succ, term });

        match &blk.term {
            Terminator::Jmp(tgt) => cur = *tgt,
            Terminator::Br { .. } => cur = hot_succ.unwrap(),
            Terminator::Ret { .. } => break,
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

// -----------------------------------------------------------------------------
// Optimization IR
// -----------------------------------------------------------------------------

type GprId = u16;
type XmmId = u16;

#[derive(Clone, Debug)]
struct OptRegion {
    entry: BlockId,
    preamble: Vec<OptInst>,
    blocks: Vec<OptBlock>,
    // Number of guest (architectural) regs; temps use ids >= these counts.
    guest_gpr_count: u16,
    gpr_count: u16,
    xmm_count: u16,
}

#[derive(Clone, Debug)]
struct OptBlock {
    orig: BlockId,
    insts: Vec<OptInst>,
    term: OptTerm,
}

#[derive(Clone, Debug)]
enum OptTerm {
    Jump(BlockId),
    Guard { cond: Cond, expected: bool, hot: BlockId, cold: BlockId },
    Ret { src: GprId },
}

#[derive(Clone, Debug)]
enum OptInst {
    Imm { dst: GprId, imm: u64 },
    Mov { dst: GprId, src: GprId },
    Add { dst: GprId, a: GprId, b: GprId, flags: FlagMask },
    Sub { dst: GprId, a: GprId, b: GprId, flags: FlagMask },
    Mul { dst: GprId, a: GprId, b: GprId, flags: FlagMask },
    Shl { dst: GprId, src: GprId, shift: u8, flags: FlagMask },
    Cmp { a: GprId, b: GprId, flags: FlagMask },
    SetFlagsConst { value: u8, mask: FlagMask },

    AddImm { dst: GprId, src: GprId, imm: i32, flags: FlagMask },

    LoadU64 { dst: GprId, addr: GprId },
    StoreU64 { addr: GprId, value: GprId },

    VImm { dst: XmmId, imm: u128 },
    VAddF32x4 { dst: XmmId, a: XmmId, b: XmmId },
    VMulF32x4 { dst: XmmId, a: XmmId, b: XmmId },
}

impl OptInst {
    fn has_side_effects(&self) -> bool {
        matches!(self, OptInst::StoreU64 { .. })
    }

    fn defs_gpr(&self) -> Option<GprId> {
        match *self {
            OptInst::Imm { dst, .. }
            | OptInst::Mov { dst, .. }
            | OptInst::Add { dst, .. }
            | OptInst::Sub { dst, .. }
            | OptInst::Mul { dst, .. }
            | OptInst::Shl { dst, .. }
            | OptInst::AddImm { dst, .. }
            | OptInst::LoadU64 { dst, .. } => Some(dst),
            OptInst::Cmp { .. } | OptInst::StoreU64 { .. } | OptInst::SetFlagsConst { .. } => None,
            OptInst::VImm { .. } | OptInst::VAddF32x4 { .. } | OptInst::VMulF32x4 { .. } => None,
        }
    }

    fn defs_xmm(&self) -> Option<XmmId> {
        match *self {
            OptInst::VImm { dst, .. }
            | OptInst::VAddF32x4 { dst, .. }
            | OptInst::VMulF32x4 { dst, .. } => Some(dst),
            _ => None,
        }
    }

    fn uses_gprs(&self, out: &mut Vec<GprId>) {
        match *self {
            OptInst::Imm { .. } => {}
            OptInst::Mov { src, .. } => out.push(src),
            OptInst::Add { a, b, .. } | OptInst::Sub { a, b, .. } | OptInst::Mul { a, b, .. } => {
                out.push(a);
                out.push(b);
            }
            OptInst::Shl { src, .. } => out.push(src),
            OptInst::Cmp { a, b, .. } => {
                out.push(a);
                out.push(b);
            }
            OptInst::AddImm { src, .. } => out.push(src),
            OptInst::LoadU64 { addr, .. } => out.push(addr),
            OptInst::StoreU64 { addr, value } => {
                out.push(addr);
                out.push(value);
            }
            OptInst::SetFlagsConst { .. } => {}
            OptInst::VImm { .. } | OptInst::VAddF32x4 { .. } | OptInst::VMulF32x4 { .. } => {}
        }
    }

    fn uses_xmms(&self, out: &mut Vec<XmmId>) {
        match *self {
            OptInst::VAddF32x4 { a, b, .. } | OptInst::VMulF32x4 { a, b, .. } => {
                out.push(a);
                out.push(b);
            }
            _ => {}
        }
    }

    fn flags_written(&self) -> FlagMask {
        match *self {
            OptInst::Add { .. }
            | OptInst::Sub { .. }
            | OptInst::Mul { .. }
            | OptInst::Shl { .. }
            | OptInst::Cmp { .. }
            | OptInst::AddImm { .. }
            | OptInst::SetFlagsConst { .. } => FlagMask::ALL,
            _ => FlagMask::empty(),
        }
    }

    fn flags_mask_mut(&mut self) -> Option<&mut FlagMask> {
        match self {
            OptInst::Add { flags, .. }
            | OptInst::Sub { flags, .. }
            | OptInst::Mul { flags, .. }
            | OptInst::Shl { flags, .. }
            | OptInst::Cmp { flags, .. }
            | OptInst::AddImm { flags, .. } => Some(flags),
            OptInst::SetFlagsConst { mask, .. } => Some(mask),
            _ => None,
        }
    }
}

fn lower_to_opt_ir(func: &Function, selection: &[SelectedBlock]) -> OptRegion {
    let mut gpr_count = func.gpr_count;

    let mut blocks = Vec::with_capacity(selection.len());
    for sel in selection {
        let mut insts = Vec::new();
        for instr in &func.blocks[sel.orig].instrs {
            match instr {
                Instr::Imm { dst, imm } => insts.push(OptInst::Imm { dst: dst.0, imm: *imm }),
                Instr::Mov { dst, src } => insts.push(OptInst::Mov { dst: dst.0, src: src.0 }),
                Instr::Add { dst, a, b } => insts.push(OptInst::Add {
                    dst: dst.0,
                    a: a.0,
                    b: b.0,
                    flags: FlagMask::ALL,
                }),
                Instr::Sub { dst, a, b } => insts.push(OptInst::Sub {
                    dst: dst.0,
                    a: a.0,
                    b: b.0,
                    flags: FlagMask::ALL,
                }),
                Instr::Mul { dst, a, b } => insts.push(OptInst::Mul {
                    dst: dst.0,
                    a: a.0,
                    b: b.0,
                    flags: FlagMask::ALL,
                }),
                Instr::Shl { dst, src, shift } => insts.push(OptInst::Shl {
                    dst: dst.0,
                    src: src.0,
                    shift: *shift,
                    flags: FlagMask::ALL,
                }),
                Instr::Cmp { a, b } => insts.push(OptInst::Cmp {
                    a: a.0,
                    b: b.0,
                    flags: FlagMask::ALL,
                }),
                Instr::Load { dst, base, offset } => {
                    let addr_tmp = gpr_count;
                    gpr_count += 1;
                    insts.push(OptInst::AddImm {
                        dst: addr_tmp,
                        src: base.0,
                        imm: *offset,
                        flags: FlagMask::empty(),
                    });
                    insts.push(OptInst::LoadU64 { dst: dst.0, addr: addr_tmp });
                }
                Instr::Store { base, offset, src } => {
                    let addr_tmp = gpr_count;
                    gpr_count += 1;
                    insts.push(OptInst::AddImm {
                        dst: addr_tmp,
                        src: base.0,
                        imm: *offset,
                        flags: FlagMask::empty(),
                    });
                    insts.push(OptInst::StoreU64 { addr: addr_tmp, value: src.0 });
                }
                Instr::VImm { dst, imm } => insts.push(OptInst::VImm { dst: dst.0, imm: *imm }),
                Instr::VAddF32x4 { dst, a, b } => {
                    insts.push(OptInst::VAddF32x4 { dst: dst.0, a: a.0, b: b.0 })
                }
                Instr::VMulF32x4 { dst, a, b } => {
                    insts.push(OptInst::VMulF32x4 { dst: dst.0, a: a.0, b: b.0 })
                }
                Instr::Call { .. } => {
                    // Tier-2 regions do not include calls; selection should stop
                    // before them.
                }
            }
        }

        let term = match &sel.term {
            Terminator::Jmp(tgt) => OptTerm::Jump(*tgt),
            Terminator::Br { cond, then_tgt, else_tgt } => {
                let hot = sel.hot_succ.unwrap();
                let cold = if hot == *then_tgt { *else_tgt } else { *then_tgt };
                let expected = hot == *then_tgt;
                OptTerm::Guard { cond: *cond, expected, hot, cold }
            }
            Terminator::Ret { src } => OptTerm::Ret { src: src.0 },
        };

        blocks.push(OptBlock { orig: sel.orig, insts, term });
    }

    OptRegion {
        entry: selection[0].orig,
        preamble: Vec::new(),
        blocks,
        guest_gpr_count: func.gpr_count,
        gpr_count,
        xmm_count: func.xmm_count,
    }
}

// -----------------------------------------------------------------------------
// Optimization passes
// -----------------------------------------------------------------------------

fn compute_flag_liveness(region: &mut OptRegion) {
    // Conservative: assume all flags are observable on exits to outside the region.
    let mut block_index = HashMap::new();
    for (idx, blk) in region.blocks.iter().enumerate() {
        block_index.insert(blk.orig, idx);
    }

    let mut live_in_flags = vec![FlagMask::empty(); region.blocks.len()];
    let mut live_out_flags = vec![FlagMask::empty(); region.blocks.len()];

    // Fixed point backwards.
    let mut changed = true;
    while changed {
        changed = false;
        for (idx, blk) in region.blocks.iter().enumerate().rev() {
            let mut out = FlagMask::empty();
            match &blk.term {
                OptTerm::Jump(tgt) => {
                    if let Some(&succ_idx) = block_index.get(tgt) {
                        out |= live_in_flags[succ_idx];
                    } else {
                        out |= FlagMask::ALL;
                    }
                }
                OptTerm::Guard { cond, expected: _, hot, cold } => {
                    out |= cond.uses_flags();
                    if let Some(&succ_idx) = block_index.get(hot) {
                        out |= live_in_flags[succ_idx];
                    } else {
                        out |= FlagMask::ALL;
                    }
                    if !block_index.contains_key(cold) {
                        out |= FlagMask::ALL;
                    }
                }
                OptTerm::Ret { .. } => {
                    out |= FlagMask::ALL;
                }
            }

            if out != live_out_flags[idx] {
                live_out_flags[idx] = out;
                changed = true;
            }

            // Approximate uses/defs per block for flags only.
            let mut live = out;
            for inst in blk.insts.iter().rev() {
                let written = inst.flags_written();
                if !written.is_empty() {
                    // Standard liveness: defs kill.
                    live &= !written;
                }
                // No instruction reads flags (only terminators do) in this toy.
            }
            if live != live_in_flags[idx] {
                live_in_flags[idx] = live;
                changed = true;
            }
        }
    }

    // Now compute per-instruction required flag masks by walking backwards.
    for (idx, blk) in region.blocks.iter_mut().enumerate() {
        let mut live = live_out_flags[idx];
        for inst in blk.insts.iter_mut().rev() {
            let written = inst.flags_written();
            if written.is_empty() {
                continue;
            }
            let needed = written & live;
            if let Some(mask) = inst.flags_mask_mut() {
                *mask = needed;
            }
            // Only the needed bits are killed; bits not computed flow through.
            live &= !needed;
        }
    }
}

fn strength_reduction(region: &mut OptRegion) {
    for blk in &mut region.blocks {
        let mut consts: HashMap<GprId, u64> = HashMap::new();
        for inst in &mut blk.insts {
            match *inst {
                OptInst::Imm { dst, imm } => {
                    consts.insert(dst, imm);
                }
                OptInst::Mov { dst, src } => {
                    if let Some(v) = consts.get(&src).copied() {
                        consts.insert(dst, v);
                    } else {
                        consts.remove(&dst);
                    }
                }
                OptInst::Mul { dst, a, b, flags } => {
                    let (var, cst) = match (consts.get(&a).copied(), consts.get(&b).copied()) {
                        (Some(va), None) => (b, va),
                        (None, Some(vb)) => (a, vb),
                        _ => {
                            consts.remove(&dst);
                            continue;
                        }
                    };
                    if cst.is_power_of_two() {
                        let shift = cst.trailing_zeros() as u8;
                        *inst = OptInst::Shl { dst, src: var, shift, flags };
                        consts.remove(&dst);
                    } else {
                        consts.remove(&dst);
                    }
                }
                OptInst::AddImm { dst, src, imm, .. } => {
                    if let Some(v) = consts.get(&src).copied() {
                        consts.insert(dst, v.wrapping_add(imm as i64 as u64));
                    } else {
                        consts.remove(&dst);
                    }
                }
                OptInst::Add { dst, .. }
                | OptInst::Sub { dst, .. }
                | OptInst::Shl { dst, .. }
                | OptInst::LoadU64 { dst, .. } => {
                    consts.remove(&dst);
                }
                _ => {}
            }
        }
    }
}

fn constant_folding(region: &mut OptRegion) {
    for blk in &mut region.blocks {
        let mut const_gpr: HashMap<GprId, u64> = HashMap::new();
        let mut const_xmm: HashMap<XmmId, u128> = HashMap::new();

        let mut new_insts = Vec::with_capacity(blk.insts.len());
        for inst in blk.insts.drain(..) {
            match inst.clone() {
                OptInst::Imm { dst, imm } => {
                    const_gpr.insert(dst, imm);
                    new_insts.push(inst);
                }
                OptInst::Mov { dst, src } => {
                    if let Some(v) = const_gpr.get(&src).copied() {
                        const_gpr.insert(dst, v);
                        new_insts.push(OptInst::Imm { dst, imm: v });
                    } else {
                        const_gpr.remove(&dst);
                        new_insts.push(inst);
                    }
                }
                OptInst::Add { dst, a, b, flags } => {
                    let (Some(va), Some(vb)) = (const_gpr.get(&a).copied(), const_gpr.get(&b).copied())
                    else {
                        const_gpr.remove(&dst);
                        new_insts.push(inst);
                        continue;
                    };
                    let rr = va.wrapping_add(vb);
                    const_gpr.insert(dst, rr);
                    new_insts.push(OptInst::Imm { dst, imm: rr });
                    if !flags.is_empty() {
                        let mut tmp_flags = 0u8;
                        set_flags_add(rr, va, vb, flags, &mut tmp_flags);
                        new_insts.push(OptInst::SetFlagsConst { value: tmp_flags, mask: flags });
                    }
                }
                OptInst::Sub { dst, a, b, flags } => {
                    let (Some(va), Some(vb)) = (const_gpr.get(&a).copied(), const_gpr.get(&b).copied())
                    else {
                        const_gpr.remove(&dst);
                        new_insts.push(inst);
                        continue;
                    };
                    let rr = va.wrapping_sub(vb);
                    const_gpr.insert(dst, rr);
                    new_insts.push(OptInst::Imm { dst, imm: rr });
                    if !flags.is_empty() {
                        let mut tmp_flags = 0u8;
                        set_flags_sub(rr, va, vb, flags, &mut tmp_flags);
                        new_insts.push(OptInst::SetFlagsConst { value: tmp_flags, mask: flags });
                    }
                }
                OptInst::Shl { dst, src, shift, flags } => {
                    let Some(v) = const_gpr.get(&src).copied() else {
                        const_gpr.remove(&dst);
                        new_insts.push(inst);
                        continue;
                    };
                    let rr = v.wrapping_shl(shift as u32);
                    const_gpr.insert(dst, rr);
                    new_insts.push(OptInst::Imm { dst, imm: rr });
                    if !flags.is_empty() {
                        let mut tmp_flags = 0u8;
                        set_flags_logic(rr, flags, &mut tmp_flags);
                        new_insts.push(OptInst::SetFlagsConst { value: tmp_flags, mask: flags });
                    }
                }
                OptInst::Cmp { a, b, flags } => {
                    let (Some(va), Some(vb)) = (const_gpr.get(&a).copied(), const_gpr.get(&b).copied())
                    else {
                        new_insts.push(inst);
                        continue;
                    };
                    if !flags.is_empty() {
                        let rr = va.wrapping_sub(vb);
                        let mut tmp_flags = 0u8;
                        set_flags_sub(rr, va, vb, flags, &mut tmp_flags);
                        new_insts.push(OptInst::SetFlagsConst { value: tmp_flags, mask: flags });
                    }
                }
                OptInst::AddImm { dst, src, imm, flags } => {
                    let Some(v) = const_gpr.get(&src).copied() else {
                        const_gpr.remove(&dst);
                        new_insts.push(inst);
                        continue;
                    };
                    let rr = v.wrapping_add(imm as i64 as u64);
                    const_gpr.insert(dst, rr);
                    new_insts.push(OptInst::Imm { dst, imm: rr });
                    if !flags.is_empty() {
                        let mut tmp_flags = 0u8;
                        set_flags_add(rr, v, imm as i64 as u64, flags, &mut tmp_flags);
                        new_insts.push(OptInst::SetFlagsConst { value: tmp_flags, mask: flags });
                    }
                }
                OptInst::LoadU64 { dst, .. } => {
                    const_gpr.remove(&dst);
                    new_insts.push(inst);
                }
                OptInst::StoreU64 { .. } => {
                    new_insts.push(inst);
                }
                OptInst::VImm { dst, imm } => {
                    const_xmm.insert(dst, imm);
                    new_insts.push(inst);
                }
                OptInst::VAddF32x4 { dst, a, b } => {
                    let (Some(va), Some(vb)) =
                        (const_xmm.get(&a).copied(), const_xmm.get(&b).copied())
                    else {
                        const_xmm.remove(&dst);
                        new_insts.push(inst);
                        continue;
                    };
                    let rr = simd_f32x4_add(va, vb);
                    const_xmm.insert(dst, rr);
                    new_insts.push(OptInst::VImm { dst, imm: rr });
                }
                OptInst::VMulF32x4 { dst, a, b } => {
                    let (Some(va), Some(vb)) =
                        (const_xmm.get(&a).copied(), const_xmm.get(&b).copied())
                    else {
                        const_xmm.remove(&dst);
                        new_insts.push(inst);
                        continue;
                    };
                    let rr = simd_f32x4_mul(va, vb);
                    const_xmm.insert(dst, rr);
                    new_insts.push(OptInst::VImm { dst, imm: rr });
                }
                OptInst::SetFlagsConst { .. } => new_insts.push(inst),
                OptInst::Mul { dst, .. } => {
                    const_gpr.remove(&dst);
                    new_insts.push(inst);
                }
            }
        }

        blk.insts = new_insts;
    }
}

fn common_subexpression_elimination(region: &mut OptRegion) {
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    enum OpKey {
        Add,
        Sub,
        Mul,
        Shl(u8),
        AddImm(i32),
    }

    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    struct ExprKey {
        op: OpKey,
        a: u32,
        b: u32,
    }

    for blk in &mut region.blocks {
        let mut next_vn: u32 = 1;
        let mut vn_of_gpr: HashMap<GprId, u32> = HashMap::new();
        let mut vn_of_xmm: HashMap<XmmId, u32> = HashMap::new();
        let mut expr_to_vn: HashMap<ExprKey, u32> = HashMap::new();
        let mut vn_rep_gpr: HashMap<u32, GprId> = HashMap::new();
        let mut vn_rep_xmm: HashMap<u32, XmmId> = HashMap::new();

        // Assign initial VNs for regs we see.
        for inst in &blk.insts {
            let mut uses = Vec::new();
            inst.uses_gprs(&mut uses);
            for r in uses {
                vn_of_gpr.entry(r).or_insert_with(|| {
                    let vn = next_vn;
                    next_vn += 1;
                    vn_rep_gpr.insert(vn, r);
                    vn
                });
            }
            let mut vuses = Vec::new();
            inst.uses_xmms(&mut vuses);
            for r in vuses {
                vn_of_xmm.entry(r).or_insert_with(|| {
                    let vn = next_vn;
                    next_vn += 1;
                    vn_rep_xmm.insert(vn, r);
                    vn
                });
            }
        }

        let mut new_insts = Vec::with_capacity(blk.insts.len());
        for inst in blk.insts.drain(..) {
            match inst {
                OptInst::Add { dst, a, b, flags } if flags.is_empty() => {
                    let va = *vn_of_gpr.entry(a).or_insert_with(|| fresh_vn(&mut next_vn, &mut vn_rep_gpr, a));
                    let vb = *vn_of_gpr.entry(b).or_insert_with(|| fresh_vn(&mut next_vn, &mut vn_rep_gpr, b));
                    let mut key = ExprKey { op: OpKey::Add, a: va, b: vb };
                    if va > vb {
                        std::mem::swap(&mut key.a, &mut key.b);
                    }
                    if let Some(&vn) = expr_to_vn.get(&key) {
                        let src = vn_rep_gpr[&vn];
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Mov { dst, src });
                    } else {
                        let vn = next_vn;
                        next_vn += 1;
                        expr_to_vn.insert(key, vn);
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Add { dst, a, b, flags });
                    }
                }
                OptInst::Mul { dst, a, b, flags } if flags.is_empty() => {
                    let va = *vn_of_gpr.entry(a).or_insert_with(|| fresh_vn(&mut next_vn, &mut vn_rep_gpr, a));
                    let vb = *vn_of_gpr.entry(b).or_insert_with(|| fresh_vn(&mut next_vn, &mut vn_rep_gpr, b));
                    let mut key = ExprKey { op: OpKey::Mul, a: va, b: vb };
                    if va > vb {
                        std::mem::swap(&mut key.a, &mut key.b);
                    }
                    if let Some(&vn) = expr_to_vn.get(&key) {
                        let src = vn_rep_gpr[&vn];
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Mov { dst, src });
                    } else {
                        let vn = next_vn;
                        next_vn += 1;
                        expr_to_vn.insert(key, vn);
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Mul { dst, a, b, flags });
                    }
                }
                OptInst::Sub { dst, a, b, flags } if flags.is_empty() => {
                    let va = *vn_of_gpr.entry(a).or_insert_with(|| fresh_vn(&mut next_vn, &mut vn_rep_gpr, a));
                    let vb = *vn_of_gpr.entry(b).or_insert_with(|| fresh_vn(&mut next_vn, &mut vn_rep_gpr, b));
                    let key = ExprKey { op: OpKey::Sub, a: va, b: vb };
                    if let Some(&vn) = expr_to_vn.get(&key) {
                        let src = vn_rep_gpr[&vn];
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Mov { dst, src });
                    } else {
                        let vn = next_vn;
                        next_vn += 1;
                        expr_to_vn.insert(key, vn);
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Sub { dst, a, b, flags });
                    }
                }
                OptInst::Shl { dst, src, shift, flags } if flags.is_empty() => {
                    let vsrc = *vn_of_gpr.entry(src).or_insert_with(|| fresh_vn(&mut next_vn, &mut vn_rep_gpr, src));
                    let key = ExprKey { op: OpKey::Shl(shift), a: vsrc, b: 0 };
                    if let Some(&vn) = expr_to_vn.get(&key) {
                        let src_reg = vn_rep_gpr[&vn];
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Mov { dst, src: src_reg });
                    } else {
                        let vn = next_vn;
                        next_vn += 1;
                        expr_to_vn.insert(key, vn);
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Shl { dst, src, shift, flags });
                    }
                }
                OptInst::AddImm { dst, src, imm, flags } if flags.is_empty() => {
                    let vsrc = *vn_of_gpr.entry(src).or_insert_with(|| fresh_vn(&mut next_vn, &mut vn_rep_gpr, src));
                    let key = ExprKey { op: OpKey::AddImm(imm), a: vsrc, b: 0 };
                    if let Some(&vn) = expr_to_vn.get(&key) {
                        let src_reg = vn_rep_gpr[&vn];
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::Mov { dst, src: src_reg });
                    } else {
                        let vn = next_vn;
                        next_vn += 1;
                        expr_to_vn.insert(key, vn);
                        vn_of_gpr.insert(dst, vn);
                        vn_rep_gpr.insert(vn, dst);
                        new_insts.push(OptInst::AddImm { dst, src, imm, flags });
                    }
                }
                other => {
                    // On side effects, clear expression table.
                    if other.has_side_effects() {
                        expr_to_vn.clear();
                    }
                    if let Some(dst) = other.defs_gpr() {
                        vn_of_gpr.insert(dst, fresh_vn(&mut next_vn, &mut vn_rep_gpr, dst));
                    }
                    if let Some(dst) = other.defs_xmm() {
                        vn_of_xmm.insert(dst, fresh_vn_xmm(&mut next_vn, &mut vn_rep_xmm, dst));
                    }
                    new_insts.push(other);
                }
            }
        }
        blk.insts = new_insts;
    }

    fn fresh_vn(next_vn: &mut u32, rep: &mut HashMap<u32, GprId>, reg: GprId) -> u32 {
        let vn = *next_vn;
        *next_vn += 1;
        rep.insert(vn, reg);
        vn
    }

    fn fresh_vn_xmm(next_vn: &mut u32, rep: &mut HashMap<u32, XmmId>, reg: XmmId) -> u32 {
        let vn = *next_vn;
        *next_vn += 1;
        rep.insert(vn, reg);
        vn
    }
}

fn dead_code_elimination(region: &mut OptRegion, func: &Function) {
    let gpr_total = region.gpr_count as usize;
    let xmm_total = region.xmm_count as usize;

    #[derive(Clone)]
    struct Live {
        gpr: Vec<bool>,
        xmm: Vec<bool>,
        flags: FlagMask,
    }

    let mut block_index = HashMap::new();
    for (idx, blk) in region.blocks.iter().enumerate() {
        block_index.insert(blk.orig, idx);
    }

    let mut live_in = vec![
        Live { gpr: vec![false; gpr_total], xmm: vec![false; xmm_total], flags: FlagMask::empty() };
        region.blocks.len()
    ];
    let mut live_out = live_in.clone();

    let guest_gprs = func.gpr_count as usize;
    let guest_xmms = func.xmm_count as usize;
    let live_all_exit = |live: &mut Live| {
        for r in 0..guest_gprs {
            live.gpr[r] = true;
        }
        for r in 0..guest_xmms {
            live.xmm[r] = true;
        }
        live.flags |= FlagMask::ALL;
    };

    // Dataflow fixed point.
    let mut changed = true;
    while changed {
        changed = false;
        for (idx, blk) in region.blocks.iter().enumerate().rev() {
            let mut out = Live { gpr: vec![false; gpr_total], xmm: vec![false; xmm_total], flags: FlagMask::empty() };
            match &blk.term {
                OptTerm::Jump(tgt) => {
                    if let Some(&succ) = block_index.get(tgt) {
                        out = live_in[succ].clone();
                    } else {
                        live_all_exit(&mut out);
                    }
                }
                OptTerm::Guard { cond, expected: _, hot, cold } => {
                    out.flags |= cond.uses_flags();
                    if let Some(&succ) = block_index.get(hot) {
                        union_live(&mut out, &live_in[succ]);
                    } else {
                        live_all_exit(&mut out);
                    }
                    if !block_index.contains_key(cold) {
                        live_all_exit(&mut out);
                    }
                }
                OptTerm::Ret { .. } => {
                    live_all_exit(&mut out);
                }
            }

            if !eq_live(&out, &live_out[idx]) {
                live_out[idx] = out.clone();
                changed = true;
            }

            // Compute live_in = uses U (out - defs)
            let mut live = out;
            for inst in blk.insts.iter().rev() {
                if let Some(d) = inst.defs_gpr() {
                    live.gpr[d as usize] = false;
                }
                if let Some(d) = inst.defs_xmm() {
                    live.xmm[d as usize] = false;
                }
                let written_flags = inst.flags_written();
                if !written_flags.is_empty() {
                    live.flags &= !written_flags;
                }
                let mut uses = Vec::new();
                inst.uses_gprs(&mut uses);
                for u in uses {
                    live.gpr[u as usize] = true;
                }
                let mut vuses = Vec::new();
                inst.uses_xmms(&mut vuses);
                for u in vuses {
                    live.xmm[u as usize] = true;
                }
            }

            if !eq_live(&live, &live_in[idx]) {
                live_in[idx] = live;
                changed = true;
            }
        }
    }

    // Now do per-block DCE based on per-instruction liveness walk.
    for (idx, blk) in region.blocks.iter_mut().enumerate() {
        let mut live = live_out[idx].clone();
        let mut new_insts = Vec::with_capacity(blk.insts.len());
        for inst in blk.insts.drain(..).rev() {
            let mut keep = inst.has_side_effects();

            // If the instruction writes flags, keep only if it writes needed bits.
            if let Some(mask) = match inst.clone() {
                OptInst::SetFlagsConst { mask, .. } => Some(mask),
                OptInst::Add { flags, .. }
                | OptInst::Sub { flags, .. }
                | OptInst::Mul { flags, .. }
                | OptInst::Shl { flags, .. }
                | OptInst::Cmp { flags, .. }
                | OptInst::AddImm { flags, .. } => Some(flags),
                _ => None,
            } {
                if !mask.is_empty() {
                    keep = true;
                }
            }

            if let Some(d) = inst.defs_gpr() {
                if live.gpr[d as usize] {
                    keep = true;
                }
            }
            if let Some(d) = inst.defs_xmm() {
                if live.xmm[d as usize] {
                    keep = true;
                }
            }

            if keep {
                // Update liveness.
                if let Some(d) = inst.defs_gpr() {
                    live.gpr[d as usize] = false;
                }
                if let Some(d) = inst.defs_xmm() {
                    live.xmm[d as usize] = false;
                }
                let written = inst.flags_written();
                if !written.is_empty() {
                    live.flags &= !written;
                }
                let mut uses = Vec::new();
                inst.uses_gprs(&mut uses);
                for u in uses {
                    live.gpr[u as usize] = true;
                }
                let mut vuses = Vec::new();
                inst.uses_xmms(&mut vuses);
                for u in vuses {
                    live.xmm[u as usize] = true;
                }
                new_insts.push(inst);
            }
        }
        new_insts.reverse();
        blk.insts = new_insts;
    }

    // Preamble DCE: conservatively keep everything for now.
    fn union_live(dst: &mut Live, src: &Live) {
        for (d, s) in dst.gpr.iter_mut().zip(&src.gpr) {
            *d |= *s;
        }
        for (d, s) in dst.xmm.iter_mut().zip(&src.xmm) {
            *d |= *s;
        }
        dst.flags |= src.flags;
    }

    fn eq_live(a: &Live, b: &Live) -> bool {
        a.flags == b.flags && a.gpr == b.gpr && a.xmm == b.xmm
    }
}

fn loop_invariant_code_motion(region: &mut OptRegion) {
    // Detect a simple backedge: last block jumps to an earlier block.
    let Some(last) = region.blocks.last() else { return };
    let backedge_tgt = match &last.term {
        OptTerm::Jump(tgt) => *tgt,
        _ => return,
    };
    let header_idx = region.blocks.iter().position(|b| b.orig == backedge_tgt);
    let Some(header_idx) = header_idx else { return };

    // We treat blocks[header_idx..] as the loop body.
    let loop_blocks = &region.blocks[header_idx..];

    // Compute write counts for gprs in the loop.
    let mut write_counts: HashMap<GprId, usize> = HashMap::new();
    for blk in loop_blocks {
        for inst in &blk.insts {
            if let Some(d) = inst.defs_gpr() {
                *write_counts.entry(d).or_insert(0) += 1;
            }
        }
    }

    // Compute live-in of header (rough): any use before def in header.
    let mut live_in_header: HashMap<GprId, bool> = HashMap::new();
    let mut defined = HashMap::<GprId, bool>::new();
    for inst in &region.blocks[header_idx].insts {
        let mut uses = Vec::new();
        inst.uses_gprs(&mut uses);
        for u in uses {
            if !defined.get(&u).copied().unwrap_or(false) {
                live_in_header.insert(u, true);
            }
        }
        if let Some(d) = inst.defs_gpr() {
            defined.insert(d, true);
        }
    }

    // Fixed point hoisting: after hoisting an instruction we re-run because
    // the write set shrinks.
    loop {
        let mut changed = false;
        for blk in region.blocks[header_idx..].iter_mut() {
            let mut i = 0;
            while i < blk.insts.len() {
                let inst = &blk.insts[i];
                if inst.has_side_effects() {
                    i += 1;
                    continue;
                }
                let Some(dst) = inst.defs_gpr() else {
                    i += 1;
                    continue;
                };
                if dst < region.guest_gpr_count {
                    // Only hoist temporaries; hoisting architectural regs would
                    // require full dominance/phi reasoning.
                    i += 1;
                    continue;
                }
                if *write_counts.get(&dst).unwrap_or(&0) != 1 {
                    i += 1;
                    continue;
                }
                if live_in_header.get(&dst).copied().unwrap_or(false) {
                    i += 1;
                    continue;
                }
                // Check operands are not written in loop.
                let mut uses = Vec::new();
                inst.uses_gprs(&mut uses);
                if uses.iter().any(|u| write_counts.contains_key(u)) {
                    i += 1;
                    continue;
                }
                // Hoist.
                let inst = blk.insts.remove(i);
                region.preamble.push(inst);
                write_counts.remove(&dst);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

// -----------------------------------------------------------------------------
// Codegen + register allocation
// -----------------------------------------------------------------------------

fn codegen(mut region: OptRegion, func: &Function, config: &JitConfig, vm: &Vm) -> CompiledRegion {
    // Register allocation: pick hot regs as "locals" and the rest as spill slots.
    let reg_alloc = allocate_slots(&region, func, config);

    let mut wasm_simd_listing = Vec::new();

    let mut preamble = Vec::new();
    for inst in &region.preamble {
        lower_inst(inst, &reg_alloc, &mut preamble, &mut wasm_simd_listing);
    }

    let mut blocks = Vec::new();
    for blk in region.blocks.drain(..) {
        let mut ops = Vec::new();
        for inst in &blk.insts {
            lower_inst(inst, &reg_alloc, &mut ops, &mut wasm_simd_listing);
        }
        let term = match blk.term {
            OptTerm::Jump(tgt) => CompiledTerm::Jump(tgt),
            OptTerm::Guard { cond, expected, hot, cold } => CompiledTerm::Guard { cond, expected, hot, cold },
            OptTerm::Ret { src } => CompiledTerm::Ret { src: reg_alloc.gpr_slots[src as usize] },
        };
        blocks.push(CompiledBlock { orig: blk.orig, ops, term });
    }

    let block_index: HashMap<BlockId, usize> = blocks.iter().enumerate().map(|(idx, blk)| (blk.orig, idx)).collect();
    CompiledRegion {
        entry: region.entry,
        blocks,
        block_index,
        preamble,
        reg_alloc,
        guard_perm_epoch: vm.mem.perm_epoch(),
        guard_code_epoch: vm.mem.code_epoch(),
        wasm_simd_listing,
    }
}

fn lower_inst(inst: &OptInst, alloc: &RegAlloc, out: &mut Vec<CompiledOp>, wasm: &mut Vec<WasmInst>) {
    match *inst {
        OptInst::Imm { dst, imm } => out.push(CompiledOp::Imm { dst: alloc.gpr_slots[dst as usize], imm }),
        OptInst::Mov { dst, src } => out.push(CompiledOp::Mov {
            dst: alloc.gpr_slots[dst as usize],
            src: alloc.gpr_slots[src as usize],
        }),
        OptInst::Add { dst, a, b, flags } => out.push(CompiledOp::Add {
            dst: alloc.gpr_slots[dst as usize],
            a: alloc.gpr_slots[a as usize],
            b: alloc.gpr_slots[b as usize],
            flags,
        }),
        OptInst::Sub { dst, a, b, flags } => out.push(CompiledOp::Sub {
            dst: alloc.gpr_slots[dst as usize],
            a: alloc.gpr_slots[a as usize],
            b: alloc.gpr_slots[b as usize],
            flags,
        }),
        OptInst::Mul { dst, a, b, flags } => out.push(CompiledOp::Mul {
            dst: alloc.gpr_slots[dst as usize],
            a: alloc.gpr_slots[a as usize],
            b: alloc.gpr_slots[b as usize],
            flags,
        }),
        OptInst::Shl { dst, src, shift, flags } => out.push(CompiledOp::Shl {
            dst: alloc.gpr_slots[dst as usize],
            src: alloc.gpr_slots[src as usize],
            shift,
            flags,
        }),
        OptInst::Cmp { a, b, flags } => out.push(CompiledOp::Cmp {
            a: alloc.gpr_slots[a as usize],
            b: alloc.gpr_slots[b as usize],
            flags,
        }),
        OptInst::SetFlagsConst { value, mask } => out.push(CompiledOp::SetFlagsConst { value, mask }),
        OptInst::AddImm { dst, src, imm, flags } => {
            out.push(CompiledOp::AddImm {
                dst: alloc.gpr_slots[dst as usize],
                src: alloc.gpr_slots[src as usize],
                imm,
                flags,
            });
        }
        OptInst::LoadU64 { dst, addr } => out.push(CompiledOp::LoadU64 {
            dst: alloc.gpr_slots[dst as usize],
            addr: alloc.gpr_slots[addr as usize],
        }),
        OptInst::StoreU64 { addr, value } => out.push(CompiledOp::StoreU64 {
            addr: alloc.gpr_slots[addr as usize],
            value: alloc.gpr_slots[value as usize],
        }),
        OptInst::VImm { dst, imm } => out.push(CompiledOp::VImm { dst: alloc.xmm_slots[dst as usize], imm }),
        OptInst::VAddF32x4 { dst, a, b } => {
            wasm.push(WasmInst::F32x4Add);
            out.push(CompiledOp::VAddF32x4 {
                dst: alloc.xmm_slots[dst as usize],
                a: alloc.xmm_slots[a as usize],
                b: alloc.xmm_slots[b as usize],
            });
        }
        OptInst::VMulF32x4 { dst, a, b } => {
            wasm.push(WasmInst::F32x4Mul);
            out.push(CompiledOp::VMulF32x4 {
                dst: alloc.xmm_slots[dst as usize],
                a: alloc.xmm_slots[a as usize],
                b: alloc.xmm_slots[b as usize],
            });
        }
    }
}

fn allocate_slots(region: &OptRegion, func: &Function, config: &JitConfig) -> RegAlloc {
    let gpr_total = region.gpr_count as usize;
    let xmm_total = region.xmm_count as usize;

    let mut gpr_use = vec![0u32; gpr_total];
    let mut xmm_use = vec![0u32; xmm_total];

    for inst in region.preamble.iter().chain(region.blocks.iter().flat_map(|b| &b.insts)) {
        let mut uses = Vec::new();
        inst.uses_gprs(&mut uses);
        for u in uses {
            gpr_use[u as usize] = gpr_use[u as usize].saturating_add(1);
        }
        if let Some(d) = inst.defs_gpr() {
            gpr_use[d as usize] = gpr_use[d as usize].saturating_add(1);
        }

        let mut vuses = Vec::new();
        inst.uses_xmms(&mut vuses);
        for u in vuses {
            xmm_use[u as usize] = xmm_use[u as usize].saturating_add(1);
        }
        if let Some(d) = inst.defs_xmm() {
            xmm_use[d as usize] = xmm_use[d as usize].saturating_add(1);
        }
    }

    let mut gpr_ids: Vec<u16> = (0..region.gpr_count).collect();
    gpr_ids.sort_by_key(|&r| std::cmp::Reverse(gpr_use[r as usize]));
    let local_gpr = config.max_gpr_locals.min(gpr_ids.len());
    let mut gpr_slots = vec![Slot(0); gpr_total];
    let mut slot_cursor = 0u16;
    for &r in &gpr_ids[..local_gpr] {
        gpr_slots[r as usize] = Slot(slot_cursor);
        slot_cursor += 1;
    }
    for &r in &gpr_ids[local_gpr..] {
        gpr_slots[r as usize] = Slot(slot_cursor);
        slot_cursor += 1;
    }

    let mut xmm_ids: Vec<u16> = (0..region.xmm_count).collect();
    xmm_ids.sort_by_key(|&r| std::cmp::Reverse(xmm_use[r as usize]));
    let local_xmm = config.max_xmm_locals.min(xmm_ids.len());
    let mut xmm_slots = vec![Slot(0); xmm_total];
    let mut xmm_cursor = 0u16;
    for &r in &xmm_ids[..local_xmm] {
        xmm_slots[r as usize] = Slot(xmm_cursor);
        xmm_cursor += 1;
    }
    for &r in &xmm_ids[local_xmm..] {
        xmm_slots[r as usize] = Slot(xmm_cursor);
        xmm_cursor += 1;
    }

    RegAlloc {
        guest_gpr_count: func.gpr_count,
        guest_xmm_count: func.xmm_count,
        gpr_slots,
        xmm_slots,
        gpr_slot_count: slot_cursor,
        xmm_slot_count: xmm_cursor,
        _gpr_local_slots: local_gpr as u16,
        _xmm_local_slots: local_xmm as u16,
    }
}
