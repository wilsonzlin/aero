use crate::bus::MemoryBus;

use super::{
    AccessType, PageSize, TranslateError, TranslateResult, CR0_WP, EFER_NXE, PFEC_ID, PFEC_P,
    PFEC_RSVD, PFEC_US, PFEC_WR,
};

const CR3_PML4_BASE_MASK: u64 = 0x000F_FFFF_FFFF_F000;

const ENTRY_PRESENT: u64 = 1 << 0;
const ENTRY_RW: u64 = 1 << 1;
const ENTRY_US: u64 = 1 << 2;
const ENTRY_ACCESSED: u64 = 1 << 5;
const ENTRY_DIRTY: u64 = 1 << 6;
const ENTRY_PS: u64 = 1 << 7;
const ENTRY_NX: u64 = 1 << 63;

const ADDR_MASK_4K: u64 = 0x000F_FFFF_FFFF_F000;
const ADDR_MASK_2M: u64 = 0x000F_FFFF_FFE0_0000;
const ADDR_MASK_1G: u64 = 0x000F_FFFF_C000_0000;

const RSVD_MASK_PML4E: u64 = ENTRY_PS;
const RSVD_MASK_PDPTE_1G: u64 = 0x3FFF_E000; // bits 29:13 must be 0 for 1GiB pages
const RSVD_MASK_PDE_2M: u64 = 0x001F_E000; // bits 20:13 must be 0 for 2MiB pages

#[inline]
fn is_canonical_4level(vaddr: u64) -> bool {
    let sign = (vaddr >> 47) & 1;
    let upper = vaddr >> 48;
    if sign == 0 {
        upper == 0
    } else {
        upper == 0xFFFF
    }
}

#[inline]
fn page_fault_code(access: AccessType, cpl: u8, present: bool, rsvd: bool) -> u32 {
    let mut code = 0u32;
    if present {
        code |= PFEC_P;
    }
    if access.is_write() {
        code |= PFEC_WR;
    }
    if cpl == 3 {
        code |= PFEC_US;
    }
    if rsvd {
        code |= PFEC_RSVD;
    }
    if access.is_execute() {
        code |= PFEC_ID;
    }
    code
}

#[inline]
fn pf_not_present(vaddr: u64, access: AccessType, cpl: u8) -> TranslateError {
    TranslateError::PageFault {
        vaddr,
        code: page_fault_code(access, cpl, false, false),
    }
}

#[inline]
fn pf_protection(vaddr: u64, access: AccessType, cpl: u8) -> TranslateError {
    TranslateError::PageFault {
        vaddr,
        code: page_fault_code(access, cpl, true, false),
    }
}

#[inline]
fn pf_rsvd(vaddr: u64, access: AccessType, cpl: u8) -> TranslateError {
    TranslateError::PageFault {
        vaddr,
        code: page_fault_code(access, cpl, true, true),
    }
}

#[inline]
fn set_entry_bits<B: MemoryBus>(bus: &mut B, paddr: u64, entry: u64, mask: u64) -> u64 {
    let new_entry = entry | mask;
    if new_entry != entry {
        bus.write_u64(paddr, new_entry);
    }
    new_entry
}

#[inline]
fn check_nx_reserved(entry: u64, nxe: bool) -> bool {
    !nxe && (entry & ENTRY_NX != 0)
}

#[inline]
fn check_user_violation(entry: u64, cpl: u8) -> bool {
    cpl == 3 && (entry & ENTRY_US == 0)
}

#[inline]
fn check_write_violation(entry: u64, access: AccessType, cpl: u8, cr0_wp: bool) -> bool {
    if !access.is_write() {
        return false;
    }
    if entry & ENTRY_RW != 0 {
        return false;
    }
    cpl == 3 || cr0_wp
}

#[inline]
fn check_execute_violation(entry: u64, access: AccessType, nxe: bool) -> bool {
    access.is_execute() && nxe && (entry & ENTRY_NX != 0)
}

