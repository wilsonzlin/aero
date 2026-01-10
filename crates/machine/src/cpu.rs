use crate::memory::MemoryAccess;

// Flag bits (low 16 bits are the real-mode-visible flags).
pub const FLAG_CF: u64 = 1 << 0;
pub const FLAG_ALWAYS_ON: u64 = 1 << 1;
pub const FLAG_PF: u64 = 1 << 2;
pub const FLAG_AF: u64 = 1 << 4;
pub const FLAG_ZF: u64 = 1 << 6;
pub const FLAG_SF: u64 = 1 << 7;
pub const FLAG_TF: u64 = 1 << 8;
pub const FLAG_IF: u64 = 1 << 9;
pub const FLAG_DF: u64 = 1 << 10;
pub const FLAG_OF: u64 = 1 << 11;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Segment {
    pub selector: u16,
}

impl Segment {
    #[inline]
    pub fn base(self) -> u64 {
        (self.selector as u64) << 4
    }
}

#[derive(Debug, Clone)]
pub struct CpuState {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub rip: u64,
    pub rflags: u64,

    pub cs: Segment,
    pub ds: Segment,
    pub es: Segment,
    pub ss: Segment,

    /// Set by `INT imm8` and consumed by BIOS ROM stubs.
    pub pending_bios_int: Option<u8>,

    pub halted: bool,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: 0,
            rsp: 0,
            rip: 0,
            rflags: FLAG_ALWAYS_ON,
            cs: Segment { selector: 0 },
            ds: Segment { selector: 0 },
            es: Segment { selector: 0 },
            ss: Segment { selector: 0 },
            pending_bios_int: None,
            halted: false,
        }
    }
}

impl CpuState {
    pub fn ip(&self) -> u16 {
        self.rip as u16
    }

    pub fn set_ip(&mut self, ip: u16) {
        self.rip = ip as u64;
    }

    pub fn sp(&self) -> u16 {
        self.rsp as u16
    }

    pub fn set_sp(&mut self, sp: u16) {
        self.rsp = sp as u64;
    }

    pub fn linear_addr(&self, seg: Segment, off: u16) -> u64 {
        seg.base() + off as u64
    }

    pub fn phys_ip(&self) -> u64 {
        self.linear_addr(self.cs, self.ip())
    }

    pub fn set_flag(&mut self, mask: u64, set: bool) {
        if set {
            self.rflags |= mask;
        } else {
            self.rflags &= !mask;
        }
        self.rflags |= FLAG_ALWAYS_ON;
    }

    fn push_u16(&mut self, mem: &mut impl MemoryAccess, val: u16) {
        let sp = self.sp().wrapping_sub(2);
        self.set_sp(sp);
        let addr = self.linear_addr(self.ss, sp);
        mem.write_u16(addr, val);
    }

    fn pop_u16(&mut self, mem: &mut impl MemoryAccess) -> u16 {
        let sp = self.sp();
        let addr = self.linear_addr(self.ss, sp);
        let val = mem.read_u16(addr);
        self.set_sp(sp.wrapping_add(2));
        val
    }

    fn next_u8(&self, mem: &impl MemoryAccess, disp: u16) -> u8 {
        mem.read_u8(self.linear_addr(self.cs, self.ip().wrapping_add(disp)))
    }

    fn advance_ip(&mut self, delta: u16) {
        self.set_ip(self.ip().wrapping_add(delta));
    }

    pub fn step(&mut self, mem: &mut impl MemoryAccess) -> CpuExit {
        if self.halted {
            return CpuExit::Halt;
        }

        let opcode = mem.read_u8(self.phys_ip());
        match opcode {
            0x90 => {
                // NOP
                self.advance_ip(1);
                CpuExit::Continue
            }
            0xFA => {
                // CLI
                self.set_flag(FLAG_IF, false);
                self.advance_ip(1);
                CpuExit::Continue
            }
            0xFB => {
                // STI
                self.set_flag(FLAG_IF, true);
                self.advance_ip(1);
                CpuExit::Continue
            }
            0xF4 => {
                // HLT
                self.advance_ip(1);
                if let Some(int) = self.pending_bios_int.take() {
                    CpuExit::BiosInterrupt(int)
                } else {
                    self.halted = true;
                    CpuExit::Halt
                }
            }
            0xCF => {
                // IRET (16-bit)
                let ip = self.pop_u16(mem);
                let cs = self.pop_u16(mem);
                let flags = self.pop_u16(mem);

                self.cs.selector = cs;
                self.set_ip(ip);
                self.rflags = (self.rflags & !0xFFFF) | flags as u64;
                self.rflags |= FLAG_ALWAYS_ON;

                CpuExit::Continue
            }
            0xCD => {
                // INT imm8
                let int = self.next_u8(mem, 1);
                let next_ip = self.ip().wrapping_add(2);
                let flags = self.rflags as u16;
                let cs = self.cs.selector;

                self.push_u16(mem, flags);
                self.push_u16(mem, cs);
                self.push_u16(mem, next_ip);

                self.set_flag(FLAG_IF | FLAG_TF, false);

                let vec_addr = (int as u64) * 4;
                let offset = mem.read_u16(vec_addr);
                let segment = mem.read_u16(vec_addr + 2);
                self.cs.selector = segment;
                self.set_ip(offset);
                self.pending_bios_int = Some(int);

                CpuExit::Continue
            }
            0xEB => {
                // JMP rel8
                let rel = self.next_u8(mem, 1) as i8 as i16;
                let base = self.ip().wrapping_add(2);
                let new_ip = (base as i16).wrapping_add(rel) as u16;
                self.set_ip(new_ip);
                CpuExit::Continue
            }
            _ => {
                // For the BIOS firmware task we only implement a minimal set.
                // Treat unknown opcodes as a hard halt to keep tests deterministic.
                self.halted = true;
                CpuExit::Halt
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuExit {
    Continue,
    Halt,
    BiosInterrupt(u8),
}
