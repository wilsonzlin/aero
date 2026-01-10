use super::error::EmuException;
use super::flags::{
    ip_mask_for_mode, Flag, LazyFlags, FLAGS_ARITH_MASK, FLAG_AF, FLAG_CF, FLAG_DF, FLAG_IF,
    FLAG_OF, FLAG_PF, FLAG_SF, FLAG_TF, FLAG_ZF,
};
use iced_x86::Register;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuMode {
    /// 16-bit real mode (CS:IP segmentation, 20-bit physical addresses unless A20 enabled).
    Real,
    /// 32-bit protected mode (segmentation + optional paging).
    Protected,
    /// 64-bit long mode.
    Long,
}

#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub selector: u16,
    pub base: u64,
    pub limit: u32,
}

impl Segment {
    pub const fn new(selector: u16, base: u64, limit: u32) -> Self {
        Self {
            selector,
            base,
            limit,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DescriptorTable {
    pub base: u64,
    pub limit: u16,
}

impl DescriptorTable {
    pub const fn empty() -> Self {
        Self { base: 0, limit: 0 }
    }
}

#[derive(Debug, Clone)]
pub struct CpuState {
    gpr: [u64; 16],
    pub rip: u64,
    rflags: u64,
    pub mode: CpuMode,
    pub a20_enabled: bool,
    pub halted: bool,

    pub segments: [Segment; 6], // ES, CS, SS, DS, FS, GS
    pub gdtr: DescriptorTable,
    pub idtr: DescriptorTable,
    pub tr: u16,

    pub cr: [u64; 9],
    pub dr: [u64; 8],
    pub msr: HashMap<u32, u64>,

    pub xmm: [u128; 16],
    pub tsc: u64,

    lazy_flags: Option<LazyFlags>,
}

impl Default for CpuState {
    fn default() -> Self {
        Self::new(CpuMode::Real)
    }
}

impl CpuState {
    pub fn new(mode: CpuMode) -> Self {
        let mut cpu = Self {
            gpr: [0; 16],
            rip: 0,
            rflags: 0x2, // bit 1 always set
            mode,
            a20_enabled: true,
            halted: false,
            segments: [
                Segment::new(0, 0, 0xffff), // ES
                Segment::new(0, 0, 0xffff), // CS
                Segment::new(0, 0, 0xffff), // SS
                Segment::new(0, 0, 0xffff), // DS
                Segment::new(0, 0, 0xffff), // FS
                Segment::new(0, 0, 0xffff), // GS
            ],
            gdtr: DescriptorTable::empty(),
            idtr: DescriptorTable::empty(),
            tr: 0,
            cr: [0; 9],
            dr: [0; 8],
            msr: HashMap::new(),
            xmm: [0; 16],
            tsc: 0,
            lazy_flags: None,
        };
        cpu.recalc_real_mode_segments();
        cpu
    }

    pub fn cs(&self) -> Segment {
        self.segments[1]
    }

    pub fn ss(&self) -> Segment {
        self.segments[2]
    }

    pub fn ds(&self) -> Segment {
        self.segments[3]
    }

    pub fn set_mode(&mut self, mode: CpuMode) {
        self.mode = mode;
    }

    pub fn set_segment_selector(
        &mut self,
        seg: Register,
        selector: u16,
    ) -> Result<(), EmuException> {
        let idx = seg_index(seg)?;
        self.segments[idx].selector = selector;
        match self.mode {
            CpuMode::Real => {
                self.segments[idx].base = (selector as u64) << 4;
                self.segments[idx].limit = 0xffff;
            }
            CpuMode::Protected | CpuMode::Long => {
                // In protected/long mode, the caller must use `load_segment_from_gdt()` when needed.
                // For Tier-0 we allow direct selector updates, leaving base/limit as-is.
            }
        }
        Ok(())
    }

    pub fn load_segment_from_gdt(
        &mut self,
        seg: Register,
        selector: u16,
        read_u64: impl FnOnce(u64) -> Result<u64, EmuException>,
    ) -> Result<(), EmuException> {
        let idx = seg_index(seg)?;
        if self.mode == CpuMode::Real {
            self.segments[idx].selector = selector;
            self.segments[idx].base = (selector as u64) << 4;
            self.segments[idx].limit = 0xffff;
            return Ok(());
        }

        let table_base = self.gdtr.base;
        let entry_index = (selector >> 3) as u64;
        let desc_addr = table_base + entry_index * 8;
        let raw = read_u64(desc_addr)?;

        // Very small subset of descriptor parsing: base and limit.
        let limit_low = (raw & 0xffff) as u32;
        let base_low = ((raw >> 16) & 0xffff) as u32;
        let base_mid = ((raw >> 32) & 0xff) as u32;
        let access = ((raw >> 40) & 0xff) as u32;
        let limit_high = ((raw >> 48) & 0xf) as u32;
        let flags = ((raw >> 52) & 0xf) as u32;
        let base_high = ((raw >> 56) & 0xff) as u32;

        let mut limit = limit_low | (limit_high << 16);
        if (flags & 0x8) != 0 {
            // Granularity: 4KiB pages.
            limit = (limit << 12) | 0xfff;
        }
        let base = (base_low | (base_mid << 16) | (base_high << 24)) as u64;

        // If it's a null selector, treat as zero base/limit.
        if selector & !0x7 == 0 {
            self.segments[idx] = Segment::new(selector, 0, 0);
            return Ok(());
        }

        // Basic present bit check.
        if (access & 0x80) == 0 {
            return Err(EmuException::InvalidOpcode);
        }

        self.segments[idx] = Segment::new(selector, base, limit);
        Ok(())
    }

    pub fn ip_mask(&self) -> u64 {
        ip_mask_for_mode(self.mode)
    }

    pub fn set_rip(&mut self, rip: u64) {
        self.rip = rip & self.ip_mask();
    }

    pub fn apply_a20(&self, addr: u64) -> u64 {
        if self.mode == CpuMode::Real && !self.a20_enabled {
            addr & 0xfffff
        } else {
            addr
        }
    }

    pub fn set_lazy_flags(&mut self, lf: LazyFlags) {
        self.lazy_flags = Some(lf);
    }

    pub fn clear_lazy_flags(&mut self) {
        self.lazy_flags = None;
    }

    pub fn materialize_lazy_flags(&mut self) {
        if let Some(lf) = self.lazy_flags {
            self.set_flag(Flag::Cf, lf.cf());
            self.set_flag(Flag::Pf, lf.pf());
            self.set_flag(Flag::Af, lf.af());
            self.set_flag(Flag::Zf, lf.zf());
            self.set_flag(Flag::Sf, lf.sf());
            self.set_flag(Flag::Of, lf.of());
        }
        self.lazy_flags = None;
    }

    pub fn get_flag(&self, flag: Flag) -> bool {
        if let Some(lf) = self.lazy_flags {
            match flag {
                Flag::Cf => return lf.cf(),
                Flag::Pf => return lf.pf(),
                Flag::Af => return lf.af(),
                Flag::Zf => return lf.zf(),
                Flag::Sf => return lf.sf(),
                Flag::Of => return lf.of(),
                _ => {}
            }
        }
        let bit = match flag {
            Flag::Cf => FLAG_CF,
            Flag::Pf => FLAG_PF,
            Flag::Af => FLAG_AF,
            Flag::Zf => FLAG_ZF,
            Flag::Sf => FLAG_SF,
            Flag::Tf => FLAG_TF,
            Flag::If => FLAG_IF,
            Flag::Df => FLAG_DF,
            Flag::Of => FLAG_OF,
        };
        (self.rflags & bit) != 0
    }

    pub fn set_flag(&mut self, flag: Flag, value: bool) {
        let bit = match flag {
            Flag::Cf => FLAG_CF,
            Flag::Pf => FLAG_PF,
            Flag::Af => FLAG_AF,
            Flag::Zf => FLAG_ZF,
            Flag::Sf => FLAG_SF,
            Flag::Tf => FLAG_TF,
            Flag::If => FLAG_IF,
            Flag::Df => FLAG_DF,
            Flag::Of => FLAG_OF,
        };
        if value {
            self.rflags |= bit;
        } else {
            self.rflags &= !bit;
        }
    }

    pub fn rflags(&self) -> u64 {
        if let Some(lf) = self.lazy_flags {
            let mut rf = self.rflags & !FLAGS_ARITH_MASK;
            if lf.cf() {
                rf |= FLAG_CF;
            }
            if lf.pf() {
                rf |= FLAG_PF;
            }
            if lf.af() {
                rf |= FLAG_AF;
            }
            if lf.zf() {
                rf |= FLAG_ZF;
            }
            if lf.sf() {
                rf |= FLAG_SF;
            }
            if lf.of() {
                rf |= FLAG_OF;
            }
            rf
        } else {
            self.rflags
        }
    }

    pub fn set_rflags(&mut self, value: u64) {
        // Preserve bit 1 (always 1).
        self.rflags = (value | 0x2) & 0xffff_ffff_ffff_ffff;
        self.lazy_flags = None;
    }

    pub fn recalc_real_mode_segments(&mut self) {
        if self.mode != CpuMode::Real {
            return;
        }
        for seg in &mut self.segments {
            seg.base = (seg.selector as u64) << 4;
            seg.limit = 0xffff;
        }
    }

    pub fn read_reg(&self, reg: Register) -> Result<u64, EmuException> {
        match reg {
            Register::None => Ok(0),
            Register::RIP | Register::EIP => Ok(match reg {
                Register::RIP => self.rip,
                Register::EIP => self.rip & 0xffff_ffff,
                _ => unreachable!(),
            }),
            Register::ES
            | Register::CS
            | Register::SS
            | Register::DS
            | Register::FS
            | Register::GS => Ok(self.segments[seg_index(reg)?].selector as u64),
            Register::CR0 | Register::CR2 | Register::CR3 | Register::CR4 | Register::CR8 => {
                Ok(self.cr[cr_index(reg)?])
            }
            Register::DR0
            | Register::DR1
            | Register::DR2
            | Register::DR3
            | Register::DR6
            | Register::DR7 => Ok(self.dr[dr_index(reg)?]),
            _ => {
                if let Some((idx, shift, bits, zero_extend_32)) = gpr_access(reg) {
                    let val = self.gpr[idx];
                    let masked = if bits == 64 {
                        val
                    } else {
                        (val >> shift) & ((1u64 << bits) - 1)
                    };
                    if zero_extend_32 {
                        Ok(masked & 0xffff_ffff)
                    } else {
                        Ok(masked)
                    }
                } else {
                    Err(EmuException::Unimplemented(iced_x86::Code::INVALID))
                }
            }
        }
    }

    pub fn write_reg(&mut self, reg: Register, value: u64) -> Result<(), EmuException> {
        match reg {
            Register::None => Ok(()),
            Register::RIP | Register::EIP => {
                self.set_rip(match reg {
                    Register::RIP => value,
                    Register::EIP => value & 0xffff_ffff,
                    _ => unreachable!(),
                });
                Ok(())
            }
            Register::ES
            | Register::CS
            | Register::SS
            | Register::DS
            | Register::FS
            | Register::GS => self.set_segment_selector(reg, value as u16),
            Register::CR0 | Register::CR2 | Register::CR3 | Register::CR4 | Register::CR8 => {
                let idx = cr_index(reg)?;
                if reg == Register::CR0 {
                    let old = self.cr[idx];
                    self.cr[idx] = value;

                    const CR0_PE: u64 = 1 << 0;
                    let old_pe = (old & CR0_PE) != 0;
                    let new_pe = (value & CR0_PE) != 0;

                    if !old_pe && new_pe && self.mode == CpuMode::Real {
                        self.mode = CpuMode::Protected;
                    } else if old_pe && !new_pe && self.mode != CpuMode::Real {
                        self.mode = CpuMode::Real;
                        self.recalc_real_mode_segments();
                    }
                } else {
                    self.cr[idx] = value;
                }
                Ok(())
            }
            Register::DR0
            | Register::DR1
            | Register::DR2
            | Register::DR3
            | Register::DR6
            | Register::DR7 => {
                self.dr[dr_index(reg)?] = value;
                Ok(())
            }
            _ => {
                if let Some((idx, shift, bits, zero_extend_32)) = gpr_access(reg) {
                    let mask = if bits == 64 {
                        u64::MAX
                    } else {
                        (1u64 << bits) - 1
                    } << shift;
                    let mut new_val = self.gpr[idx] & !mask;
                    let mut v = value
                        & if bits == 64 {
                            u64::MAX
                        } else {
                            (1u64 << bits) - 1
                        };
                    if zero_extend_32 {
                        v &= 0xffff_ffff;
                        new_val = 0; // writing to r32 zero-extends.
                    }
                    new_val |= v << shift;
                    self.gpr[idx] = new_val;
                    Ok(())
                } else {
                    Err(EmuException::Unimplemented(iced_x86::Code::INVALID))
                }
            }
        }
    }

    pub fn read_gpr64(&self, idx: usize) -> u64 {
        self.gpr[idx]
    }

    pub fn write_gpr64(&mut self, idx: usize, value: u64) {
        self.gpr[idx] = value;
    }
}

fn seg_index(reg: Register) -> Result<usize, EmuException> {
    match reg {
        Register::ES => Ok(0),
        Register::CS => Ok(1),
        Register::SS => Ok(2),
        Register::DS => Ok(3),
        Register::FS => Ok(4),
        Register::GS => Ok(5),
        _ => Err(EmuException::InvalidOpcode),
    }
}

fn cr_index(reg: Register) -> Result<usize, EmuException> {
    match reg {
        Register::CR0 => Ok(0),
        Register::CR2 => Ok(2),
        Register::CR3 => Ok(3),
        Register::CR4 => Ok(4),
        Register::CR8 => Ok(8),
        _ => Err(EmuException::InvalidOpcode),
    }
}

fn dr_index(reg: Register) -> Result<usize, EmuException> {
    match reg {
        Register::DR0 => Ok(0),
        Register::DR1 => Ok(1),
        Register::DR2 => Ok(2),
        Register::DR3 => Ok(3),
        Register::DR6 => Ok(6),
        Register::DR7 => Ok(7),
        _ => Err(EmuException::InvalidOpcode),
    }
}

fn gpr_access(reg: Register) -> Option<(usize, u32, u32, bool)> {
    use iced_x86::Register::*;
    // Returns (gpr_idx, bit_shift, bit_width, zero_extend_32_write)
    let (idx, shift, bits, zext32) = match reg {
        AL => (0, 0, 8, false),
        CL => (1, 0, 8, false),
        DL => (2, 0, 8, false),
        BL => (3, 0, 8, false),
        AH => (0, 8, 8, false),
        CH => (1, 8, 8, false),
        DH => (2, 8, 8, false),
        BH => (3, 8, 8, false),
        SPL => (4, 0, 8, false),
        BPL => (5, 0, 8, false),
        SIL => (6, 0, 8, false),
        DIL => (7, 0, 8, false),

        AX => (0, 0, 16, false),
        CX => (1, 0, 16, false),
        DX => (2, 0, 16, false),
        BX => (3, 0, 16, false),
        SP => (4, 0, 16, false),
        BP => (5, 0, 16, false),
        SI => (6, 0, 16, false),
        DI => (7, 0, 16, false),

        EAX => (0, 0, 32, true),
        ECX => (1, 0, 32, true),
        EDX => (2, 0, 32, true),
        EBX => (3, 0, 32, true),
        ESP => (4, 0, 32, true),
        EBP => (5, 0, 32, true),
        ESI => (6, 0, 32, true),
        EDI => (7, 0, 32, true),

        RAX => (0, 0, 64, false),
        RCX => (1, 0, 64, false),
        RDX => (2, 0, 64, false),
        RBX => (3, 0, 64, false),
        RSP => (4, 0, 64, false),
        RBP => (5, 0, 64, false),
        RSI => (6, 0, 64, false),
        RDI => (7, 0, 64, false),

        R8L => (8, 0, 8, false),
        R9L => (9, 0, 8, false),
        R10L => (10, 0, 8, false),
        R11L => (11, 0, 8, false),
        R12L => (12, 0, 8, false),
        R13L => (13, 0, 8, false),
        R14L => (14, 0, 8, false),
        R15L => (15, 0, 8, false),

        R8W => (8, 0, 16, false),
        R9W => (9, 0, 16, false),
        R10W => (10, 0, 16, false),
        R11W => (11, 0, 16, false),
        R12W => (12, 0, 16, false),
        R13W => (13, 0, 16, false),
        R14W => (14, 0, 16, false),
        R15W => (15, 0, 16, false),

        R8D => (8, 0, 32, true),
        R9D => (9, 0, 32, true),
        R10D => (10, 0, 32, true),
        R11D => (11, 0, 32, true),
        R12D => (12, 0, 32, true),
        R13D => (13, 0, 32, true),
        R14D => (14, 0, 32, true),
        R15D => (15, 0, 32, true),

        R8 => (8, 0, 64, false),
        R9 => (9, 0, 64, false),
        R10 => (10, 0, 64, false),
        R11 => (11, 0, 64, false),
        R12 => (12, 0, 64, false),
        R13 => (13, 0, 64, false),
        R14 => (14, 0, 64, false),
        R15 => (15, 0, 64, false),

        _ => return std::option::Option::None,
    };
    Some((idx, shift, bits, zext32))
}
