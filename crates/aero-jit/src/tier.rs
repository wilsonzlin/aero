use crate::microvm::{
    set_flags_add, set_flags_logic, set_flags_sub, BlockId, Cond, FlagMask, FuncId, Instr, Program,
    Terminator, Vm,
};
use crate::opt::{compile_region, CompiledRegion, ExecOutcome};
use crate::profile::FuncProfile;
use perf::jit::JitTier;
use perf::telemetry::{Telemetry, TelemetrySnapshot};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct JitConfig {
    /// After this many executions of a block, Tier-1 compilation is triggered.
    pub tier1_threshold: u64,
    /// After this many executions of a block, Tier-2 compilation is triggered.
    pub tier2_threshold: u64,

    /// Maximum number of blocks in a Tier-2 trace/region.
    pub max_region_blocks: usize,

    /// Register allocation: keep the hottest regs in the first N "local" slots.
    pub max_gpr_locals: usize,
    pub max_xmm_locals: usize,

    /// Target capacity for the combined Tier-1 + Tier-2 code cache.
    ///
    /// This prototype does not enforce eviction yet, but the value is still
    /// exported via PF-006 telemetry so cache utilization can be tracked.
    pub code_cache_capacity_bytes: u64,
}

impl Default for JitConfig {
    fn default() -> Self {
        Self {
            tier1_threshold: 10,
            tier2_threshold: 1_000,
            max_region_blocks: 32,
            max_gpr_locals: 24,
            max_xmm_locals: 16,
            code_cache_capacity_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub tier1_blocks_compiled: usize,
    pub tier2_regions_compiled: usize,
    pub tier2_exec_count: u64,
    pub tier2_deopt_count: u64,
}

pub struct Engine {
    config: JitConfig,
    profiles: Vec<FuncProfile>,
    tier1_blocks: HashMap<(FuncId, BlockId), Tier1Block>,
    tier2_regions: HashMap<(FuncId, BlockId), CompiledRegion>,
    stats: EngineStats,
    telemetry: Telemetry,
}

impl Engine {
    pub fn new(program: &Program, config: JitConfig) -> Self {
        let profiles = program
            .functions
            .iter()
            .map(|f| FuncProfile::new(f.blocks.len()))
            .collect();
        let jit_enabled = config.tier1_threshold != u64::MAX || config.tier2_threshold != u64::MAX;
        let telemetry = Telemetry::new(jit_enabled);
        telemetry
            .jit
            .set_cache_capacity_bytes(config.code_cache_capacity_bytes);
        Self {
            config,
            profiles,
            tier1_blocks: HashMap::new(),
            tier2_regions: HashMap::new(),
            stats: EngineStats::default(),
            telemetry,
        }
    }

    pub fn stats(&self) -> EngineStats {
        self.stats
    }

    /// Snapshot PF-006 JIT telemetry (totals + rolling window deltas).
    pub fn telemetry_snapshot(&self) -> TelemetrySnapshot {
        self.telemetry.snapshot()
    }

    pub fn run(&mut self, vm: &mut Vm, program: &Program, func: FuncId) -> u64 {
        let func_ref = &program.functions[func];
        assert_eq!(vm.gprs.len(), func_ref.gpr_count as usize);
        assert_eq!(vm.xmms.len(), func_ref.xmm_count as usize);

        let mut cur = func_ref.entry;
        loop {
            let exec_count = self.profiles[func].record_block_entry(cur);
            if exec_count >= self.config.tier1_threshold {
                self.maybe_compile_tier1(program, func, cur);
            }
            if exec_count >= self.config.tier2_threshold {
                self.maybe_compile_tier2(program, vm, func, cur);
            }

            // Tier-2 has priority.
            if let Some(region) = self.tier2_regions.get(&(func, cur)).cloned() {
                self.telemetry.jit.record_cache_hit();
                self.stats.tier2_exec_count = self.stats.tier2_exec_count.wrapping_add(1);
                match region.execute(vm) {
                    ExecOutcome::Return(v) => return v,
                    ExecOutcome::Continue(next) => {
                        cur = next;
                        continue;
                    }
                    ExecOutcome::GuardFailed(next) => {
                        self.telemetry.jit.record_guard_fail();
                        cur = next;
                        continue;
                    }
                    ExecOutcome::Deopt(next) => {
                        self.stats.tier2_deopt_count = self.stats.tier2_deopt_count.wrapping_add(1);
                        self.telemetry.jit.record_deopt();
                        // Invalidate and resume in the interpreter / tier1.
                        self.tier2_regions.remove(&(func, cur));
                        self.sync_code_cache_metrics();
                        cur = next;
                        continue;
                    }
                }
            }

            if let Some(block) = self.tier1_blocks.get(&(func, cur)).cloned() {
                self.telemetry.jit.record_cache_hit();
                match block.execute(self, vm, program, func, cur) {
                    BlockOutcome::Return(v) => return v,
                    BlockOutcome::Next(next) => cur = next,
                }
                continue;
            }

            self.telemetry.jit.record_cache_miss();
            match interpret_block(self, vm, program, func, cur) {
                BlockOutcome::Return(v) => return v,
                BlockOutcome::Next(next) => cur = next,
            }
        }
    }

    fn maybe_compile_tier1(&mut self, program: &Program, func: FuncId, block: BlockId) {
        let key = (func, block);
        if self.tier1_blocks.contains_key(&key) {
            return;
        }
        let blk = &program.functions[func].blocks[block];
        let start = Instant::now();
        let compiled = Tier1Block::compile(blk);
        self.telemetry
            .jit
            .add_compile_time(JitTier::Tier1, start.elapsed());
        self.telemetry.jit.record_block_compiled(JitTier::Tier1);
        self.tier1_blocks.insert(key, compiled);
        self.stats.tier1_blocks_compiled += 1;
        self.sync_code_cache_metrics();
    }

    fn maybe_compile_tier2(&mut self, program: &Program, vm: &Vm, func: FuncId, block: BlockId) {
        if self.tier2_regions.contains_key(&(func, block)) {
            return;
        }
        let prof = &self.profiles[func];
        let metrics = self
            .telemetry
            .jit
            .enabled()
            .then_some(self.telemetry.jit.as_ref());
        let start = Instant::now();
        if let Some(region) = compile_region(program, func, block, prof, &self.config, vm, metrics)
        {
            self.tier2_regions.insert((func, block), region);
            self.stats.tier2_regions_compiled += 1;
            self.telemetry
                .jit
                .add_compile_time(JitTier::Tier2, start.elapsed());
            self.telemetry.jit.record_block_compiled(JitTier::Tier2);
            self.sync_code_cache_metrics();
        }
    }

    fn record_branch(&mut self, func: FuncId, block: BlockId, cond: Cond, taken_then: bool) {
        self.profiles[func].record_branch(block, cond, taken_then);
    }

    fn record_call(&mut self, func: FuncId, callee: FuncId) {
        self.profiles[func].record_call(callee);
    }

    fn estimated_code_cache_bytes(&self) -> u64 {
        let mut total = 0u64;
        for block in self.tier1_blocks.values() {
            total = total.saturating_add(block.estimated_size_bytes());
        }
        for region in self.tier2_regions.values() {
            total = total.saturating_add(region.estimated_size_bytes());
        }
        total
    }

    fn sync_code_cache_metrics(&mut self) {
        if !self.telemetry.jit.enabled() {
            return;
        }
        let used = self.estimated_code_cache_bytes();
        self.telemetry.jit.set_cache_used_bytes(used);
        self.telemetry
            .jit
            .set_cache_capacity_bytes(self.config.code_cache_capacity_bytes);
    }
}

// -----------------------------------------------------------------------------
// Tier-0 interpreter
// -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockOutcome {
    Next(BlockId),
    Return(u64),
}

fn interpret_block(
    engine: &mut Engine,
    vm: &mut Vm,
    program: &Program,
    func: FuncId,
    block: BlockId,
) -> BlockOutcome {
    let func_ref = &program.functions[func];
    let blk = &func_ref.blocks[block];

    for instr in &blk.instrs {
        exec_instr(engine, vm, program, func, instr);
    }

    exec_term(engine, vm, func, block, &blk.term)
}

fn exec_term(
    engine: &mut Engine,
    vm: &mut Vm,
    func: FuncId,
    block: BlockId,
    term: &Terminator,
) -> BlockOutcome {
    match *term {
        Terminator::Jmp(tgt) => BlockOutcome::Next(tgt),
        Terminator::Br {
            cond,
            then_tgt,
            else_tgt,
        } => {
            let taken_then = cond.eval(vm.flags);
            engine.record_branch(func, block, cond, taken_then);
            BlockOutcome::Next(if taken_then { then_tgt } else { else_tgt })
        }
        Terminator::Ret { src } => BlockOutcome::Return(vm.gprs[src.0 as usize]),
    }
}

fn exec_instr(engine: &mut Engine, vm: &mut Vm, program: &Program, func: FuncId, instr: &Instr) {
    match instr {
        Instr::Imm { dst, imm } => vm.gprs[dst.0 as usize] = *imm,
        Instr::Mov { dst, src } => vm.gprs[dst.0 as usize] = vm.gprs[src.0 as usize],
        Instr::Add { dst, a, b } => {
            let aa = vm.gprs[a.0 as usize];
            let bb = vm.gprs[b.0 as usize];
            let rr = aa.wrapping_add(bb);
            vm.gprs[dst.0 as usize] = rr;
            set_flags_add(rr, aa, bb, FlagMask::ALL, &mut vm.flags);
        }
        Instr::Sub { dst, a, b } => {
            let aa = vm.gprs[a.0 as usize];
            let bb = vm.gprs[b.0 as usize];
            let rr = aa.wrapping_sub(bb);
            vm.gprs[dst.0 as usize] = rr;
            set_flags_sub(rr, aa, bb, FlagMask::ALL, &mut vm.flags);
        }
        Instr::Mul { dst, a, b } => {
            let aa = vm.gprs[a.0 as usize];
            let bb = vm.gprs[b.0 as usize];
            let rr = aa.wrapping_mul(bb);
            vm.gprs[dst.0 as usize] = rr;
            set_flags_logic(rr, FlagMask::ALL, &mut vm.flags);
        }
        Instr::Shl { dst, src, shift } => {
            let aa = vm.gprs[src.0 as usize];
            let rr = aa.wrapping_shl(*shift as u32);
            vm.gprs[dst.0 as usize] = rr;
            set_flags_logic(rr, FlagMask::ALL, &mut vm.flags);
        }
        Instr::Cmp { a, b } => {
            let aa = vm.gprs[a.0 as usize];
            let bb = vm.gprs[b.0 as usize];
            let rr = aa.wrapping_sub(bb);
            set_flags_sub(rr, aa, bb, FlagMask::ALL, &mut vm.flags);
        }
        Instr::Load { dst, base, offset } => {
            let addr = vm.gprs[base.0 as usize].wrapping_add(*offset as i64 as u64);
            let addr_usize = addr as usize;
            if addr_usize + 8 > vm.mem.len() {
                panic!("OOB load_u64 at {addr:#x}");
            }
            vm.gprs[dst.0 as usize] = vm.mem.load_u64(addr);
        }
        Instr::Store { base, offset, src } => {
            let addr = vm.gprs[base.0 as usize].wrapping_add(*offset as i64 as u64);
            let addr_usize = addr as usize;
            if addr_usize + 8 > vm.mem.len() {
                panic!("OOB store_u64 at {addr:#x}");
            }
            let val = vm.gprs[src.0 as usize];
            vm.mem.store_u64(addr, val);
        }
        Instr::VImm { dst, imm } => vm.xmms[dst.0 as usize] = *imm,
        Instr::VAddF32x4 { dst, a, b } => {
            let aa = vm.xmms[a.0 as usize];
            let bb = vm.xmms[b.0 as usize];
            vm.xmms[dst.0 as usize] = crate::microvm::simd_f32x4_add(aa, bb);
        }
        Instr::VMulF32x4 { dst, a, b } => {
            let aa = vm.xmms[a.0 as usize];
            let bb = vm.xmms[b.0 as usize];
            vm.xmms[dst.0 as usize] = crate::microvm::simd_f32x4_mul(aa, bb);
        }
        Instr::Call {
            dst,
            func: callee,
            args,
        } => {
            engine.record_call(func, *callee);
            let ret = call_function(engine, vm, program, *callee, args);
            vm.gprs[dst.0 as usize] = ret;
        }
    }
}

fn call_function(
    engine: &mut Engine,
    vm: &mut Vm,
    program: &Program,
    callee: FuncId,
    args: &[crate::microvm::Gpr],
) -> u64 {
    let callee_func = &program.functions[callee];
    let mut new_vm = Vm::new(callee_func.gpr_count, callee_func.xmm_count, vm.mem.len());
    // Share memory with the caller (swap in the callee's temporary Memory object).
    std::mem::swap(&mut new_vm.mem, &mut vm.mem);
    // Move args into callee regs 0..n.
    for (i, arg) in args.iter().enumerate() {
        if i < new_vm.gprs.len() {
            new_vm.gprs[i] = vm.gprs[arg.0 as usize];
        }
    }

    let ret = engine.run(&mut new_vm, program, callee);
    // Restore memory.
    std::mem::swap(&mut new_vm.mem, &mut vm.mem);
    ret
}

// -----------------------------------------------------------------------------
// Tier-1 baseline block compiler
// -----------------------------------------------------------------------------

#[derive(Clone)]
struct Tier1Block {
    ops: Vec<Tier1Op>,
    term: Terminator,
}

#[derive(Clone)]
enum Tier1Op {
    Imm {
        dst: u16,
        imm: u64,
    },
    Mov {
        dst: u16,
        src: u16,
    },
    Add {
        dst: u16,
        a: u16,
        b: u16,
    },
    Sub {
        dst: u16,
        a: u16,
        b: u16,
    },
    Mul {
        dst: u16,
        a: u16,
        b: u16,
    },
    Shl {
        dst: u16,
        src: u16,
        shift: u8,
    },
    Cmp {
        a: u16,
        b: u16,
    },
    Load {
        dst: u16,
        base: u16,
        offset: i32,
    },
    Store {
        base: u16,
        offset: i32,
        src: u16,
    },
    VImm {
        dst: u16,
        imm: u128,
    },
    VAddF32x4 {
        dst: u16,
        a: u16,
        b: u16,
    },
    VMulF32x4 {
        dst: u16,
        a: u16,
        b: u16,
    },
    Call {
        dst: u16,
        func: FuncId,
        args: Vec<u16>,
    },
}

impl Tier1Block {
    fn compile(blk: &crate::microvm::Block) -> Self {
        let mut ops = Vec::new();
        for instr in &blk.instrs {
            match instr {
                Instr::Imm { dst, imm } => ops.push(Tier1Op::Imm {
                    dst: dst.0,
                    imm: *imm,
                }),
                Instr::Mov { dst, src } => ops.push(Tier1Op::Mov {
                    dst: dst.0,
                    src: src.0,
                }),
                Instr::Add { dst, a, b } => ops.push(Tier1Op::Add {
                    dst: dst.0,
                    a: a.0,
                    b: b.0,
                }),
                Instr::Sub { dst, a, b } => ops.push(Tier1Op::Sub {
                    dst: dst.0,
                    a: a.0,
                    b: b.0,
                }),
                Instr::Mul { dst, a, b } => ops.push(Tier1Op::Mul {
                    dst: dst.0,
                    a: a.0,
                    b: b.0,
                }),
                Instr::Shl { dst, src, shift } => ops.push(Tier1Op::Shl {
                    dst: dst.0,
                    src: src.0,
                    shift: *shift,
                }),
                Instr::Cmp { a, b } => ops.push(Tier1Op::Cmp { a: a.0, b: b.0 }),
                Instr::Load { dst, base, offset } => ops.push(Tier1Op::Load {
                    dst: dst.0,
                    base: base.0,
                    offset: *offset,
                }),
                Instr::Store { base, offset, src } => ops.push(Tier1Op::Store {
                    base: base.0,
                    offset: *offset,
                    src: src.0,
                }),
                Instr::VImm { dst, imm } => ops.push(Tier1Op::VImm {
                    dst: dst.0,
                    imm: *imm,
                }),
                Instr::VAddF32x4 { dst, a, b } => ops.push(Tier1Op::VAddF32x4 {
                    dst: dst.0,
                    a: a.0,
                    b: b.0,
                }),
                Instr::VMulF32x4 { dst, a, b } => ops.push(Tier1Op::VMulF32x4 {
                    dst: dst.0,
                    a: a.0,
                    b: b.0,
                }),
                Instr::Call { dst, func, args } => ops.push(Tier1Op::Call {
                    dst: dst.0,
                    func: *func,
                    args: args.iter().map(|r| r.0).collect(),
                }),
            }
        }
        Self {
            ops,
            term: blk.term.clone(),
        }
    }

