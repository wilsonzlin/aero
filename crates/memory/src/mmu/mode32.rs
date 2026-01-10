//! 32-bit (non-PAE) page table walk (CR0.PG=1, CR4.PAE=0).
//!
//! Supports 4KiB pages and 4MiB pages (when `CR4.PSE=1` and `PDE.PS=1`).

use crate::bus::MemoryBus;
use crate::mmu::{AccessType, PageFault, CR0_WP, CR4_PSE};

const PTE_P: u32 = 1 << 0;
const PTE_RW: u32 = 1 << 1;
const PTE_US: u32 = 1 << 2;
const PTE_A: u32 = 1 << 5;
const PTE_D: u32 = 1 << 6;
const PDE_PS: u32 = 1 << 7;

const CR3_PD_MASK: u32 = 0xFFFF_F000;

const PDE_ADDR_MASK_4K: u32 = 0xFFFF_F000;
const PTE_ADDR_MASK_4K: u32 = 0xFFFF_F000;
const PDE_ADDR_MASK_4M: u32 = 0xFFC0_0000;
const PAGE_OFFSET_MASK_4K: u32 = 0x0000_0FFF;
const PAGE_OFFSET_MASK_4M: u32 = 0x003F_FFFF;

const RESERVED_PDE_4M_MASK: u32 = 0x003F_E000; // bits 21:13

