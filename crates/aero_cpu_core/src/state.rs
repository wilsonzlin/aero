use aero_x86::Register;

pub const FLAG_CF: u64 = 1 << 0;
pub const FLAG_PF: u64 = 1 << 2;
pub const FLAG_AF: u64 = 1 << 4;
pub const FLAG_ZF: u64 = 1 << 6;
pub const FLAG_SF: u64 = 1 << 7;
pub const FLAG_DF: u64 = 1 << 10;
pub const FLAG_OF: u64 = 1 << 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuMode {
    /// 16-bit code segment (real mode or 16-bit protected mode).
    Bit16,
    /// 32-bit code segment (protected mode or compatibility mode).
    Bit32,
    /// 64-bit code segment (long mode).
    Bit64,
}

impl CpuMode {
    pub fn bitness(self) -> u32 {
        match self {
            CpuMode::Bit16 => 16,
            CpuMode::Bit32 => 32,
            CpuMode::Bit64 => 64,
        }
    }

    pub fn ip_mask(self) -> u64 {
        match self {
            CpuMode::Bit16 => 0xFFFF,
            CpuMode::Bit32 => 0xFFFF_FFFF,
            CpuMode::Bit64 => u64::MAX,
        }
    }

    pub fn addr_mask(self) -> u64 {
        self.ip_mask()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    ES = 0,
    CS = 1,
    SS = 2,
    DS = 3,
    FS = 4,
    GS = 5,
}

impl Segment {
    pub fn from_register(reg: Register) -> Option<Self> {
        match reg {
            Register::ES => Some(Segment::ES),
            Register::CS => Some(Segment::CS),
            Register::SS => Some(Segment::SS),
            Register::DS => Some(Segment::DS),
            Register::FS => Some(Segment::FS),
            Register::GS => Some(Segment::GS),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CpuState {
    gpr: [u64; 16],
    rip: u64,
    rflags: u64,
    seg_base: [u64; 6],
    pub mode: CpuMode,
    pub halted: bool,
}

impl Default for CpuState {
    fn default() -> Self {
        Self::new(CpuMode::Bit16)
    }
}

impl CpuState {
    pub fn new(mode: CpuMode) -> Self {
        Self {
            gpr: [0; 16],
            rip: 0,
            rflags: 0x2, // bit 1 is always set
            seg_base: [0; 6],
            mode,
            halted: false,
        }
    }

    pub fn bitness(&self) -> u32 {
        self.mode.bitness()
    }

    pub fn rip(&self) -> u64 {
        self.rip & self.mode.ip_mask()
    }

    pub fn set_rip(&mut self, rip: u64) {
        self.rip = rip & self.mode.ip_mask();
    }

    pub fn advance_rip(&mut self, delta: u64) {
        self.set_rip(self.rip().wrapping_add(delta));
    }

    pub fn rflags(&self) -> u64 {
        self.rflags
    }

    pub fn set_rflags(&mut self, flags: u64) {
        // Preserve the always-1 bit 1.
        self.rflags = flags | 0x2;
    }

    pub fn get_flag(&self, mask: u64) -> bool {
        (self.rflags & mask) != 0
    }

    pub fn set_flag(&mut self, mask: u64, val: bool) {
        if val {
            self.rflags |= mask;
        } else {
            self.rflags &= !mask;
        }
    }

    pub fn seg_base(&self, seg: Segment) -> u64 {
        self.seg_base[seg as usize]
    }

    pub fn set_seg_base(&mut self, seg: Segment, base: u64) {
        self.seg_base[seg as usize] = base;
    }

    pub fn seg_base_reg(&self, seg: Register) -> u64 {
        Segment::from_register(seg)
            .map(|s| self.seg_base(s))
            .unwrap_or(0)
    }

    pub fn gpr_u64(&self, index: usize) -> u64 {
        self.gpr[index]
    }

    pub fn set_gpr_u64(&mut self, index: usize, val: u64) {
        self.gpr[index] = val;
    }

    pub fn read_reg(&self, reg: Register) -> u64 {
        if let Some((idx, bits, high8)) = gpr_info(reg) {
            let full = self.gpr[idx];
            return match (bits, high8) {
                (8, false) => full & 0xFF,
                (8, true) => (full >> 8) & 0xFF,
                (16, _) => full & 0xFFFF,
                (32, _) => full & 0xFFFF_FFFF,
                (64, _) => full,
                _ => 0,
            };
        }
        match reg {
            Register::RIP | Register::EIP => self.rip(),
            _ => 0,
        }
    }

    pub fn write_reg(&mut self, reg: Register, val: u64) {
        if let Some((idx, bits, high8)) = gpr_info(reg) {
            let cur = self.gpr[idx];
            self.gpr[idx] = match (bits, high8) {
                (64, _) => val,
                // Writes to a 32-bit GPR clear the upper 32 bits, even in 64-bit mode.
                (32, _) => val & 0xFFFF_FFFF,
                (16, _) => (cur & !0xFFFF) | (val & 0xFFFF),
                (8, false) => (cur & !0xFF) | (val & 0xFF),
                (8, true) => (cur & !0xFF00) | ((val & 0xFF) << 8),
                _ => cur,
            };
            return;
        }

        match reg {
            Register::RIP | Register::EIP => self.set_rip(val),
            _ => {}
        }
    }

    pub fn stack_ptr_reg(&self) -> Register {
        match self.mode {
            CpuMode::Bit16 => Register::SP,
            CpuMode::Bit32 => Register::ESP,
            CpuMode::Bit64 => Register::RSP,
        }
    }

    pub fn stack_ptr_bits(&self) -> u32 {
        match self.mode {
            CpuMode::Bit64 => 64,
            CpuMode::Bit32 => 32,
            CpuMode::Bit16 => 16,
        }
    }

    pub fn stack_ptr(&self) -> u64 {
        let reg = self.stack_ptr_reg();
        self.read_reg(reg) & mask_bits(self.stack_ptr_bits())
    }

    pub fn set_stack_ptr(&mut self, val: u64) {
        let reg = self.stack_ptr_reg();
        let bits = self.stack_ptr_bits();
        let v = val & mask_bits(bits);
        self.write_reg(reg, v);
    }
}

pub fn mask_bits(bits: u32) -> u64 {
    match bits {
        8 => 0xFF,
        16 => 0xFFFF,
        32 => 0xFFFF_FFFF,
        64 => u64::MAX,
        _ => {
            if bits >= 64 {
                u64::MAX
            } else {
                (1u64 << bits) - 1
            }
        }
    }
}

fn gpr_info(reg: Register) -> Option<(usize, u32, bool)> {
    use Register::*;
    let (idx, bits, high8) = match reg {
        AL => (0, 8, false),
        CL => (1, 8, false),
        DL => (2, 8, false),
        BL => (3, 8, false),
        AH => (0, 8, true),
        CH => (1, 8, true),
        DH => (2, 8, true),
        BH => (3, 8, true),
        SPL => (4, 8, false),
        BPL => (5, 8, false),
        SIL => (6, 8, false),
        DIL => (7, 8, false),
        R8L => (8, 8, false),
        R9L => (9, 8, false),
        R10L => (10, 8, false),
        R11L => (11, 8, false),
        R12L => (12, 8, false),
        R13L => (13, 8, false),
        R14L => (14, 8, false),
        R15L => (15, 8, false),

        AX => (0, 16, false),
        CX => (1, 16, false),
        DX => (2, 16, false),
        BX => (3, 16, false),
        SP => (4, 16, false),
        BP => (5, 16, false),
        SI => (6, 16, false),
        DI => (7, 16, false),
        R8W => (8, 16, false),
        R9W => (9, 16, false),
        R10W => (10, 16, false),
        R11W => (11, 16, false),
        R12W => (12, 16, false),
        R13W => (13, 16, false),
        R14W => (14, 16, false),
        R15W => (15, 16, false),

        EAX => (0, 32, false),
        ECX => (1, 32, false),
        EDX => (2, 32, false),
        EBX => (3, 32, false),
        ESP => (4, 32, false),
        EBP => (5, 32, false),
        ESI => (6, 32, false),
        EDI => (7, 32, false),
        R8D => (8, 32, false),
        R9D => (9, 32, false),
        R10D => (10, 32, false),
        R11D => (11, 32, false),
        R12D => (12, 32, false),
        R13D => (13, 32, false),
        R14D => (14, 32, false),
        R15D => (15, 32, false),

        RAX => (0, 64, false),
        RCX => (1, 64, false),
        RDX => (2, 64, false),
        RBX => (3, 64, false),
        RSP => (4, 64, false),
        RBP => (5, 64, false),
        RSI => (6, 64, false),
        RDI => (7, 64, false),
        R8 => (8, 64, false),
        R9 => (9, 64, false),
        R10 => (10, 64, false),
        R11 => (11, 64, false),
        R12 => (12, 64, false),
        R13 => (13, 64, false),
        R14 => (14, 64, false),
        R15 => (15, 64, false),

        _ => return std::option::Option::None,
    };
    Some((idx, bits, high8))
}
