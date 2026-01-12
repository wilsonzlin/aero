//! AHCI command list / command table parsing helpers.

use memory::MemoryBus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandHeader {
    pub cfl_bytes: u8,
    pub write: bool,
    pub prdt_len: u16,
    pub prdbc: u32,
    pub ctba: u64,
}

impl CommandHeader {
    pub const SIZE: usize = 32;

    pub fn read_from(mem: &mut dyn MemoryBus, paddr: u64) -> Self {
        let dw0 = mem.read_u32(paddr);
        // Use wrapping arithmetic so malformed guest DMA addresses can't panic under overflow
        // checks (e.g. fuzzing).
        let prdbc = mem.read_u32(paddr.wrapping_add(4));
        let ctba_low = mem.read_u32(paddr.wrapping_add(8)) as u64;
        let ctba_high = mem.read_u32(paddr.wrapping_add(12)) as u64;

        let cfl_dwords = (dw0 & 0x1f) as u8;
        let write = dw0 & (1 << 6) != 0;
        let prdt_len = ((dw0 >> 16) & 0xffff) as u16;

        Self {
            cfl_bytes: cfl_dwords.saturating_mul(4),
            write,
            prdt_len,
            prdbc,
            ctba: ctba_low | (ctba_high << 32),
        }
    }

    pub fn write_prdbc(mem: &mut dyn MemoryBus, paddr: u64, value: u32) {
        mem.write_u32(paddr.wrapping_add(4), value);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrdEntry {
    pub dba: u64,
    pub dbc: u32,
    pub interrupt_on_completion: bool,
}

impl PrdEntry {
    pub const SIZE: usize = 16;

    pub fn read_from(mem: &mut dyn MemoryBus, paddr: u64) -> Self {
        let dba_low = mem.read_u32(paddr) as u64;
        let dba_high = mem.read_u32(paddr.wrapping_add(4)) as u64;
        let dbc = mem.read_u32(paddr.wrapping_add(12));
        Self {
            dba: dba_low | (dba_high << 32),
            dbc,
            interrupt_on_completion: dbc & (1 << 31) != 0,
        }
    }

    pub fn byte_count(&self) -> usize {
        // Bits 0..21 store byte_count-1.
        (((self.dbc & 0x3f_ffff) as usize) + 1).max(1)
    }
}