/// Translate a linear address (treated as 32-bit) using 32-bit paging.
///
/// `linear` is masked to 32 bits internally.
pub fn translate(
    bus: &mut impl MemoryBus,
    linear: u64,
    access: AccessType,
    cpl: u8,
    cr0: u32,
    cr3: u32,
    cr4: u32,
    _efer: u64,
) -> Result<u64, PageFault> {
    let vaddr = (linear & 0xFFFF_FFFF) as u32;
    let is_write = access == AccessType::Write;
    let is_user = cpl == 3;
    let is_instr = access == AccessType::Execute;

    let pd_base = (cr3 & CR3_PD_MASK) as u64;
    let pde_index = ((vaddr >> 22) & 0x3FF) as u64;
    let pde_addr = pd_base + pde_index * 4;
    let pde = bus.read_u32(pde_addr);

    if (pde & PTE_P) == 0 {
        return Err(PageFault::new(vaddr, false, is_write, is_user, false, is_instr));
    }

    let pde_rw = (pde & PTE_RW) != 0;
    let pde_us = (pde & PTE_US) != 0;

    let pse_enabled = (cr4 & CR4_PSE) != 0;
    let pde_ps = (pde & PDE_PS) != 0;
    if pde_ps {
        if !pse_enabled {
            return Err(PageFault::new(vaddr, true, is_write, is_user, true, is_instr));
        }

        if (pde & RESERVED_PDE_4M_MASK) != 0 {
            return Err(PageFault::new(vaddr, true, is_write, is_user, true, is_instr));
        }

        if is_user && !pde_us {
            return Err(PageFault::new(vaddr, true, is_write, true, false, is_instr));
        }

        if is_write && !pde_rw && (is_user || (cr0 & CR0_WP) != 0) {
            return Err(PageFault::new(vaddr, true, true, is_user, false, is_instr));
        }

        let paddr = ((pde & PDE_ADDR_MASK_4M) as u64) | ((vaddr & PAGE_OFFSET_MASK_4M) as u64);

        let mut new_pde = pde | PTE_A;
        if is_write {
            new_pde |= PTE_D;
        }
        if new_pde != pde {
            bus.write_u32(pde_addr, new_pde);
        }

        return Ok(paddr);
    }

    // 4KiB pages: PDE points to page table.
    let pt_base = (pde & PDE_ADDR_MASK_4K) as u64;
    let pte_index = ((vaddr >> 12) & 0x3FF) as u64;
    let pte_addr = pt_base + pte_index * 4;
    let pte = bus.read_u32(pte_addr);

    if (pte & PTE_P) == 0 {
        return Err(PageFault::new(vaddr, false, is_write, is_user, false, is_instr));
    }

    let pte_rw = (pte & PTE_RW) != 0;
    let pte_us = (pte & PTE_US) != 0;
    let eff_rw = pde_rw && pte_rw;
    let eff_us = pde_us && pte_us;

    if is_user && !eff_us {
        return Err(PageFault::new(vaddr, true, is_write, true, false, is_instr));
    }

    if is_write && !eff_rw && (is_user || (cr0 & CR0_WP) != 0) {
        return Err(PageFault::new(vaddr, true, true, is_user, false, is_instr));
    }

    let paddr =
        ((pte & PTE_ADDR_MASK_4K) as u64) | ((vaddr & PAGE_OFFSET_MASK_4K) as u64);

    // Accessed/dirty updates are performed on successful translations.
    let new_pde = pde | PTE_A;
    if new_pde != pde {
        bus.write_u32(pde_addr, new_pde);
    }

    let mut new_pte = pte | PTE_A;
    if is_write {
        new_pte |= PTE_D;
    }
    if new_pte != pte {
        bus.write_u32(pte_addr, new_pte);
    }

    Ok(paddr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmu::{AccessType, PageFault, CR0_PG, CR0_WP, CR4_PSE};

    struct TestBus {
        mem: Vec<u8>,
    }

    impl TestBus {
        fn new(size: usize) -> Self {
            Self { mem: vec![0; size] }
        }

        fn read_u32_phys(&self, paddr: u64) -> u32 {
            let mut buf = [0u8; 4];
            let start = paddr as usize;
            for (i, b) in buf.iter_mut().enumerate() {
                *b = self.mem.get(start + i).copied().unwrap_or(0);
            }
            u32::from_le_bytes(buf)
        }

        fn write_u32_phys(&mut self, paddr: u64, val: u32) {
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

    fn assert_pf(pf: PageFault, addr: u32, error_code: u32) {
        assert_eq!(pf.addr, addr);
        assert_eq!(pf.error_code, error_code);
    }

    #[test]
    fn maps_4k_page_and_sets_accessed_dirty() {
        let mut bus = TestBus::new(0x10_000);

        let cr3 = 0x1000u32;
        let pd = cr3 as u64;
        let pt = 0x2000u64;

        let vaddr = 0x0040_1000u64;
        let pde_index = ((vaddr as u32) >> 22) & 0x3FF;
        let pte_index = ((vaddr as u32) >> 12) & 0x3FF;

        let pde_addr = pd + (pde_index as u64) * 4;
        let pte_addr = pt + (pte_index as u64) * 4;

        let pde = (pt as u32) | PTE_P | PTE_RW | PTE_US;
        let pte = 0x3000u32 | PTE_P | PTE_RW | PTE_US;
        bus.write_u32_phys(pde_addr, pde);
        bus.write_u32_phys(pte_addr, pte);

        let paddr = translate(
            &mut bus,
            vaddr,
            AccessType::Read,
            3,
            CR0_PG,
            cr3,
            0,
            0,
        )
        .unwrap();
        assert_eq!(paddr, 0x3000);

        let pde_after = bus.read_u32_phys(pde_addr);
        let pte_after = bus.read_u32_phys(pte_addr);
        assert_ne!(pde_after & PTE_A, 0);
        assert_ne!(pte_after & PTE_A, 0);
        assert_eq!(pte_after & PTE_D, 0);

        let _ = translate(
            &mut bus,
            vaddr,
            AccessType::Write,
            3,
            CR0_PG,
            cr3,
            0,
            0,
        )
        .unwrap();
        let pte_after_write = bus.read_u32_phys(pte_addr);
        assert_ne!(pte_after_write & PTE_D, 0);
    }

    #[test]
    fn maps_4m_page_when_pse_and_ps_set() {
        let mut bus = TestBus::new(0x10_000);
        let cr3 = 0x1000u32;
        let pd = cr3 as u64;

        let vaddr = 0x0400_1234u64;
        let pde_index = ((vaddr as u32) >> 22) & 0x3FF;
        let pde_addr = pd + (pde_index as u64) * 4;

        let phys_base = 0x0200_0000u32;
        let pde = (phys_base & PDE_ADDR_MASK_4M) | PTE_P | PTE_RW | PTE_US | PDE_PS;
        bus.write_u32_phys(pde_addr, pde);

        let paddr = translate(
            &mut bus,
            vaddr,
            AccessType::Read,
            3,
            CR0_PG,
            cr3,
            CR4_PSE,
            0,
        )
        .unwrap();
        assert_eq!(paddr, 0x0200_0000u64 + (vaddr & PAGE_OFFSET_MASK_4M as u64));

        let pde_after = bus.read_u32_phys(pde_addr);
        assert_ne!(pde_after & PTE_A, 0);
    }

    #[test]
    fn user_access_to_supervisor_page_faults() {
        let mut bus = TestBus::new(0x10_000);
        let cr3 = 0x1000u32;
        let pd = cr3 as u64;
        let pt = 0x2000u64;

        let vaddr = 0x0040_0000u64;
        let pde_index = ((vaddr as u32) >> 22) & 0x3FF;
        let pte_index = ((vaddr as u32) >> 12) & 0x3FF;
        let pde_addr = pd + (pde_index as u64) * 4;
        let pte_addr = pt + (pte_index as u64) * 4;

        // Supervisor-only mapping.
        let pde = (pt as u32) | PTE_P | PTE_RW;
        let pte = 0x3000u32 | PTE_P | PTE_RW;
        bus.write_u32_phys(pde_addr, pde);
        bus.write_u32_phys(pte_addr, pte);

        let err = translate(
            &mut bus,
            vaddr,
            AccessType::Read,
            3,
            CR0_PG,
            cr3,
            0,
            0,
        )
        .unwrap_err();
        assert_pf(
            err,
            vaddr as u32,
            PageFault::EC_P | PageFault::EC_US,
        );
    }

    #[test]
    fn supervisor_write_to_ro_page_respects_wp() {
        let mut bus = TestBus::new(0x10_000);
        let cr3 = 0x1000u32;
        let pd = cr3 as u64;
        let pt = 0x2000u64;

        let vaddr = 0x0080_0000u64;
        let pde_index = ((vaddr as u32) >> 22) & 0x3FF;
        let pte_index = ((vaddr as u32) >> 12) & 0x3FF;
        let pde_addr = pd + (pde_index as u64) * 4;
        let pte_addr = pt + (pte_index as u64) * 4;

        // Read-only mapping (RW=0).
        let pde = (pt as u32) | PTE_P | PTE_US;
        let pte = 0x3000u32 | PTE_P | PTE_US;
        bus.write_u32_phys(pde_addr, pde);
        bus.write_u32_phys(pte_addr, pte);

        // WP=0: supervisor writes succeed.
        let paddr = translate(
            &mut bus,
            vaddr,
            AccessType::Write,
            0,
            CR0_PG,
            cr3,
            0,
            0,
        )
        .unwrap();
        assert_eq!(paddr, 0x3000);

        // WP=1: supervisor writes fault.
        let err = translate(
            &mut bus,
            vaddr,
            AccessType::Write,
            0,
            CR0_PG | CR0_WP,
            cr3,
            0,
            0,
        )
        .unwrap_err();
        assert_pf(
            err,
            vaddr as u32,
            PageFault::EC_P | PageFault::EC_WR,
        );
    }

    #[test]
    fn pde_ps_with_pse_disabled_is_reserved_bit_violation() {
        let mut bus = TestBus::new(0x10_000);
        let cr3 = 0x1000u32;
        let pd = cr3 as u64;

        let vaddr = 0x0000_2000u64;
        let pde_index = ((vaddr as u32) >> 22) & 0x3FF;
        let pde_addr = pd + (pde_index as u64) * 4;

        let pde = PTE_P | PDE_PS;
        bus.write_u32_phys(pde_addr, pde);

        let err = translate(
            &mut bus,
            vaddr,
            AccessType::Read,
            0,
            CR0_PG,
            cr3,
            0,
            0,
        )
        .unwrap_err();
        assert_pf(err, vaddr as u32, PageFault::EC_P | PageFault::EC_RSVD);
    }
}