    fn estimated_size_bytes(&self) -> u64 {
        use std::mem::size_of;

        let mut total =
            size_of::<Self>() as u64 + (self.ops.len() as u64) * (size_of::<Tier1Op>() as u64);

        for op in &self.ops {
            if let Tier1Op::Call { args, .. } = op {
                total = total.saturating_add((args.len() as u64) * (size_of::<u16>() as u64));
            }
        }

        total
    }

    fn execute(
        &self,
        engine: &mut Engine,
        vm: &mut Vm,
        program: &Program,
        func: FuncId,
        block_id: BlockId,
    ) -> BlockOutcome {
        // Baseline strategy: copy the whole machine state into locals for this
        // block, then copy back. This is representative of a "baseline block"
        // compiled as a standalone function.
        let mut gprs = vm.gprs.clone();
        let mut xmms = vm.xmms.clone();
        let mut flags = vm.flags;

        for op in &self.ops {
            match op {
                Tier1Op::Imm { dst, imm } => gprs[*dst as usize] = *imm,
                Tier1Op::Mov { dst, src } => gprs[*dst as usize] = gprs[*src as usize],
                Tier1Op::Add { dst, a, b } => {
                    let aa = gprs[*a as usize];
                    let bb = gprs[*b as usize];
                    let rr = aa.wrapping_add(bb);
                    gprs[*dst as usize] = rr;
                    set_flags_add(rr, aa, bb, FlagMask::ALL, &mut flags);
                }
                Tier1Op::Sub { dst, a, b } => {
                    let aa = gprs[*a as usize];
                    let bb = gprs[*b as usize];
                    let rr = aa.wrapping_sub(bb);
                    gprs[*dst as usize] = rr;
                    set_flags_sub(rr, aa, bb, FlagMask::ALL, &mut flags);
                }
                Tier1Op::Mul { dst, a, b } => {
                    let aa = gprs[*a as usize];
                    let bb = gprs[*b as usize];
                    let rr = aa.wrapping_mul(bb);
                    gprs[*dst as usize] = rr;
                    set_flags_logic(rr, FlagMask::ALL, &mut flags);
                }
                Tier1Op::Shl { dst, src, shift } => {
                    let aa = gprs[*src as usize];
                    let rr = aa.wrapping_shl(*shift as u32);
                    gprs[*dst as usize] = rr;
                    set_flags_logic(rr, FlagMask::ALL, &mut flags);
                }
                Tier1Op::Cmp { a, b } => {
                    let aa = gprs[*a as usize];
                    let bb = gprs[*b as usize];
                    let rr = aa.wrapping_sub(bb);
                    set_flags_sub(rr, aa, bb, FlagMask::ALL, &mut flags);
                }
                Tier1Op::Load { dst, base, offset } => {
                    let addr = gprs[*base as usize].wrapping_add(*offset as i64 as u64);
                    let addr_usize = addr as usize;
                    if addr_usize + 8 > vm.mem.len() {
                        panic!("OOB load_u64 at {addr:#x}");
                    }
                    gprs[*dst as usize] = vm.mem.load_u64(addr);
                }
                Tier1Op::Store { base, offset, src } => {
                    let addr = gprs[*base as usize].wrapping_add(*offset as i64 as u64);
                    let addr_usize = addr as usize;
                    if addr_usize + 8 > vm.mem.len() {
                        panic!("OOB store_u64 at {addr:#x}");
                    }
                    let val = gprs[*src as usize];
                    vm.mem.store_u64(addr, val);
                }
                Tier1Op::VImm { dst, imm } => xmms[*dst as usize] = *imm,
                Tier1Op::VAddF32x4 { dst, a, b } => {
                    xmms[*dst as usize] =
                        crate::microvm::simd_f32x4_add(xmms[*a as usize], xmms[*b as usize]);
                }
                Tier1Op::VMulF32x4 { dst, a, b } => {
                    xmms[*dst as usize] =
                        crate::microvm::simd_f32x4_mul(xmms[*a as usize], xmms[*b as usize]);
                }
                Tier1Op::Call {
                    dst,
                    func: callee,
                    args,
                } => {
                    engine.record_call(func, *callee);
                    let arg_regs: Vec<crate::microvm::Gpr> =
                        args.iter().map(|r| crate::microvm::Gpr(*r)).collect();
                    // Flush locals to vm before call.
                    vm.gprs.clone_from(&gprs);
                    vm.xmms.clone_from(&xmms);
                    vm.flags = flags;
                    let ret = call_function(engine, vm, program, *callee, &arg_regs);
                    // Reload locals after call.
                    gprs.clone_from(&vm.gprs);
                    xmms.clone_from(&vm.xmms);
                    flags = vm.flags;
                    gprs[*dst as usize] = ret;
                }
            }
        }

        // Commit locals back to VM state.
        vm.gprs = gprs;
        vm.xmms = xmms;
        vm.flags = flags;

        exec_term(engine, vm, func, block_id, &self.term)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::microvm::{Block, Function, Gpr, Program, Terminator, Vm, Xmm};

    fn build_loop_program() -> Program {
        // Registers:
        // r0 = loop counter (arg)
        // r1 = accumulator
        // r2 = base pointer (0)
        // r3 = zero (0)
        // r4 = constant 8 (reloaded each iter to enable local strength reduction)
        // r5 = temp
        // r6 = temp
        // r7 = constant 1 (reloaded each iter)
        let r0 = Gpr(0);
        let r1 = Gpr(1);
        let r2 = Gpr(2);
        let r3 = Gpr(3);
        let r4 = Gpr(4);
        let r5 = Gpr(5);
        let r6 = Gpr(6);
        let r7 = Gpr(7);

        let x0 = Xmm(0);
        let x1 = Xmm(1);

        let entry = Block {
            instrs: vec![
                Instr::Imm { dst: r1, imm: 0 },
                Instr::Imm { dst: r2, imm: 0 },
                Instr::Imm { dst: r3, imm: 0 },
                Instr::VImm {
                    dst: x0,
                    imm: 0x3f8000003f8000003f8000003f800000,
                }, // 1.0 lanes
                Instr::VImm {
                    dst: x1,
                    imm: 0x40000000400000004000000040000000,
                }, // 2.0 lanes
            ],
            term: Terminator::Jmp(1),
        };

        let header = Block {
            instrs: vec![Instr::Cmp { a: r0, b: r3 }],
            term: Terminator::Br {
                cond: Cond::Zero,
                then_tgt: 3,
                else_tgt: 2,
            },
        };

        let body = Block {
            instrs: vec![
                Instr::Imm { dst: r4, imm: 8 },
                Instr::Mul {
                    dst: r5,
                    a: r1,
                    b: r4,
                },
                Instr::Mul {
                    dst: r6,
                    a: r1,
                    b: r4,
                }, // redundant
                Instr::Add {
                    dst: r1,
                    a: r5,
                    b: r6,
                },
                Instr::Load {
                    dst: r5,
                    base: r2,
                    offset: 16,
                },
                Instr::Add {
                    dst: r1,
                    a: r1,
                    b: r5,
                },
                Instr::Store {
                    base: r2,
                    offset: 24,
                    src: r1,
                },
                Instr::Imm { dst: r7, imm: 1 },
                Instr::Sub {
                    dst: r0,
                    a: r0,
                    b: r7,
                },
                Instr::VAddF32x4 {
                    dst: x0,
                    a: x0,
                    b: x1,
                },
                Instr::VMulF32x4 {
                    dst: x0,
                    a: x0,
                    b: x1,
                },
            ],
            term: Terminator::Jmp(1),
        };

        let exit = Block {
            instrs: vec![],
            term: Terminator::Ret { src: r1 },
        };

        Program {
            functions: vec![Function {
                entry: 0,
                blocks: vec![entry, header, body, exit],
                gpr_count: 8,
                xmm_count: 2,
            }],
        }
    }

    fn init_vm(loop_count: u64) -> Vm {
        let mut vm = Vm::new(8, 2, 4096);
        vm.gprs[0] = loop_count;
        vm.mem.store_u64(16, 1);
        vm
    }

    #[test]
    fn tier2_triggers_and_matches_interpreter() {
        let program = build_loop_program();

        // Interpreter-only baseline.
        let mut vm_ref = init_vm(50);
        let mut interp = Engine::new(
            &program,
            JitConfig {
                tier1_threshold: u64::MAX,
                tier2_threshold: u64::MAX,
                ..JitConfig::default()
            },
        );
        let ref_ret = interp.run(&mut vm_ref, &program, 0);

        // Tiered execution.
        let mut vm_opt = init_vm(50);
        let mut engine = Engine::new(
            &program,
            JitConfig {
                tier1_threshold: 1,
                tier2_threshold: 5,
                ..JitConfig::default()
            },
        );
        let opt_ret = engine.run(&mut vm_opt, &program, 0);

        assert_eq!(opt_ret, ref_ret);
        assert_eq!(vm_opt.gprs, vm_ref.gprs);
        assert_eq!(vm_opt.xmms, vm_ref.xmms);
        assert_eq!(vm_opt.flags, vm_ref.flags);
        assert_eq!(vm_opt.mem.load_u64(24), vm_ref.mem.load_u64(24));

        let stats = engine.stats();
        assert!(
            stats.tier2_regions_compiled > 0,
            "tier2 should compile at least one region"
        );
        assert!(
            stats.tier2_exec_count > 0,
            "tier2 should execute at least once"
        );
    }

    #[test]
    fn tier2_deopts_on_permission_epoch_change() {
        let program = build_loop_program();
        let mut vm = init_vm(10);
        let mut engine = Engine::new(
            &program,
            JitConfig {
                tier1_threshold: 1,
                tier2_threshold: 1,
                ..JitConfig::default()
            },
        );

        let ret1 = engine.run(&mut vm, &program, 0);
        let compiled = engine.stats().tier2_regions_compiled;
        assert!(compiled > 0);

        // Invalidate assumptions (simulated page permission change).
        let mut vm = init_vm(10);
        vm.mem.set_page_executable(0, true);

        let ret2 = engine.run(&mut vm, &program, 0);
        assert_eq!(ret2, ret1);
        assert!(engine.stats().tier2_deopt_count > 0);
    }

    #[test]
    fn tier2_emits_wasm_simd_ops() {
        let program = build_loop_program();
        let mut vm = init_vm(5);
        let mut engine = Engine::new(
            &program,
            JitConfig {
                tier1_threshold: 1,
                tier2_threshold: 1,
                ..JitConfig::default()
            },
        );
        let _ = engine.run(&mut vm, &program, 0);
        let mut saw_add = false;
        let mut saw_mul = false;
        for region in engine.tier2_regions.values() {
            let listing = region.wasm_simd_listing();
            saw_add |= listing
                .iter()
                .any(|i| matches!(i, crate::opt::WasmInst::F32x4Add));
            saw_mul |= listing
                .iter()
                .any(|i| matches!(i, crate::opt::WasmInst::F32x4Mul));
        }
        assert!(
            saw_add,
            "expected at least one tier2 region to lower f32x4.add"
        );
        assert!(
            saw_mul,
            "expected at least one tier2 region to lower f32x4.mul"
        );
    }
}