/// Translate a virtual address using 4-level IA-32e (x86-64) paging.
///
/// This models the paging behavior used by Windows 7 x64 (4-level paging, 48-bit canonical
/// addresses) including 4KiB, 2MiB, and 1GiB pages, NX, and Accessed/Dirty updates.
pub fn translate_4level<B: MemoryBus>(
    bus: &mut B,
    vaddr: u64,
    access: AccessType,
    cr3: u64,
    cr0: u64,
    efer: u64,
    cpl: u8,
) -> Result<TranslateResult, TranslateError> {
    if !is_canonical_4level(vaddr) {
        return Err(TranslateError::GeneralProtection { vaddr });
    }

    let cr0_wp = (cr0 & CR0_WP) != 0;
    let nxe = (efer & EFER_NXE) != 0;

    let pml4_base = cr3 & CR3_PML4_BASE_MASK;

    let pml4_index = (vaddr >> 39) & 0x1FF;
    let pdpt_index = (vaddr >> 30) & 0x1FF;
    let pd_index = (vaddr >> 21) & 0x1FF;
    let pt_index = (vaddr >> 12) & 0x1FF;

    // Level 4: PML4E
    let pml4e_addr = pml4_base + (pml4_index * 8);
    let mut pml4e = bus.read_u64(pml4e_addr);
    if pml4e & ENTRY_PRESENT == 0 {
        return Err(pf_not_present(vaddr, access, cpl));
    }
    if pml4e & RSVD_MASK_PML4E != 0 || check_nx_reserved(pml4e, nxe) {
        return Err(pf_rsvd(vaddr, access, cpl));
    }
    let user_violation = check_user_violation(pml4e, cpl);
    let write_violation = check_write_violation(pml4e, access, cpl, cr0_wp);
    let exec_violation = check_execute_violation(pml4e, access, nxe);
    pml4e = set_entry_bits(bus, pml4e_addr, pml4e, ENTRY_ACCESSED);
    if user_violation || write_violation || exec_violation {
        return Err(pf_protection(vaddr, access, cpl));
    }

    let pdpt_base = pml4e & ADDR_MASK_4K;

    // Level 3: PDPTE
    let pdpte_addr = pdpt_base + (pdpt_index * 8);
    let mut pdpte = bus.read_u64(pdpte_addr);
    if pdpte & ENTRY_PRESENT == 0 {
        return Err(pf_not_present(vaddr, access, cpl));
    }
    if check_nx_reserved(pdpte, nxe) {
        return Err(pf_rsvd(vaddr, access, cpl));
    }

    if pdpte & ENTRY_PS != 0 {
        // 1GiB page
        if pdpte & RSVD_MASK_PDPTE_1G != 0 {
            return Err(pf_rsvd(vaddr, access, cpl));
        }

        let user_violation = check_user_violation(pdpte, cpl);
        let write_violation = check_write_violation(pdpte, access, cpl, cr0_wp);
        let exec_violation = check_execute_violation(pdpte, access, nxe);
        let update_mask = if user_violation || write_violation || exec_violation {
            ENTRY_ACCESSED
        } else if access.is_write() {
            ENTRY_ACCESSED | ENTRY_DIRTY
        } else {
            ENTRY_ACCESSED
        };

        pdpte = set_entry_bits(bus, pdpte_addr, pdpte, update_mask);
        if user_violation || write_violation || exec_violation {
            return Err(pf_protection(vaddr, access, cpl));
        }

        let page_base = pdpte & ADDR_MASK_1G;
        let page_off = vaddr & 0x3FFF_FFFF;
        return Ok(TranslateResult {
            paddr: page_base + page_off,
            page_size: PageSize::Size1G,
        });
    }

    let user_violation = check_user_violation(pdpte, cpl);
    let write_violation = check_write_violation(pdpte, access, cpl, cr0_wp);
    let exec_violation = check_execute_violation(pdpte, access, nxe);
    pdpte = set_entry_bits(bus, pdpte_addr, pdpte, ENTRY_ACCESSED);
    if user_violation || write_violation || exec_violation {
        return Err(pf_protection(vaddr, access, cpl));
    }

    let pd_base = pdpte & ADDR_MASK_4K;

    // Level 2: PDE
    let pde_addr = pd_base + (pd_index * 8);
    let mut pde = bus.read_u64(pde_addr);
    if pde & ENTRY_PRESENT == 0 {
        return Err(pf_not_present(vaddr, access, cpl));
    }
    if check_nx_reserved(pde, nxe) {
        return Err(pf_rsvd(vaddr, access, cpl));
    }

    if pde & ENTRY_PS != 0 {
        // 2MiB page
        if pde & RSVD_MASK_PDE_2M != 0 {
            return Err(pf_rsvd(vaddr, access, cpl));
        }

        let user_violation = check_user_violation(pde, cpl);
        let write_violation = check_write_violation(pde, access, cpl, cr0_wp);
        let exec_violation = check_execute_violation(pde, access, nxe);
        let update_mask = if user_violation || write_violation || exec_violation {
            ENTRY_ACCESSED
        } else if access.is_write() {
            ENTRY_ACCESSED | ENTRY_DIRTY
        } else {
            ENTRY_ACCESSED
        };

        pde = set_entry_bits(bus, pde_addr, pde, update_mask);
        if user_violation || write_violation || exec_violation {
            return Err(pf_protection(vaddr, access, cpl));
        }

        let page_base = pde & ADDR_MASK_2M;
        let page_off = vaddr & 0x1F_FFFF;
        return Ok(TranslateResult {
            paddr: page_base + page_off,
            page_size: PageSize::Size2M,
        });
    }

    let user_violation = check_user_violation(pde, cpl);
    let write_violation = check_write_violation(pde, access, cpl, cr0_wp);
    let exec_violation = check_execute_violation(pde, access, nxe);
    pde = set_entry_bits(bus, pde_addr, pde, ENTRY_ACCESSED);
    if user_violation || write_violation || exec_violation {
        return Err(pf_protection(vaddr, access, cpl));
    }

    let pt_base = pde & ADDR_MASK_4K;

    // Level 1: PTE
    let pte_addr = pt_base + (pt_index * 8);
    let mut pte = bus.read_u64(pte_addr);
    if pte & ENTRY_PRESENT == 0 {
        return Err(pf_not_present(vaddr, access, cpl));
    }
    if check_nx_reserved(pte, nxe) {
        return Err(pf_rsvd(vaddr, access, cpl));
    }

    let user_violation = check_user_violation(pte, cpl);
    let write_violation = check_write_violation(pte, access, cpl, cr0_wp);
    let exec_violation = check_execute_violation(pte, access, nxe);
    let update_mask = if user_violation || write_violation || exec_violation {
        ENTRY_ACCESSED
    } else if access.is_write() {
        ENTRY_ACCESSED | ENTRY_DIRTY
    } else {
        ENTRY_ACCESSED
    };

    pte = set_entry_bits(bus, pte_addr, pte, update_mask);
    if user_violation || write_violation || exec_violation {
        return Err(pf_protection(vaddr, access, cpl));
    }

    let page_base = pte & ADDR_MASK_4K;
    let page_off = vaddr & 0xFFF;
    Ok(TranslateResult {
        paddr: page_base + page_off,
        page_size: PageSize::Size4K,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBus {
        mem: Vec<u8>,
    }

    impl TestBus {
        fn new(size: usize) -> Self {
            Self { mem: vec![0; size] }
        }

        fn write_u64_at(&mut self, paddr: u64, value: u64) {
            let start = paddr as usize;
            self.mem[start..start + 8].copy_from_slice(&value.to_le_bytes());
        }

        fn read_u64_at(&self, paddr: u64) -> u64 {
            let start = paddr as usize;
            u64::from_le_bytes(self.mem[start..start + 8].try_into().unwrap())
        }
    }

    impl MemoryBus for TestBus {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let start = paddr as usize;
            let end = start + buf.len();
            buf.copy_from_slice(&self.mem[start..end]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let start = paddr as usize;
            let end = start + buf.len();
            self.mem[start..end].copy_from_slice(buf);
        }
    }

    fn build_4k_mapping(
        bus: &mut TestBus,
        vaddr: u64,
        page_paddr: u64,
        flags: u64,
    ) -> (u64, u64, u64, u64) {
        let pml4_base = 0x1000u64;
        let pdpt_base = 0x2000u64;
        let pd_base = 0x3000u64;
        let pt_base = 0x4000u64;

        let pml4_index = (vaddr >> 39) & 0x1FF;
        let pdpt_index = (vaddr >> 30) & 0x1FF;
        let pd_index = (vaddr >> 21) & 0x1FF;
        let pt_index = (vaddr >> 12) & 0x1FF;

        bus.write_u64_at(pml4_base + pml4_index * 8, pdpt_base | flags);
        bus.write_u64_at(pdpt_base + pdpt_index * 8, pd_base | flags);
        bus.write_u64_at(pd_base + pd_index * 8, pt_base | flags);
        bus.write_u64_at(pt_base + pt_index * 8, page_paddr | flags);

        (pml4_base, pdpt_base, pd_base, pt_base)
    }

    #[test]
    fn canonical_address_enforced() {
        let mut bus = TestBus::new(0x8000);
        let non_canonical = 0x0001_0000_0000_0000u64;
        let err = translate_4level(
            &mut bus,
            non_canonical,
            AccessType::Read,
            0,
            0,
            0,
            0,
        )
        .unwrap_err();
        assert_eq!(err, TranslateError::GeneralProtection { vaddr: non_canonical });
    }

    #[test]
    fn translate_4k_and_updates_ad() {
        let mut bus = TestBus::new(0x10_000);
        let vaddr = 0x0000_0000_0040_1234u64;
        let page_paddr = 0x0000_0000_0080_0000u64;
        let flags = ENTRY_PRESENT | ENTRY_RW | ENTRY_US;

        let (pml4_base, pdpt_base, pd_base, pt_base) =
            build_4k_mapping(&mut bus, vaddr, page_paddr, flags);
        let cr3 = pml4_base;

        let res = translate_4level(
            &mut bus,
            vaddr,
            AccessType::Read,
            cr3,
            CR0_WP,
            EFER_NXE,
            3,
        )
        .unwrap();
        assert_eq!(res.paddr, page_paddr + (vaddr & 0xFFF));
        assert_eq!(res.page_size, PageSize::Size4K);

        let pml4e = bus.read_u64_at(pml4_base + (((vaddr >> 39) & 0x1FF) * 8));
        let pdpte = bus.read_u64_at(pdpt_base + (((vaddr >> 30) & 0x1FF) * 8));
        let pde = bus.read_u64_at(pd_base + (((vaddr >> 21) & 0x1FF) * 8));
        let pte = bus.read_u64_at(pt_base + (((vaddr >> 12) & 0x1FF) * 8));

        assert!(pml4e & ENTRY_ACCESSED != 0);
        assert!(pdpte & ENTRY_ACCESSED != 0);
        assert!(pde & ENTRY_ACCESSED != 0);
        assert!(pte & ENTRY_ACCESSED != 0);
        assert!(pte & ENTRY_DIRTY == 0);

        let _ = translate_4level(
            &mut bus,
            vaddr,
            AccessType::Write,
            cr3,
            CR0_WP,
            EFER_NXE,
            3,
        )
        .unwrap();
        let pte_after = bus.read_u64_at(pt_base + (((vaddr >> 12) & 0x1FF) * 8));
        assert!(pte_after & ENTRY_DIRTY != 0);
    }

    #[test]
    fn translate_2m_page() {
        let mut bus = TestBus::new(0x10_000);
        let vaddr = 0x0000_0001_0020_3456u64;
        let page_base = 0x0000_0000_0020_0000u64; // 2MiB aligned
        let flags = ENTRY_PRESENT | ENTRY_RW | ENTRY_US;

        let pml4_base = 0x1000u64;
        let pdpt_base = 0x2000u64;
        let pd_base = 0x3000u64;

        let pml4_index = (vaddr >> 39) & 0x1FF;
        let pdpt_index = (vaddr >> 30) & 0x1FF;
        let pd_index = (vaddr >> 21) & 0x1FF;

        bus.write_u64_at(pml4_base + pml4_index * 8, pdpt_base | flags);
        bus.write_u64_at(pdpt_base + pdpt_index * 8, pd_base | flags);
        bus.write_u64_at(
            pd_base + pd_index * 8,
            page_base | flags | ENTRY_PS, // 2MiB page
        );

        let res = translate_4level(
            &mut bus,
            vaddr,
            AccessType::Write,
            pml4_base,
            CR0_WP,
            EFER_NXE,
            3,
        )
        .unwrap();
        assert_eq!(res.page_size, PageSize::Size2M);
        assert_eq!(res.paddr, page_base + (vaddr & 0x1F_FFFF));

        let pde = bus.read_u64_at(pd_base + pd_index * 8);
        assert!(pde & ENTRY_ACCESSED != 0);
        assert!(pde & ENTRY_DIRTY != 0);
    }

    #[test]
    fn translate_1g_page() {
        let mut bus = TestBus::new(0x10_000);
        let vaddr = 0x0000_0008_1234_5678u64;
        let page_base = 0x0000_0000_4000_0000u64; // 1GiB aligned
        let flags = ENTRY_PRESENT | ENTRY_RW | ENTRY_US;

        let pml4_base = 0x1000u64;
        let pdpt_base = 0x2000u64;

        let pml4_index = (vaddr >> 39) & 0x1FF;
        let pdpt_index = (vaddr >> 30) & 0x1FF;

        bus.write_u64_at(pml4_base + pml4_index * 8, pdpt_base | flags);
        bus.write_u64_at(
            pdpt_base + pdpt_index * 8,
            page_base | flags | ENTRY_PS, // 1GiB page
        );

        let res = translate_4level(
            &mut bus,
            vaddr,
            AccessType::Read,
            pml4_base,
            CR0_WP,
            EFER_NXE,
            3,
        )
        .unwrap();
        assert_eq!(res.page_size, PageSize::Size1G);
        assert_eq!(res.paddr, page_base + (vaddr & 0x3FFF_FFFF));

        let pdpte = bus.read_u64_at(pdpt_base + pdpt_index * 8);
        assert!(pdpte & ENTRY_ACCESSED != 0);
        assert!(pdpte & ENTRY_DIRTY == 0);
    }

    #[test]
    fn nx_execute_fault_sets_id() {
        let mut bus = TestBus::new(0x10_000);
        let vaddr = 0x0000_0000_0040_1000u64;
        let page_paddr = 0x0000_0000_0080_0000u64;
        let flags = ENTRY_PRESENT | ENTRY_RW | ENTRY_US;

        let (pml4_base, pdpt_base, pd_base, pt_base) =
            build_4k_mapping(&mut bus, vaddr, page_paddr, flags);
        let pt_index = (vaddr >> 12) & 0x1FF;
        let pte_addr = pt_base + pt_index * 8;
        let pte = bus.read_u64_at(pte_addr);
        bus.write_u64_at(pte_addr, pte | ENTRY_NX);
        let cr3 = pml4_base;

        let err = translate_4level(
            &mut bus,
            vaddr,
            AccessType::Execute,
            cr3,
            CR0_WP,
            EFER_NXE,
            3,
        )
        .unwrap_err();

        match err {
            TranslateError::PageFault { vaddr: fault_addr, code } => {
                assert_eq!(fault_addr, vaddr);
                assert!(code & PFEC_P != 0);
                assert!(code & PFEC_ID != 0);
                assert!(code & PFEC_US != 0);
                assert!(code & PFEC_WR == 0);
                assert!(code & PFEC_RSVD == 0);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        // Protection faults still set Accessed on entries used.
        let pml4e = bus.read_u64_at(pml4_base + (((vaddr >> 39) & 0x1FF) * 8));
        let pdpte = bus.read_u64_at(pdpt_base + (((vaddr >> 30) & 0x1FF) * 8));
        let pde = bus.read_u64_at(pd_base + (((vaddr >> 21) & 0x1FF) * 8));
        let pte = bus.read_u64_at(pt_base + (((vaddr >> 12) & 0x1FF) * 8));
        assert!(pml4e & ENTRY_ACCESSED != 0);
        assert!(pdpte & ENTRY_ACCESSED != 0);
        assert!(pde & ENTRY_ACCESSED != 0);
        assert!(pte & ENTRY_ACCESSED != 0);
    }

    #[test]
    fn user_supervisor_violation_faults() {
        let mut bus = TestBus::new(0x10_000);
        let vaddr = 0x0000_0000_0040_2000u64;
        let page_paddr = 0x0000_0000_0090_0000u64;

        let pml4_base = 0x1000u64;
        let pdpt_base = 0x2000u64;
        let pd_base = 0x3000u64;
        let pt_base = 0x4000u64;

        let flags_supervisor = ENTRY_PRESENT | ENTRY_RW; // US=0

        let pml4_index = (vaddr >> 39) & 0x1FF;
        let pdpt_index = (vaddr >> 30) & 0x1FF;
        let pd_index = (vaddr >> 21) & 0x1FF;
        let pt_index = (vaddr >> 12) & 0x1FF;

        // Make the violation happen at the top level.
        bus.write_u64_at(pml4_base + pml4_index * 8, pdpt_base | flags_supervisor);
        bus.write_u64_at(pdpt_base + pdpt_index * 8, pd_base | (ENTRY_PRESENT | ENTRY_RW | ENTRY_US));
        bus.write_u64_at(pd_base + pd_index * 8, pt_base | (ENTRY_PRESENT | ENTRY_RW | ENTRY_US));
        bus.write_u64_at(pt_base + pt_index * 8, page_paddr | (ENTRY_PRESENT | ENTRY_RW | ENTRY_US));

        let err = translate_4level(
            &mut bus,
            vaddr,
            AccessType::Read,
            pml4_base,
            CR0_WP,
            EFER_NXE,
            3,
        )
        .unwrap_err();

        match err {
            TranslateError::PageFault { vaddr: fault_addr, code } => {
                assert_eq!(fault_addr, vaddr);
                assert_eq!(code, PFEC_P | PFEC_US);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let pml4e = bus.read_u64_at(pml4_base + pml4_index * 8);
        assert!(pml4e & ENTRY_ACCESSED != 0);
    }

    #[test]
    fn rw_violation_and_wp_semantics() {
        let mut bus = TestBus::new(0x10_000);
        let vaddr = 0x0000_0000_0040_3000u64;
        let page_paddr = 0x0000_0000_00A0_0000u64;

        let flags_user_ro = ENTRY_PRESENT | ENTRY_US; // RW=0

        let (pml4_base, _pdpt_base, _pd_base, pt_base) =
            build_4k_mapping(&mut bus, vaddr, page_paddr, flags_user_ro);

        // User write should fault.
        let err = translate_4level(
            &mut bus,
            vaddr,
            AccessType::Write,
            pml4_base,
            CR0_WP,
            EFER_NXE,
            3,
        )
        .unwrap_err();
        match err {
            TranslateError::PageFault { code, .. } => {
                assert_eq!(code, PFEC_P | PFEC_WR | PFEC_US);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        // Supervisor write with WP=0 should succeed even if RW=0.
        let res = translate_4level(
            &mut bus,
            vaddr,
            AccessType::Write,
            pml4_base,
            0, // CR0.WP=0
            EFER_NXE,
            0,
        )
        .unwrap();
        assert_eq!(res.paddr, page_paddr + (vaddr & 0xFFF));

        let pte_after = bus.read_u64_at(pt_base + (((vaddr >> 12) & 0x1FF) * 8));
        assert!(pte_after & ENTRY_DIRTY != 0);
    }
}
