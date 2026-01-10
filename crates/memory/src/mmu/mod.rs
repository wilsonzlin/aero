//! Paging / MMU helpers.

pub mod mode32;

use crate::bus::MemoryBus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    Read,
    Write,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFault {
    /// Faulting linear address (to be written to CR2).
    pub addr: u32,
    /// x86 #PF error code.
    pub error_code: u32,
}

impl PageFault {
    pub const EC_P: u32 = 1 << 0;
    pub const EC_WR: u32 = 1 << 1;
    pub const EC_US: u32 = 1 << 2;
    pub const EC_RSVD: u32 = 1 << 3;
    pub const EC_ID: u32 = 1 << 4;

    pub fn new(addr: u32, present: bool, write: bool, user: bool, rsvd: bool, instr: bool) -> Self {
        let mut error_code = 0u32;
        if present {
            error_code |= Self::EC_P;
        }
        if write {
            error_code |= Self::EC_WR;
        }
        if user {
            error_code |= Self::EC_US;
        }
        if rsvd {
            error_code |= Self::EC_RSVD;
        }
        if instr {
            error_code |= Self::EC_ID;
        }
        Self { addr, error_code }
    }
}

pub const CR0_PG: u32 = 1 << 31;
pub const CR0_WP: u32 = 1 << 16;
pub const CR4_PSE: u32 = 1 << 4;
pub const CR4_PAE: u32 = 1 << 5;

/// Translate a linear address to a physical address.
///
/// Currently supports:
/// - paging disabled: identity mapping (32-bit mask)
/// - 32-bit non-PAE paging: `CR0.PG=1` and `CR4.PAE=0`
pub fn translate(
    bus: &mut impl MemoryBus,
    linear: u64,
    access: AccessType,
    cpl: u8,
    cr0: u32,
    cr3: u32,
    cr4: u32,
) -> Result<u64, PageFault> {
    let vaddr = (linear & 0xFFFF_FFFF) as u32;
    if (cr0 & CR0_PG) == 0 {
        return Ok(vaddr as u64);
    }

    if (cr4 & CR4_PAE) != 0 {
        // PAE/long-mode walks are implemented in other modules.
        // For now fail with a deterministic reserved-bit violation.
        return Err(PageFault::new(
            vaddr,
            true,
            access == AccessType::Write,
            cpl == 3,
            true,
            access == AccessType::Execute,
        ));
    }

    mode32::translate(bus, vaddr as u64, access, cpl, cr0, cr3, cr4)
}

