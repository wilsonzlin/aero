use crate::jit::runtime::{CompileRequestSink, JitBackend, JitBlockExit, JitRuntime};

pub trait ExecCpu {
    fn rip(&self) -> u64;
    fn set_rip(&mut self, rip: u64);
    fn maybe_deliver_interrupt(&mut self) -> bool;
}

pub trait Interpreter<Cpu: ExecCpu> {
    fn exec_block(&mut self, cpu: &mut Cpu) -> u64;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutedTier {
    Interpreter,
    Jit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    InterruptDelivered,
    Block {
        tier: ExecutedTier,
        entry_rip: u64,
        next_rip: u64,
    },
}

pub struct ExecDispatcher<I, B, C> {
    interpreter: I,
    jit: JitRuntime<B, C>,
    force_interpreter: bool,
}

impl<I, B, C> ExecDispatcher<I, B, C>
where
    B: JitBackend,
    B::Cpu: ExecCpu,
    I: Interpreter<B::Cpu>,
    C: CompileRequestSink,
{
    pub fn new(interpreter: I, jit: JitRuntime<B, C>) -> Self {
        Self {
            interpreter,
            jit,
            force_interpreter: false,
        }
    }

    pub fn jit_mut(&mut self) -> &mut JitRuntime<B, C> {
        &mut self.jit
    }

    pub fn step(&mut self, cpu: &mut B::Cpu) -> StepOutcome {
        if cpu.maybe_deliver_interrupt() {
            return StepOutcome::InterruptDelivered;
        }

        let entry_rip = cpu.rip();
        let compiled = self.jit.prepare_block(entry_rip);

        if self.force_interpreter || compiled.is_none() {
            let next_rip = self.interpreter.exec_block(cpu);
            cpu.set_rip(next_rip);
            self.force_interpreter = false;
            return StepOutcome::Block {
                tier: ExecutedTier::Interpreter,
                entry_rip,
                next_rip,
            };
        }

        let handle = compiled.expect("checked is_some above");
        let exit: JitBlockExit = self.jit.execute_block(cpu, &handle);
        cpu.set_rip(exit.next_rip);
        self.force_interpreter = exit.exit_to_interpreter;

        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            entry_rip,
            next_rip: exit.next_rip,
        }
    }

    pub fn run_blocks(&mut self, cpu: &mut B::Cpu, mut blocks: u64) {
        while blocks > 0 {
            match self.step(cpu) {
                StepOutcome::InterruptDelivered => continue,
                StepOutcome::Block { .. } => blocks -= 1,
            }
        }
    }
}
