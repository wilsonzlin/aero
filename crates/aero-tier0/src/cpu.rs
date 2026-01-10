/// 16 general purpose registers in the standard x86-64 order.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    pub zf: bool,
    pub iflag: bool,
    pub df: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuState {
    regs: [u64; 16],
    pub rip: u64,
    pub flags: Flags,

    pub ss: u16,

    /// Interrupt shadow for STI/MOV SS.
    ///
    /// Architectural rule: external interrupts are not recognized until *after*
    /// the instruction following STI/MOV SS completes.
    ///
    /// We implement this by setting `interrupt_shadow = 2` when STI/MOV SS
    /// executes. The generic post-instruction interrupt check decrements it;
    /// therefore:
    /// - after STI: shadow becomes 1 and interrupts are blocked
    /// - during the following instruction: shadow is 1 (blocks mid-instruction
    ///   "safe boundary" checks such as REP iterations)
    /// - after the following instruction: shadow becomes 0 and interrupts may
    ///   be delivered.
    pub interrupt_shadow: u8,
    pub pending_interrupt: Option<u8>,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            regs: [0; 16],
            rip: 0,
            flags: Flags::default(),
            ss: 0,
            interrupt_shadow: 0,
            pending_interrupt: None,
        }
    }
}

impl CpuState {
    pub fn reg(&self, reg: Reg) -> u64 {
        self.regs[reg as usize]
    }

    pub fn set_reg(&mut self, reg: Reg, value: u64) {
        self.regs[reg as usize] = value;
    }

    pub fn reg_by_index(&self, index: u8) -> u64 {
        self.regs[index as usize]
    }

    pub fn set_reg_by_index(&mut self, index: u8, value: u64) {
        self.regs[index as usize] = value;
    }

    pub fn set_pending_interrupt(&mut self, vector: u8) {
        self.pending_interrupt = Some(vector);
    }
}
