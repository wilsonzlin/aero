//! IA-32 PAE paging (CR0.PG=1, CR4.PAE=1, EFER.LME=0).
//!
//! 3-level page walk:
//!   PDPT (4 entries) -> PD (512 entries) -> PT (512 entries).
//!
//! Supported leaf mappings:
//!   - 4KiB pages via PTEs
//!   - 2MiB pages via PDEs with PS=1
//!
//! NX (XD) support is enforced only when `EFER.NXE=1` and the access is an
//! instruction fetch (`AccessType::Execute`).

use crate::bus::MemoryBus;
use crate::mmu::{
    AccessType, TranslateError, CR0_WP, EFER_NXE, PFEC_ID, PFEC_P, PFEC_RSVD, PFEC_US, PFEC_WR,
};

const PTE_P: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_US: u64 = 1 << 2;
const PTE_A: u64 = 1 << 5;
const PTE_D: u64 = 1 << 6;
const PDE_PS: u64 = 1 << 7;
const PTE_NX: u64 = 1 << 63;

const CR3_PDPT_MASK: u64 = 0xFFFF_FFE0;

const ADDR_MASK_4K: u64 = 0x000F_FFFF_FFFF_F000;
const ADDR_MASK_2M: u64 = 0x000F_FFFF_FFE0_0000;

const PAGE_OFFSET_4K: u32 = 0x0000_0FFF;
const PAGE_OFFSET_2M: u32 = 0x001F_FFFF;

// Simplified reserved-bit model: treat bits 62:52 as reserved.
const RSVD_HIGH_MASK: u64 = 0x7FF0_0000_0000_0000;

// For PAE PDPT entries (PDPTE), only P (bit 0), PWT (bit 3), PCD (bit 4),
// address bits (51:12), and NX (bit 63) are architecturally defined.
const RSVD_PDPTE_MASK: u64 = RSVD_HIGH_MASK | 0x0000_0000_0000_0FE6; // bits 11:5 + bits 2:1

// For 2MiB PDEs, bits 20:13 are reserved (must be 0).
const RSVD_PDE_2M_MASK: u64 = 0x0000_0000_001F_E000;

