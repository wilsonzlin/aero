use super::AccessType;

const PF_ERR_PRESENT: u32 = 1 << 0;
const PF_ERR_WRITE: u32 = 1 << 1;
const PF_ERR_USER: u32 = 1 << 2;
const PF_ERR_RSVD: u32 = 1 << 3;
const PF_ERR_INSTR_FETCH: u32 = 1 << 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFault {
    /// Faulting linear address (CR2).
    pub cr2: u64,
    /// Page fault error code (bits per Intel/AMD manuals).
    pub error_code: u32,
}

impl PageFault {
    pub fn not_present(vaddr: u64, access: AccessType, cpl: u8) -> Self {
        Self {
            cr2: vaddr,
            error_code: Self::base_error_code(access, cpl),
        }
    }

    pub fn protection(vaddr: u64, access: AccessType, cpl: u8) -> Self {
        Self {
            cr2: vaddr,
            error_code: Self::base_error_code(access, cpl) | PF_ERR_PRESENT,
        }
    }

    pub fn rsvd(vaddr: u64, access: AccessType, cpl: u8) -> Self {
        Self {
            cr2: vaddr,
            error_code: Self::base_error_code(access, cpl) | PF_ERR_PRESENT | PF_ERR_RSVD,
        }
    }

    fn base_error_code(access: AccessType, cpl: u8) -> u32 {
        let mut error_code = 0u32;

        if access == AccessType::Write {
            error_code |= PF_ERR_WRITE;
        }
        if cpl == 3 {
            error_code |= PF_ERR_USER;
        }
        if access == AccessType::Execute {
            error_code |= PF_ERR_INSTR_FETCH;
        }

        error_code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_present_error_code_bits() {
        let pf = PageFault::not_present(0x1234, AccessType::Read, 0);
        assert_eq!(pf.cr2, 0x1234);
        assert_eq!(pf.error_code, 0);

        let pf = PageFault::not_present(0x1234, AccessType::Write, 3);
        assert_eq!(pf.error_code, PF_ERR_WRITE | PF_ERR_USER);
    }

    #[test]
    fn protection_error_code_bits() {
        let pf = PageFault::protection(0x1234, AccessType::Read, 0);
        assert_eq!(pf.error_code, PF_ERR_PRESENT);

        let pf = PageFault::protection(0x1234, AccessType::Execute, 3);
        assert_eq!(pf.error_code, PF_ERR_PRESENT | PF_ERR_USER | PF_ERR_INSTR_FETCH);
    }

    #[test]
    fn rsvd_error_code_bits() {
        let pf = PageFault::rsvd(0x1234, AccessType::Write, 0);
        assert_eq!(pf.error_code, PF_ERR_PRESENT | PF_ERR_WRITE | PF_ERR_RSVD);
    }
}

