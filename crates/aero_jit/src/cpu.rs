use aero_types::Gpr;

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

/// 4KiB page shift used by the baseline JIT TLB.
pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_SHIFT;
pub const PAGE_OFFSET_MASK: u64 = PAGE_SIZE - 1;
pub const PAGE_BASE_MASK: u64 = !PAGE_OFFSET_MASK;

/// Number of entries in the baseline JIT TLB.
///
/// This is intentionally small and direct-mapped: it is a JIT acceleration structure,
/// not an architecturally-accurate model of x86 TLBs.
pub const JIT_TLB_ENTRIES: usize = 256;
pub const JIT_TLB_INDEX_MASK: u64 = (JIT_TLB_ENTRIES as u64) - 1;

/// Size of a single TLB entry in bytes (`tag: u64` + `data: u64`).
pub const JIT_TLB_ENTRY_SIZE: u32 = 16;

/// Entry flags packed into the low 12 bits of the returned translation `data` word.
///
/// The upper bits contain the 4KiB-aligned physical page base.
pub const TLB_FLAG_READ: u64 = 1 << 0;
pub const TLB_FLAG_WRITE: u64 = 1 << 1;
pub const TLB_FLAG_EXEC: u64 = 1 << 2;
pub const TLB_FLAG_IS_RAM: u64 = 1 << 3;

/// `CpuState` layout that is shared with generated Tier-1 WASM blocks.
///
/// The state is stored in linear memory at `cpu_ptr` (WASM i32 byte offset).
/// All integer fields are little-endian.
///
/// Layout (bytes):
/// - `regs[0..16]` (`u64` each, 8-byte aligned)
/// - `rip` (`u64`)
/// - `ram_base` (`u64`): base offset of guest RAM within linear memory
/// - `tlb_salt` (`u64`): tag salt used by the baseline JIT TLB
/// - `tlb[]` (`JIT_TLB_ENTRIES` × 16 bytes): `{ tag: u64, data: u64 }`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct CpuState {
    regs: [u64; Reg::COUNT],
    pub rip: u64,
    pub ram_base: u64,
    pub tlb_salt: u64,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            regs: [0; Reg::COUNT],
            rip: 0,
            ram_base: 0,
            tlb_salt: 0,
        }
    }
}

impl CpuState {
    pub const REGS_OFFSET: u32 = 0;
    pub const RIP_OFFSET: u32 = (Reg::COUNT as u32) * 8;
    pub const RAM_BASE_OFFSET: u32 = Self::RIP_OFFSET + 8;
    pub const TLB_SALT_OFFSET: u32 = Self::RAM_BASE_OFFSET + 8;
    pub const TLB_OFFSET: u32 = Self::TLB_SALT_OFFSET + 8;

    /// Size of the architectural CPU state prefix (regs + rip + jit metadata fields).
    ///
    /// Note that this does *not* include the variable-size TLB array, which begins at
    /// [`Self::TLB_OFFSET`].
    pub const BYTE_SIZE: usize = Self::TLB_OFFSET as usize;
    pub const TLB_BYTES: usize = JIT_TLB_ENTRIES * (JIT_TLB_ENTRY_SIZE as usize);
    pub const TOTAL_BYTE_SIZE: usize = Self::BYTE_SIZE + Self::TLB_BYTES;

    #[inline]
    pub fn get_reg(&self, reg: Reg) -> u64 {
        self.regs[reg.index()]
    }

    #[inline]
    pub fn get_gpr(&self, reg: Gpr) -> u64 {
        self.regs[reg.as_u8() as usize]
    }

    #[inline]
    pub fn set_reg(&mut self, reg: Reg, val: u64) {
        self.regs[reg.index()] = val;
    }

    #[inline]
    pub fn set_gpr(&mut self, reg: Gpr, val: u64) {
        self.regs[reg.as_u8() as usize] = val;
    }

    #[inline]
    pub const fn reg_offset(reg: Reg) -> u32 {
        Self::REGS_OFFSET + (reg as u32) * 8
    }

    #[inline]
    pub const fn gpr_offset(reg: Gpr) -> u32 {
        Self::REGS_OFFSET + (reg.as_u8() as u32) * 8
    }

    pub fn write_to_mem(&self, mem: &mut [u8], base: usize) {
        assert!(
            base + Self::TOTAL_BYTE_SIZE <= mem.len(),
            "CpuState write out of bounds"
        );
        for (i, reg) in self.regs.iter().enumerate() {
            let off = base + i * 8;
            mem[off..off + 8].copy_from_slice(&reg.to_le_bytes());
        }
        let rip_off = base + (Reg::COUNT * 8);
        mem[rip_off..rip_off + 8].copy_from_slice(&self.rip.to_le_bytes());

        let ram_base_off = base + (Self::RAM_BASE_OFFSET as usize);
        mem[ram_base_off..ram_base_off + 8].copy_from_slice(&self.ram_base.to_le_bytes());

        let tlb_salt_off = base + (Self::TLB_SALT_OFFSET as usize);
        mem[tlb_salt_off..tlb_salt_off + 8].copy_from_slice(&self.tlb_salt.to_le_bytes());
    }

    pub fn read_from_mem(mem: &[u8], base: usize) -> Self {
        assert!(
            base + Self::TOTAL_BYTE_SIZE <= mem.len(),
            "CpuState read out of bounds"
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

        let ram_base_off = base + (Self::RAM_BASE_OFFSET as usize);
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&mem[ram_base_off..ram_base_off + 8]);
        let ram_base = u64::from_le_bytes(buf);

        let tlb_salt_off = base + (Self::TLB_SALT_OFFSET as usize);
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&mem[tlb_salt_off..tlb_salt_off + 8]);
        let tlb_salt = u64::from_le_bytes(buf);

        Self {
            regs,
            rip,
            ram_base,
            tlb_salt,
        }
    }
}