/// Translate a linear address (treated as 32-bit) using PAE paging.
///
/// `linear` is masked to 32 bits internally.
pub fn translate(
    bus: &mut impl MemoryBus,
    linear: u64,
    access: AccessType,
    cpl: u8,
    cr0: u64,
    cr3: u64,
    efer: u64,
) -> Result<u64, TranslateError> {
    let vaddr = (linear & 0xFFFF_FFFF) as u32;
    let is_write = access == AccessType::Write;
    let is_user = cpl == 3;
    let is_instr = access == AccessType::Execute;

    let pf = |present: bool, write: bool, user: bool, rsvd: bool, instr: bool| {
        TranslateError::PageFault {
            vaddr: vaddr as u64,
            code: (if present { PFEC_P } else { 0 })
                | (if write { PFEC_WR } else { 0 })
                | (if user { PFEC_US } else { 0 })
                | (if rsvd { PFEC_RSVD } else { 0 })
                | (if instr { PFEC_ID } else { 0 }),
        }
    };

    let wp = (cr0 & CR0_WP) != 0;
    let nx_enabled = (efer & EFER_NXE) != 0;

    let pdpt_base = cr3 & CR3_PDPT_MASK;
    let pdpt_index = ((vaddr >> 30) & 0x3) as u64;
    let pdpte_addr = pdpt_base + pdpt_index * 8;
    let pdpte = bus.read_u64(pdpte_addr);

    if (pdpte & PTE_P) == 0 {
        return Err(pf(false, is_write, is_user, false, is_instr));
    }
    if (pdpte & RSVD_PDPTE_MASK) != 0 {
        return Err(pf(true, is_write, is_user, true, is_instr));
    }

    let mut nx_fault = nx_enabled && is_instr && (pdpte & PTE_NX) != 0;

    let pd_base = pdpte & ADDR_MASK_4K;
    let pd_index = ((vaddr >> 21) & 0x1FF) as u64;
    let pde_addr = pd_base + pd_index * 8;
    let pde = bus.read_u64(pde_addr);

    if (pde & PTE_P) == 0 {
        return Err(pf(false, is_write, is_user, false, is_instr));
    }

    let pde_ps = (pde & PDE_PS) != 0;
    let rsvd = (pde & RSVD_HIGH_MASK) != 0 || (pde_ps && (pde & RSVD_PDE_2M_MASK) != 0);
    if rsvd {
        return Err(pf(true, is_write, is_user, true, is_instr));
    }

    if nx_enabled && is_instr && (pde & PTE_NX) != 0 {
        nx_fault = true;
    }

    let pde_rw = (pde & PTE_RW) != 0;
    let pde_us = (pde & PTE_US) != 0;

    if pde_ps {
        // 2MiB page.
        if is_user && !pde_us {
            return Err(pf(true, is_write, true, false, is_instr));
        }

        if is_write && !pde_rw && (is_user || wp) {
            return Err(pf(true, true, is_user, false, is_instr));
        }

        if nx_fault {
            return Err(pf(true, is_write, is_user, false, is_instr));
        }

        let paddr = (pde & ADDR_MASK_2M) | ((vaddr & PAGE_OFFSET_2M) as u64);

        let mut new_pde = pde | PTE_A;
        if is_write {
            new_pde |= PTE_D;
        }
        if new_pde != pde {
            bus.write_u64(pde_addr, new_pde);
        }

        return Ok(paddr);
    }

    // 4KiB pages: PDE points to page table.
    let pt_base = pde & ADDR_MASK_4K;
    let pt_index = ((vaddr >> 12) & 0x1FF) as u64;
    let pte_addr = pt_base + pt_index * 8;
    let pte = bus.read_u64(pte_addr);

    if (pte & PTE_P) == 0 {
        return Err(pf(false, is_write, is_user, false, is_instr));
    }
    if (pte & RSVD_HIGH_MASK) != 0 {
        return Err(pf(true, is_write, is_user, true, is_instr));
    }

    if nx_enabled && is_instr && (pte & PTE_NX) != 0 {
        nx_fault = true;
    }

    let pte_rw = (pte & PTE_RW) != 0;
    let pte_us = (pte & PTE_US) != 0;

    let eff_rw = pde_rw && pte_rw;
    let eff_us = pde_us && pte_us;

    if is_user && !eff_us {
        return Err(pf(true, is_write, true, false, is_instr));
    }

    if is_write && !eff_rw && (is_user || wp) {
        return Err(pf(true, true, is_user, false, is_instr));
    }

    if nx_fault {
        return Err(pf(true, is_write, is_user, false, is_instr));
    }

    let paddr = (pte & ADDR_MASK_4K) | ((vaddr & PAGE_OFFSET_4K) as u64);

    let new_pde = pde | PTE_A;
    if new_pde != pde {
        bus.write_u64(pde_addr, new_pde);
    }

    let mut new_pte = pte | PTE_A;
    if is_write {
        new_pte |= PTE_D;
    }
    if new_pte != pte {
        bus.write_u64(pte_addr, new_pte);
    }

    Ok(paddr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmu::{TranslateError, PFEC_ID, PFEC_P, PFEC_RSVD, PFEC_WR};

    struct TestBus {
        mem: Vec<u8>,
    }

    impl TestBus {
        fn new(size: usize) -> Self {
            Self { mem: vec![0; size] }
        }

        fn read_u64_phys(&self, paddr: u64) -> u64 {
            let mut buf = [0u8; 8];
            let start = paddr as usize;
            for (i, b) in buf.iter_mut().enumerate() {
                *b = self.mem.get(start + i).copied().unwrap_or(0);
            }
            u64::from_le_bytes(buf)
        }

        fn write_u64_phys(&mut self, paddr: u64, val: u64) {
            let bytes = val.to_le_bytes();
            let start = paddr as usize;
            for (i, b) in bytes.iter().enumerate() {
                if let Some(slot) = self.mem.get_mut(start + i) {
                    *slot = *b;
                }
            }
        }
    }

    impl MemoryBus for TestBus {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let start = paddr as usize;
            for (i, b) in buf.iter_mut().enumerate() {
                *b = self.mem.get(start + i).copied().unwrap_or(0);
            }
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let start = paddr as usize;
            for (i, b) in buf.iter().enumerate() {
                if let Some(slot) = self.mem.get_mut(start + i) {
                    *slot = *b;
                }
            }
        }
    }

    fn assert_pf(err: TranslateError, addr: u32, code: u32) {
        match err {
            TranslateError::PageFault {
                vaddr: got_addr,
                code: got_code,
            } => {
                assert_eq!(got_addr, addr as u64);
                assert_eq!(got_code, code);
            }
            other => panic!("expected page fault, got {other:?}"),
        }
    }

    #[test]
    fn maps_4k_page_sets_accessed_dirty_and_supports_phys_above_4g() {
        let mut bus = TestBus::new(0x20_000);

        let cr3 = 0x1000u64;
        let pd_base = 0x2000u64;
        let pt_base = 0x3000u64;

        let vaddr = 0x1234_5678u64;
        let phys_page = 0x1_0000_0000u64;

        // PDPT[0] -> PD.
        bus.write_u64_phys(cr3, PTE_P | (pd_base & ADDR_MASK_4K));

        let pd_index = ((vaddr as u32) >> 21) & 0x1FF;
        let pt_index = ((vaddr as u32) >> 12) & 0x1FF;

        let pde_addr = pd_base + (pd_index as u64) * 8;
        let pte_addr = pt_base + (pt_index as u64) * 8;

        bus.write_u64_phys(pde_addr, PTE_P | PTE_RW | PTE_US | (pt_base & ADDR_MASK_4K));
        bus.write_u64_phys(
            pte_addr,
            PTE_P | PTE_RW | PTE_US | (phys_page & ADDR_MASK_4K),
        );

        let out = translate(
            &mut bus,
            0x1_0000_0000 + vaddr,
            AccessType::Write,
            0,
            0,
            cr3,
            0,
        )
        .unwrap();
        assert_eq!(out, phys_page + (vaddr & PAGE_OFFSET_4K as u64));

        let pde_after = bus.read_u64_phys(pde_addr);
        let pte_after = bus.read_u64_phys(pte_addr);

        assert_ne!(pde_after & PTE_A, 0);
        assert_eq!(pde_after & PTE_D, 0);

        assert_ne!(pte_after & PTE_A, 0);
        assert_ne!(pte_after & PTE_D, 0);
    }

    #[test]
    fn maps_2m_page_and_sets_accessed_dirty() {
        let mut bus = TestBus::new(0x20_000);

        let cr3 = 0x1000u64;
        let pd_base = 0x2000u64;

        let vaddr = 0x2345_6789u64;
        let phys_base = 0x0080_0000u64; // 8MiB, 2MiB-aligned

        bus.write_u64_phys(cr3, PTE_P | (pd_base & ADDR_MASK_4K));

        let pd_index = ((vaddr as u32) >> 21) & 0x1FF;
        let pde_addr = pd_base + (pd_index as u64) * 8;
        bus.write_u64_phys(
            pde_addr,
            PTE_P | PTE_RW | PTE_US | PDE_PS | (phys_base & ADDR_MASK_2M),
        );

        let out = translate(&mut bus, vaddr, AccessType::Write, 0, 0, cr3, 0).unwrap();
        assert_eq!(out, phys_base + (vaddr & PAGE_OFFSET_2M as u64));

        let pde_after = bus.read_u64_phys(pde_addr);
        assert_ne!(pde_after & PTE_A, 0);
        assert_ne!(pde_after & PTE_D, 0);
    }

    #[test]
    fn nx_fault_when_nxe_enabled_sets_id_bit() {
        let mut bus = TestBus::new(0x20_000);

        let cr3 = 0x1000u64;
        let pd_base = 0x2000u64;
        let pt_base = 0x3000u64;

        let vaddr = 0x0010_2000u64;

        bus.write_u64_phys(cr3, PTE_P | (pd_base & ADDR_MASK_4K));

        let pd_index = ((vaddr as u32) >> 21) & 0x1FF;
        let pt_index = ((vaddr as u32) >> 12) & 0x1FF;
        let pde_addr = pd_base + (pd_index as u64) * 8;
        let pte_addr = pt_base + (pt_index as u64) * 8;

        bus.write_u64_phys(pde_addr, PTE_P | PTE_RW | PTE_US | (pt_base & ADDR_MASK_4K));
        bus.write_u64_phys(
            pte_addr,
            PTE_P | PTE_RW | PTE_US | PTE_NX | (0x0040_0000u64 & ADDR_MASK_4K),
        );

        let err = translate(&mut bus, vaddr, AccessType::Execute, 0, 0, cr3, EFER_NXE).unwrap_err();

        assert_pf(err, vaddr as u32, PFEC_P | PFEC_ID);
    }

    #[test]
    fn rsvd_fault_by_setting_2m_alignment_bits() {
        let mut bus = TestBus::new(0x20_000);

        let cr3 = 0x1000u64;
        let pd_base = 0x2000u64;
        let vaddr = 0x0020_1000u64;

        bus.write_u64_phys(cr3, PTE_P | (pd_base & ADDR_MASK_4K));

        let pd_index = ((vaddr as u32) >> 21) & 0x1FF;
        let pde_addr = pd_base + (pd_index as u64) * 8;

        let phys_base = 0x0060_0000u64 & !0x1F_FFFF;
        bus.write_u64_phys(
            pde_addr,
            PTE_P | PTE_RW | PDE_PS | (phys_base & ADDR_MASK_2M) | (1 << 13),
        );

        let err = translate(&mut bus, vaddr, AccessType::Read, 0, 0, cr3, 0).unwrap_err();
        assert_pf(err, vaddr as u32, PFEC_P | PFEC_RSVD);
    }

    #[test]
    fn supervisor_write_to_ro_page_respects_wp() {
        let mut bus = TestBus::new(0x20_000);

        let cr3 = 0x1000u64;
        let pd_base = 0x2000u64;
        let pt_base = 0x3000u64;

        let vaddr = 0x0000_1000u64;

        bus.write_u64_phys(cr3, PTE_P | (pd_base & ADDR_MASK_4K));

        // Use index 0/1 for simplicity.
        bus.write_u64_phys(pd_base, PTE_P | (pt_base & ADDR_MASK_4K)); // RW=0
        bus.write_u64_phys(pt_base + 8, PTE_P | (0x2000u64 & ADDR_MASK_4K)); // RW=0

        // WP=0: supervisor writes succeed.
        translate(&mut bus, vaddr, AccessType::Write, 0, 0, cr3, 0).unwrap();

        // WP=1: supervisor writes fault.
        let err = translate(&mut bus, vaddr, AccessType::Write, 0, CR0_WP, cr3, 0).unwrap_err();
        assert_pf(err, vaddr as u32, PFEC_P | PFEC_WR);
    }
}
