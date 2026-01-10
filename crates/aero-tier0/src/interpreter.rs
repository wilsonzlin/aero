mod legacy;
mod tier0;

pub use legacy::LegacyInterpreter;
pub use tier0::Tier0Interpreter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exception {
    DecodeError { rip: u64 },
    InvalidOpcode,
    MemFault { addr: u64 },
    Interrupt(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Continue,
    Jump(u64),
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    Halt,
    Interrupt(u8),
    InstructionLimit,
}

pub(crate) fn post_instruction_check(cpu: &mut crate::cpu::CpuState) -> Option<ExitReason> {
    if cpu.interrupt_shadow != 0 {
        cpu.interrupt_shadow = cpu.interrupt_shadow.saturating_sub(1);
        // If the shadow is still active after decrement, interrupts remain blocked.
        if cpu.interrupt_shadow != 0 {
            return None;
        }
    }

    if cpu.flags.iflag {
        if let Some(vector) = cpu.pending_interrupt.take() {
            return Some(ExitReason::Interrupt(vector));
        }
    }
    None
}
