use std::collections::HashSet;

use crate::profile::{ProfileData, TraceConfig};
use crate::t2_ir::{BlockId, Function, Instr, TraceIr, TraceKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideExit {
    pub from_block: BlockId,
    pub to_block: BlockId,
    pub next_rip: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    pub entry_block: BlockId,
    pub blocks: Vec<BlockId>,
    pub ir: TraceIr,
    pub side_exits: Vec<SideExit>,
}

pub struct TraceBuilder<'a> {
    func: &'a Function,
    profile: &'a ProfileData,
    cfg: TraceConfig,
}

impl<'a> TraceBuilder<'a> {
    pub fn new(func: &'a Function, profile: &'a ProfileData, cfg: TraceConfig) -> Self {
        Self { func, profile, cfg }
    }

    pub fn build_from(&self, entry_block: BlockId) -> Option<Trace> {
        if self.profile.block_count(entry_block) < self.cfg.hot_block_threshold {
            return None;
        }

        let entry_rip = self.func.block(entry_block).start_rip;
        let entry_page = entry_rip >> 12;
        let expected_version = self.profile.code_page_version(entry_page);

        let mut trace = Trace {
            entry_block,
            blocks: Vec::new(),
            ir: TraceIr {
                prologue: vec![Instr::GuardCodeVersion {
                    page: entry_page,
                    expected: expected_version,
                    exit_rip: entry_rip,
                }],
                body: Vec::new(),
                kind: TraceKind::Linear,
            },
            side_exits: Vec::new(),
        };

        let mut visited: HashSet<BlockId> = HashSet::new();
        let mut cur = entry_block;
        let mut instr_budget = self.cfg.max_instrs;

        while trace.blocks.len() < self.cfg.max_blocks && instr_budget > 0 {
            if !visited.insert(cur) {
                break;
            }

            trace.blocks.push(cur);
            let block = self.func.block(cur);

            for inst in &block.instrs {
                if instr_budget == 0 {
                    break;
                }
                trace.ir.body.push(inst.clone());
                instr_budget -= 1;
                if inst.is_terminator() {
                    return Some(trace);
                }
            }

            match &block.term {
                crate::t2_ir::Terminator::Return => {
                    trace.ir.kind = TraceKind::Linear;
                    break;
                }
                crate::t2_ir::Terminator::SideExit { exit_rip } => {
                    // Side exits are trace terminators: the trace must return the correct next RIP.
                    if instr_budget == 0 {
                        break;
                    }
                    trace.ir.body.push(Instr::SideExit { exit_rip: *exit_rip });
                    return Some(trace);
                }
                crate::t2_ir::Terminator::Jump(t) => {
                    if *t == entry_block && self.profile.is_hot_backedge(cur, *t) {
                        trace.ir.kind = TraceKind::Loop;
                        break;
                    }
                    if visited.contains(t) {
                        break;
                    }
                    cur = *t;
                }
                crate::t2_ir::Terminator::Branch {
                    cond,
                    then_bb,
                    else_bb,
                } => {
                    let then_count = self.profile.edge_count(cur, *then_bb);
                    let else_count = self.profile.edge_count(cur, *else_bb);

                    let (hot, cold, expected) = if then_count >= else_count {
                        (*then_bb, *else_bb, true)
                    } else {
                        (*else_bb, *then_bb, false)
                    };

                    let cold_rip = self.func.block(cold).start_rip;
                    trace.side_exits.push(SideExit {
                        from_block: cur,
                        to_block: cold,
                        next_rip: cold_rip,
                    });

                    if instr_budget == 0 {
                        break;
                    }
                    trace.ir.body.push(Instr::Guard {
                        cond: *cond,
                        expected,
                        exit_rip: cold_rip,
                    });
                    instr_budget -= 1;

                    if hot == entry_block && self.profile.is_hot_backedge(cur, hot) {
                        trace.ir.kind = TraceKind::Loop;
                        break;
                    }
                    if visited.contains(&hot) {
                        break;
                    }
                    cur = hot;
                }
            }
        }

        Some(trace)
    }
}

/// Build traces for hot blocks, in descending hotness order.
pub fn build_hot_traces(func: &Function, profile: &ProfileData, cfg: TraceConfig) -> Vec<Trace> {
    let mut hot: Vec<(BlockId, u64)> = func
        .blocks
        .iter()
        .map(|b| (b.id, profile.block_count(b.id)))
        .filter(|(_, c)| *c >= cfg.hot_block_threshold)
        .collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1));

    let builder = TraceBuilder::new(func, profile, cfg);
    hot.into_iter()
        .filter_map(|(b, _)| builder.build_from(b))
        .collect()
}
