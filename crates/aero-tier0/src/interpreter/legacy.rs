use std::marker::PhantomData;

use crate::bus::CpuBus;
use crate::cpu::CpuState;
use crate::decoder::Decoder;
use crate::dispatch::{self, Handler, OpcodeKind};
use crate::interpreter::{post_instruction_check, Exception, ExitReason, Next};

pub struct LegacyInterpreter {
    pub cpu: CpuState,
}

impl LegacyInterpreter {
    pub fn new(cpu: CpuState) -> Self {
        Self { cpu }
    }

    pub fn run<B: CpuBus>(
        &mut self,
        bus: &mut B,
        instruction_limit: u64,
    ) -> Result<ExitReason, Exception> {
        for _ in 0..instruction_limit {
            let inst = Decoder::decode_inst(bus, self.cpu.rip)?;
            let next = match self.execute_inst(bus, &inst) {
                Err(Exception::Interrupt(vector)) => return Ok(ExitReason::Interrupt(vector)),
                other => other?,
            };

            if let Some(exit) = self.apply_next(&inst, next) {
                return Ok(exit);
            }

            if let Some(exit) = post_instruction_check(&mut self.cpu) {
                return Ok(exit);
            }
        }
        Ok(ExitReason::InstructionLimit)
    }

    fn execute_inst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        inst: &dispatch::DecodedInst,
    ) -> Result<Next, Exception> {
        let handler = LegacyDispatchTable::<B>::TABLE[inst.opcode as usize];
        handler(&mut self.cpu, bus, inst)
    }

    fn apply_next(&mut self, inst: &dispatch::DecodedInst, next: Next) -> Option<ExitReason> {
        match next {
            Next::Continue => {
                self.cpu.rip = self.cpu.rip.wrapping_add(inst.len as u64);
                None
            }
            Next::Jump(target) => {
                self.cpu.rip = target;
                None
            }
            Next::Exit => {
                self.cpu.rip = self.cpu.rip.wrapping_add(inst.len as u64);
                Some(ExitReason::Halt)
            }
        }
    }
}

struct LegacyDispatchTable<B: CpuBus>(PhantomData<B>);

impl<B: CpuBus> LegacyDispatchTable<B> {
    const TABLE: [Handler<B>; OpcodeKind::COUNT] = [
        dispatch::op_invalid::<B>,
        dispatch::op_nop::<B>,
        dispatch::op_hlt::<B>,
        dispatch::op_dec_reg::<B>,
        dispatch::op_jnz_rel::<B>,
        dispatch::op_rep_movsb_slow::<B>,
        dispatch::op_sti::<B>,
        dispatch::op_mov_ss_ax::<B>,
    ];
}
