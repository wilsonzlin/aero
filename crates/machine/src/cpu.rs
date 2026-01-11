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
    fn fetch_u8(&mut self, mem: &impl MemoryAccess) -> u8 {
        let b = mem.read_u8(self.phys_ip());
        self.advance_ip(1);
        b
    }

    fn fetch_u16(&mut self, mem: &impl MemoryAccess) -> u16 {
        let lo = self.fetch_u8(mem) as u16;
        let hi = self.fetch_u8(mem) as u16;
        lo | (hi << 8)
    }

    fn fetch_u32(&mut self, mem: &impl MemoryAccess) -> u32 {
        let b0 = self.fetch_u8(mem) as u32;
        let b1 = self.fetch_u8(mem) as u32;
        let b2 = self.fetch_u8(mem) as u32;
        let b3 = self.fetch_u8(mem) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn al(&self) -> u8 {
        self.rax as u8
    }

    fn ax(&self) -> u16 {
        self.rax as u16
    }

    fn set_reg8(&mut self, reg: u8, val: u8) {
        let val = val as u64;
        match reg {
            0 => self.rax = (self.rax & !0xFF) | val,          // AL
            1 => self.rcx = (self.rcx & !0xFF) | val,          // CL
            2 => self.rdx = (self.rdx & !0xFF) | val,          // DL
            3 => self.rbx = (self.rbx & !0xFF) | val,          // BL
            4 => self.rax = (self.rax & !0xFF00) | (val << 8), // AH
            5 => self.rcx = (self.rcx & !0xFF00) | (val << 8), // CH
            6 => self.rdx = (self.rdx & !0xFF00) | (val << 8), // DH
            7 => self.rbx = (self.rbx & !0xFF00) | (val << 8), // BH
            _ => {}
        }
    }

    fn set_reg16(&mut self, reg: u8, val: u16) {
        let val = val as u64;
        match reg {
            0 => self.rax = (self.rax & !0xFFFF) | val,
            1 => self.rcx = (self.rcx & !0xFFFF) | val,
            2 => self.rdx = (self.rdx & !0xFFFF) | val,
            3 => self.rbx = (self.rbx & !0xFFFF) | val,
            4 => self.rsp = (self.rsp & !0xFFFF) | val,
            5 => self.rbp = (self.rbp & !0xFFFF) | val,
            6 => self.rsi = (self.rsi & !0xFFFF) | val,
            7 => self.rdi = (self.rdi & !0xFFFF) | val,
            _ => {}
        }
    }

    fn set_reg32(&mut self, reg: u8, val: u32) {
        let val = val as u64;
        match reg {
            0 => self.rax = val,
            1 => self.rcx = val,
            2 => self.rdx = val,
            3 => self.rbx = val,
            4 => self.rsp = val,
            5 => self.rbp = val,
            6 => self.rsi = val,
            7 => self.rdi = val,
            _ => {}
        }
    }

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

    fn advance_ip(&mut self, delta: u16) {
        self.set_ip(self.ip().wrapping_add(delta));
    }

    pub fn step(&mut self, mem: &mut impl MemoryAccess) -> CpuExit {
        if self.halted {
            return CpuExit::Halt;
        }

        // Operand-size override (16-bit default in real mode).
        let mut op32 = false;
        let mut opcode = self.fetch_u8(mem);
        if opcode == 0x66 {
            op32 = true;
            opcode = self.fetch_u8(mem);
        }

        match opcode {
            0x90 => {
                // NOP
                CpuExit::Continue
            }
            0xFA => {
                // CLI
                self.set_flag(FLAG_IF, false);
                CpuExit::Continue
            }
            0xFB => {
                // STI
                self.set_flag(FLAG_IF, true);
                CpuExit::Continue
            }
            0xB0..=0xB7 => {
                // MOV r8, imm8
                let reg = (opcode - 0xB0) as u8;
                let imm = self.fetch_u8(mem);
                self.set_reg8(reg, imm);
                CpuExit::Continue
            }
            0xB8..=0xBF => {
                // MOV r16/32, imm16/32
                let reg = (opcode - 0xB8) as u8;
                if op32 {
                    let imm = self.fetch_u32(mem);
                    self.set_reg32(reg, imm);
                } else {
                    let imm = self.fetch_u16(mem);
                    self.set_reg16(reg, imm);
                }
                CpuExit::Continue
            }
            0x8E => {
                // MOV Sreg, r/m16 (subset: DS/ES from AX).
                let modrm = self.fetch_u8(mem);
                match modrm {
                    0xD8 => {
                        self.ds.selector = self.ax(); // mov ds, ax
                        CpuExit::Continue
                    }
                    0xC0 => {
                        self.es.selector = self.ax(); // mov es, ax
                        CpuExit::Continue
                    }
                    _ => {
                        // Unsupported addressing mode.
                        self.halted = true;
                        CpuExit::Halt
                    }
                }
            }
            0xA0 => {
                // MOV AL, moffs16
                let off = self.fetch_u16(mem);
                let addr = self.linear_addr(self.ds, off);
                let val = mem.read_u8(addr);
                self.set_reg8(0, val);
                CpuExit::Continue
            }
            0xA2 => {
                // MOV moffs16, AL
                let off = self.fetch_u16(mem);
                let addr = self.linear_addr(self.ds, off);
                mem.write_u8(addr, self.al());
                CpuExit::Continue
            }
            0xA3 => {
                // MOV moffs16, AX
                let off = self.fetch_u16(mem);
                let addr = self.linear_addr(self.ds, off);
                mem.write_u16(addr, self.ax());
                CpuExit::Continue
            }
            0xC7 => {
                // MOV r/m16/32, imm16/32 (subset: modrm 0x06 => [disp16]).
                let modrm = self.fetch_u8(mem);
                if modrm != 0x06 {
                    self.halted = true;
                    return CpuExit::Halt;
                }
                let off = self.fetch_u16(mem);
                let addr = self.linear_addr(self.ds, off);
                if op32 {
                    let imm = self.fetch_u32(mem);
                    mem.write_u32(addr, imm);
                } else {
                    let imm = self.fetch_u16(mem);
                    mem.write_u16(addr, imm);
                }
                CpuExit::Continue
            }
            0xF4 => {
                // HLT
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
                let int = self.fetch_u8(mem);
                let next_ip = self.ip();
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
                let rel = self.fetch_u8(mem) as i8 as i16;
                let base = self.ip();
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
