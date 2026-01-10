use std::collections::HashMap;

use crate::t2_ir::{
    eval_binop, Flag, FlagMask, FlagValues, Function, Instr, Operand, TraceIr, TraceKind, REG_COUNT,
};
use crate::CpuState;
use crate::Reg;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub zf: bool,
    pub sf: bool,
    pub cf: bool,
    pub of: bool,
}

impl Flags {
    pub fn get(&self, flag: Flag) -> bool {
        match flag {
            Flag::Zf => self.zf,
            Flag::Sf => self.sf,
            Flag::Cf => self.cf,
            Flag::Of => self.of,
        }
    }

    pub fn apply_mask(&mut self, mask: FlagMask, values: FlagValues) {
        if mask.intersects(FlagMask::ZF) {
            self.zf = values.zf;
        }
        if mask.intersects(FlagMask::SF) {
            self.sf = values.sf;
        }
        if mask.intersects(FlagMask::CF) {
            self.cf = values.cf;
        }
        if mask.intersects(FlagMask::OF) {
            self.of = values.of;
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct T2State {
    pub cpu: CpuState,
    pub flags: Flags,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeEnv {
    /// Current code page versions (self-modifying code invalidation).
    pub code_page_versions: HashMap<u64, u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecStats {
    pub reg_loads: u64,
    pub reg_stores: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunExit {
    Returned,
    SideExit { next_rip: u64 },
    StepLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    pub exit: RunExit,
    pub stats: ExecStats,
}

fn max_value_id_in_instrs<'a>(instrs: impl Iterator<Item = &'a Instr>) -> usize {
    let mut max_id: Option<u32> = None;
    for inst in instrs {
        if let Some(dst) = inst.dst() {
            max_id = Some(max_id.map_or(dst.0, |cur| cur.max(dst.0)));
        }
        inst.for_each_operand(|op| {
            if let Operand::Value(v) = op {
                max_id = Some(max_id.map_or(v.0, |cur| cur.max(v.0)));
            }
        });
    }
    max_id.map_or(0, |v| v as usize + 1)
}

fn eval_operand(op: Operand, values: &[u64]) -> u64 {
    match op {
        Operand::Const(v) => v,
        Operand::Value(id) => values[id.index()],
    }
}

fn exec_instr(
    inst: &Instr,
    state: &mut T2State,
    env: &RuntimeEnv,
    values: &mut [u64],
    stats: &mut ExecStats,
    reg_cache: Option<&mut RegCache>,
) -> Option<RunExit> {
    match *inst {
        Instr::Nop => {}
        Instr::Const { dst, value } => values[dst.index()] = value,
        Instr::LoadReg { dst, reg } => {
            let val = if let Some(cache) = reg_cache {
                cache.read_reg(reg, &state.cpu, stats)
            } else {
                stats.reg_loads += 1;
                state.cpu.get_reg(reg)
            };
            values[dst.index()] = val;
        }
        Instr::StoreReg { reg, src } => {
            let val = eval_operand(src, values);
            if let Some(cache) = reg_cache {
                cache.write_reg(reg, val, &mut state.cpu, stats);
            } else {
                stats.reg_stores += 1;
                state.cpu.set_reg(reg, val);
            }
        }
        Instr::LoadFlag { dst, flag } => {
            values[dst.index()] = state.flags.get(flag) as u64;
        }
        Instr::SetFlags { mask, values: fv } => {
            state.flags.apply_mask(mask, fv);
        }
        Instr::BinOp {
            dst,
            op,
            lhs,
            rhs,
            flags,
        } => {
            let lhs = eval_operand(lhs, values);
            let rhs = eval_operand(rhs, values);
            let (res, computed) = eval_binop(op, lhs, rhs);
            values[dst.index()] = res;
            if !flags.is_empty() {
                state.flags.apply_mask(flags, computed);
            }
        }
        Instr::Addr {
            dst,
            base,
            index,
            scale,
            disp,
        } => {
            let base = eval_operand(base, values);
            let index = eval_operand(index, values);
            let addr = base
                .wrapping_add(index.wrapping_mul(scale as u64))
                .wrapping_add(disp as u64);
            values[dst.index()] = addr;
        }
        Instr::Guard {
            cond,
            expected,
            exit_rip,
        } => {
            let cond = eval_operand(cond, values) != 0;
            if cond != expected {
                if let Some(cache) = reg_cache {
                    cache.spill(&mut state.cpu, stats);
                }
                state.cpu.rip = exit_rip;
                return Some(RunExit::SideExit { next_rip: exit_rip });
            }
        }
        Instr::GuardCodeVersion {
            page,
            expected,
            exit_rip,
        } => {
            let current = env.code_page_versions.get(&page).copied().unwrap_or(0);
            if current != expected {
                if let Some(cache) = reg_cache {
                    cache.spill(&mut state.cpu, stats);
                }
                state.cpu.rip = exit_rip;
                return Some(RunExit::SideExit { next_rip: exit_rip });
            }
        }
        Instr::SideExit { exit_rip } => {
            if let Some(cache) = reg_cache {
                cache.spill(&mut state.cpu, stats);
            }
            state.cpu.rip = exit_rip;
            return Some(RunExit::SideExit { next_rip: exit_rip });
        }
    }
    None
}

pub fn run_function(
    func: &Function,
    env: &RuntimeEnv,
    state: &mut T2State,
    max_steps: usize,
) -> RunExit {
    run_function_from_block(func, env, state, func.entry, max_steps)
}

pub fn run_function_from_block(
    func: &Function,
    env: &RuntimeEnv,
    state: &mut T2State,
    start: crate::t2_ir::BlockId,
    max_steps: usize,
) -> RunExit {
    let mut steps = 0usize;
    let mut cur = start;
    let slots = max_value_id_in_instrs(func.blocks.iter().flat_map(|b| b.instrs.iter()));
    let mut values = vec![0u64; slots.max(1)];
    'outer: loop {
        if steps >= max_steps {
            return RunExit::StepLimit;
        }
        steps += 1;
        let block = func.block(cur);
        state.cpu.rip = block.start_rip;
        let mut dummy_stats = ExecStats::default();
        for inst in &block.instrs {
            if let Some(exit) = exec_instr(inst, state, env, &mut values, &mut dummy_stats, None) {
                match exit {
                    RunExit::SideExit { next_rip } => {
                        if let Some(id) = func.find_block_by_rip(next_rip) {
                            cur = id;
                            continue 'outer;
                        }
                        return exit;
                    }
                    RunExit::Returned | RunExit::StepLimit => return exit,
                }
            }
        }
        match &block.term {
            crate::t2_ir::Terminator::Jump(t) => cur = *t,
            crate::t2_ir::Terminator::Branch {
                cond,
                then_bb,
                else_bb,
            } => {
                let v = eval_operand(*cond, &values);
                cur = if v != 0 { *then_bb } else { *else_bb };
            }
            crate::t2_ir::Terminator::Return => return RunExit::Returned,
        }
    }
}

pub fn run_trace(
    trace: &TraceIr,
    env: &RuntimeEnv,
    state: &mut T2State,
    max_iters: usize,
) -> RunResult {
    run_trace_inner(trace, env, state, max_iters, None)
}

pub fn run_trace_with_cached_regs(
    trace: &TraceIr,
    env: &RuntimeEnv,
    state: &mut T2State,
    max_iters: usize,
    cached_regs: &[bool; REG_COUNT],
) -> RunResult {
    let cache = RegCache::new(*cached_regs);
    run_trace_inner(trace, env, state, max_iters, Some(cache))
}

fn run_trace_inner(
    trace: &TraceIr,
    env: &RuntimeEnv,
    state: &mut T2State,
    max_iters: usize,
    mut cache: Option<RegCache>,
) -> RunResult {
    let slots = max_value_id_in_instrs(trace.iter_instrs());
    let mut values = vec![0u64; slots.max(1)];
    let mut stats = ExecStats::default();

    for inst in &trace.prologue {
        if let Some(exit) = exec_instr(inst, state, env, &mut values, &mut stats, cache.as_mut()) {
            return RunResult { exit, stats };
        }
    }

    let mut iters = 0usize;
    loop {
        if iters >= max_iters {
            if let Some(cache) = cache.as_mut() {
                cache.spill(&mut state.cpu, &mut stats);
            }
            return RunResult {
                exit: RunExit::StepLimit,
                stats,
            };
        }
        iters += 1;
        for inst in &trace.body {
            if let Some(exit) =
                exec_instr(inst, state, env, &mut values, &mut stats, cache.as_mut())
            {
                return RunResult { exit, stats };
            }
        }

        if trace.kind == TraceKind::Linear {
            if let Some(cache) = cache.as_mut() {
                cache.spill(&mut state.cpu, &mut stats);
            }
            return RunResult {
                exit: RunExit::Returned,
                stats,
            };
        }
    }
}

struct RegCache {
    cached: [bool; REG_COUNT],
    locals: [u64; REG_COUNT],
    valid: [bool; REG_COUNT],
    dirty: [bool; REG_COUNT],
}

impl RegCache {
    fn new(cached: [bool; REG_COUNT]) -> Self {
        Self {
            cached,
            locals: [0; REG_COUNT],
            valid: [false; REG_COUNT],
            dirty: [false; REG_COUNT],
        }
    }

    fn read_reg(&mut self, reg: Reg, cpu: &CpuState, stats: &mut ExecStats) -> u64 {
        let idx = reg.index();
        if !self.cached[idx] {
            stats.reg_loads += 1;
            return cpu.get_reg(reg);
        }
        if !self.valid[idx] {
            stats.reg_loads += 1;
            self.locals[idx] = cpu.get_reg(reg);
            self.valid[idx] = true;
        }
        self.locals[idx]
    }

    fn write_reg(&mut self, reg: Reg, value: u64, cpu: &mut CpuState, stats: &mut ExecStats) {
        let idx = reg.index();
        if !self.cached[idx] {
            stats.reg_stores += 1;
            cpu.set_reg(reg, value);
            return;
        }
        self.locals[idx] = value;
        self.valid[idx] = true;
        self.dirty[idx] = true;
    }

    fn spill(&mut self, cpu: &mut CpuState, stats: &mut ExecStats) {
        for reg in all_regs() {
            let idx = reg.index();
            if self.cached[idx] && self.dirty[idx] {
                stats.reg_stores += 1;
                cpu.set_reg(reg, self.locals[idx]);
                self.dirty[idx] = false;
            }
        }
    }
}

fn all_regs() -> [Reg; REG_COUNT] {
    [
        Reg::Rax,
        Reg::Rcx,
        Reg::Rdx,
        Reg::Rbx,
        Reg::Rsp,
        Reg::Rbp,
        Reg::Rsi,
        Reg::Rdi,
        Reg::R8,
        Reg::R9,
        Reg::R10,
        Reg::R11,
        Reg::R12,
        Reg::R13,
        Reg::R14,
        Reg::R15,
    ]
}
