use std::marker::PhantomData;
use std::rc::Rc;

use rustc_hash::FxHashMap;

use crate::bus::CpuBus;
use crate::cpu::CpuState;
use crate::decoder::{DecodedBlock, Decoder};
use crate::dispatch::{self, Handler, OpcodeKind};
use crate::interpreter::{post_instruction_check, Exception, ExitReason, Next};

const MAX_BLOCK_INSTS: usize = 256;

pub struct Tier0Interpreter {
    pub cpu: CpuState,
    cache: FxHashMap<u64, Rc<DecodedBlock>>,
    last_block: Option<(u64, Rc<DecodedBlock>)>,
}

impl Tier0Interpreter {
    pub fn new(cpu: CpuState) -> Self {
        Self {
            cpu,
            cache: FxHashMap::default(),
            last_block: None,
        }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.last_block = None;
    }

    fn ensure_last_block<B: CpuBus>(&mut self, bus: &B, rip: u64) -> Result<(), Exception> {
        if let Some((last_rip, last_block)) = &self.last_block {
            if *last_rip == rip && last_block.is_still_valid(bus) {
                return Ok(());
            }
        }

        if let Some(block) = self.cache.get(&rip) {
            if block.is_still_valid(bus) {
                self.last_block = Some((rip, Rc::clone(block)));
                return Ok(());
            }
        }

        // Stale or missing: (re)decode.
        self.cache.remove(&rip);
        let block = Rc::new(Decoder::decode_block(bus, rip, MAX_BLOCK_INSTS)?);
        self.cache.insert(rip, Rc::clone(&block));
        self.last_block = Some((rip, block));
        Ok(())
    }

    pub fn run<B: CpuBus>(
        &mut self,
        bus: &mut B,
        instruction_limit: u64,
    ) -> Result<ExitReason, Exception> {
        let mut executed = 0u64;
        while executed < instruction_limit {
            let start_rip = self.cpu.rip;
            self.ensure_last_block(bus, start_rip)?;
            let block = self
                .last_block
                .as_ref()
                .ok_or(Exception::DecodeError { rip: start_rip })?;
            let block = block.1.as_ref();

            for inst in &block.insts {
                let handler = DispatchTable::<B>::TABLE[inst.opcode as usize];
                let next = match handler(&mut self.cpu, bus, inst) {
                    Err(Exception::Interrupt(vector)) => return Ok(ExitReason::Interrupt(vector)),
                    other => other?,
                };

                if let Some(exit) = apply_next(&mut self.cpu, inst, next) {
                    return Ok(exit);
                }
                executed += 1;

                if let Some(exit) = post_instruction_check(&mut self.cpu) {
                    return Ok(exit);
                }

                if matches!(next, Next::Jump(_) | Next::Exit) {
                    break;
                }
            }
        }

        Ok(ExitReason::InstructionLimit)
    }
}

fn apply_next(cpu: &mut CpuState, inst: &dispatch::DecodedInst, next: Next) -> Option<ExitReason> {
    match next {
        Next::Continue => {
            cpu.rip = cpu.rip.wrapping_add(inst.len as u64);
            None
        }
        Next::Jump(target) => {
            cpu.rip = target;
            None
        }
        Next::Exit => {
            cpu.rip = cpu.rip.wrapping_add(inst.len as u64);
            Some(ExitReason::Halt)
        }
    }
}

struct DispatchTable<B: CpuBus>(PhantomData<B>);

impl<B: CpuBus> DispatchTable<B> {
    const TABLE: [Handler<B>; OpcodeKind::COUNT] = [
        dispatch::op_invalid::<B>,
        dispatch::op_nop::<B>,
        dispatch::op_hlt::<B>,
        dispatch::op_dec_reg::<B>,
        dispatch::op_jnz_rel::<B>,
        dispatch::op_rep_movsb_fast::<B>,
        dispatch::op_sti::<B>,
        dispatch::op_mov_ss_ax::<B>,
    ];
}
