#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
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

impl Reg {
    pub const COUNT: usize = 16;

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// `CpuState` layout that is shared with generated Tier-1 WASM blocks.
///
/// The state is stored in linear memory at `cpu_ptr` (WASM i32 byte offset).
/// All integer fields are little-endian.
///
/// Layout (bytes):
/// - `regs[0..16]` (`u64` each, 8-byte aligned)
/// - `rip` (`u64`)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CpuState {
    regs: [u64; Reg::COUNT],
    pub rip: u64,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            regs: [0; Reg::COUNT],
            rip: 0,
        }
    }
}

impl CpuState {
    pub const REGS_OFFSET: u32 = 0;
    pub const RIP_OFFSET: u32 = (Reg::COUNT as u32) * 8;
    pub const BYTE_SIZE: usize = (Reg::COUNT * 8) + 8;

    #[inline]
    pub fn get_reg(&self, reg: Reg) -> u64 {
        self.regs[reg.index()]
    }

    #[inline]
    pub fn set_reg(&mut self, reg: Reg, val: u64) {
        self.regs[reg.index()] = val;
    }

    #[inline]
    pub const fn reg_offset(reg: Reg) -> u32 {
        Self::REGS_OFFSET + (reg as u32) * 8
    }

    pub fn write_to_mem(&self, mem: &mut [u8], base: usize) {
        assert!(
            base + Self::BYTE_SIZE <= mem.len(),
            "CpuState write out of bounds: base={base} size={} mem_len={}",
            Self::BYTE_SIZE,
            mem.len()
        );
        for (i, reg) in self.regs.iter().enumerate() {
            let off = base + i * 8;
            mem[off..off + 8].copy_from_slice(&reg.to_le_bytes());
        }
        let rip_off = base + (Reg::COUNT * 8);
        mem[rip_off..rip_off + 8].copy_from_slice(&self.rip.to_le_bytes());
    }

    pub fn read_from_mem(mem: &[u8], base: usize) -> Self {
        assert!(
            base + Self::BYTE_SIZE <= mem.len(),
            "CpuState read out of bounds: base={base} size={} mem_len={}",
            Self::BYTE_SIZE,
            mem.len()
        );
        let mut regs = [0u64; Reg::COUNT];
        for i in 0..Reg::COUNT {
            let off = base + i * 8;
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&mem[off..off + 8]);
            regs[i] = u64::from_le_bytes(buf);
        }
        let rip_off = base + (Reg::COUNT * 8);
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&mem[rip_off..rip_off + 8]);
        let rip = u64::from_le_bytes(buf);
        Self { regs, rip }
    }
}
