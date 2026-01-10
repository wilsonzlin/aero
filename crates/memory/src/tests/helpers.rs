use crate::bus::MemoryBus;
use crate::mmu::{
    long, AccessType, Mmu, TranslateError, CR0_PG, CR0_WP, CR4_PAE, CR4_PGE, CR4_PSE, EFER_LME,
    EFER_NXE,
};
use crate::{Bus, Tlb, TlbEntry};

pub const RAM_SIZE: usize = 8 * 1024 * 1024;

pub fn new_bus() -> Bus {
    Bus::new(RAM_SIZE)
}

pub fn new_mmu_legacy32(cr3: u64) -> Mmu {
    let mut mmu = Mmu::default();
    mmu.cr0 = CR0_PG;
    mmu.cr3 = cr3;
    mmu.cr4 = CR4_PSE;
    mmu
}

pub fn new_mmu_pae(cr3: u64) -> Mmu {
    let mut mmu = Mmu::default();
    mmu.cr0 = CR0_PG;
    mmu.cr3 = cr3;
    mmu.cr4 = CR4_PAE;
    mmu
}

pub fn new_mmu_long(cr3: u64, nxe: bool) -> Mmu {
    let mut mmu = Mmu::default();
    mmu.cr0 = CR0_PG;
    mmu.cr3 = cr3;
    mmu.cr4 = CR4_PAE;
    mmu.efer = EFER_LME | if nxe { EFER_NXE } else { 0 };
    mmu
}

fn is_canonical_4level(vaddr: u64) -> bool {
    let sign = (vaddr >> 47) & 1;
    let upper = vaddr >> 48;
    if sign == 0 {
        upper == 0
    } else {
        upper == 0xFFFF
    }
}

fn pf_protection(vaddr: u64, access: AccessType, cpl: u8) -> TranslateError {
    let mut code = crate::mmu::PFEC_P;
    if access.is_write() {
        code |= crate::mmu::PFEC_WR;
    }
    if cpl == 3 {
        code |= crate::mmu::PFEC_US;
    }
    if access.is_execute() {
        code |= crate::mmu::PFEC_ID;
    }
    TranslateError::PageFault { vaddr, code }
}

fn check_tlb_permissions(entry: &TlbEntry, mmu: &Mmu, vaddr: u64, access: AccessType) -> Option<TranslateError> {
    if mmu.cpl == 3 && !entry.user {
        return Some(pf_protection(vaddr, access, mmu.cpl));
    }

    let wp = (mmu.cr0 & CR0_WP) != 0;
    if access.is_write() && !entry.writable && (mmu.cpl == 3 || wp) {
        return Some(pf_protection(vaddr, access, mmu.cpl));
    }

    let nxe = (mmu.efer & EFER_NXE) != 0;
    if access.is_execute() && nxe && entry.nx {
        return Some(pf_protection(vaddr, access, mmu.cpl));
    }

    None
}

fn build_long_tlb_entry(
    bus: &mut impl MemoryBus,
    mmu: &Mmu,
    vaddr: u64,
    paddr: u64,
    page_size: crate::mmu::PageSize,
) -> Option<TlbEntry> {
    const ENTRY_RW: u64 = 1 << 1;
    const ENTRY_US: u64 = 1 << 2;
    const ENTRY_PS: u64 = 1 << 7;
    const ENTRY_G: u64 = 1 << 8;
    const ENTRY_NX: u64 = 1 << 63;
    const CR3_PML4_BASE_MASK: u64 = 0x000F_FFFF_FFFF_F000;
    const ADDR_MASK_4K: u64 = 0x000F_FFFF_FFFF_F000;

    let pml4_base = mmu.cr3 & CR3_PML4_BASE_MASK;

    let pml4_index = (vaddr >> 39) & 0x1FF;
    let pdpt_index = (vaddr >> 30) & 0x1FF;
    let pd_index = (vaddr >> 21) & 0x1FF;
    let pt_index = (vaddr >> 12) & 0x1FF;

    let pml4e = bus.read_u64(pml4_base + pml4_index * 8);
    let pdpt_base = pml4e & ADDR_MASK_4K;
    let pdpte = bus.read_u64(pdpt_base + pdpt_index * 8);

    let mut writable = (pml4e & ENTRY_RW) != 0 && (pdpte & ENTRY_RW) != 0;
    let mut user = (pml4e & ENTRY_US) != 0 && (pdpte & ENTRY_US) != 0;
    let mut nx = (pml4e & ENTRY_NX) != 0 || (pdpte & ENTRY_NX) != 0;

    let (leaf, tlb_page_size) = match page_size {
        crate::mmu::PageSize::Size1G => {
            debug_assert!((pdpte & ENTRY_PS) != 0);
            (pdpte, crate::tlb::PageSize::Size1G)
        }
        crate::mmu::PageSize::Size2M => {
            let pd_base = pdpte & ADDR_MASK_4K;
            let pde = bus.read_u64(pd_base + pd_index * 8);
            writable &= (pde & ENTRY_RW) != 0;
            user &= (pde & ENTRY_US) != 0;
            nx |= (pde & ENTRY_NX) != 0;
            debug_assert!((pde & ENTRY_PS) != 0);
            (pde, crate::tlb::PageSize::Size2M)
        }
        crate::mmu::PageSize::Size4K => {
            let pd_base = pdpte & ADDR_MASK_4K;
            let pde = bus.read_u64(pd_base + pd_index * 8);
            writable &= (pde & ENTRY_RW) != 0;
            user &= (pde & ENTRY_US) != 0;
            nx |= (pde & ENTRY_NX) != 0;

            let pt_base = pde & ADDR_MASK_4K;
            let pte = bus.read_u64(pt_base + pt_index * 8);
            writable &= (pte & ENTRY_RW) != 0;
            user &= (pte & ENTRY_US) != 0;
            nx |= (pte & ENTRY_NX) != 0;
            (pte, crate::tlb::PageSize::Size4K)
        }
    };

    Some(TlbEntry {
        vbase: tlb_page_size.align_down(vaddr),
        pbase: tlb_page_size.align_down(paddr),
        page_size: tlb_page_size,
        writable,
        user,
        nx,
        global: (leaf & ENTRY_G) != 0,
    })
}

pub struct TlbMmu {
    pub mmu: Mmu,
    pub tlb: Tlb,
}

impl TlbMmu {
    pub fn new(mmu: Mmu) -> Self {
        Self { mmu, tlb: Tlb::new() }
    }

    pub fn tlb_len(&self) -> usize {
        self.tlb.len()
    }

    pub fn invalidate_page(&mut self, vaddr: u64) {
        self.tlb.invalidate_page(vaddr);
    }

    pub fn set_cr3(&mut self, cr3: u64) {
        self.mmu.cr3 = cr3;
        self.tlb.flush_on_cr3_write((self.mmu.cr4 & CR4_PGE) != 0);
    }

    pub fn translate_without_tlb(
        &self,
        bus: &mut impl MemoryBus,
        linear: u64,
        access: AccessType,
    ) -> Result<u64, TranslateError> {
        self.mmu.translate(bus, linear, access)
    }

    pub fn translate_with_tlb(
        &mut self,
        bus: &mut impl MemoryBus,
        linear: u64,
        access: AccessType,
    ) -> Result<u64, TranslateError> {
        // Only cache translations in 4-level paging for now.
        let paging = (self.mmu.cr0 & CR0_PG) != 0;
        let long_mode = paging && (self.mmu.cr4 & CR4_PAE) != 0 && (self.mmu.efer & EFER_LME) != 0;
        if !long_mode {
            return self.translate_without_tlb(bus, linear, access);
        }

        let vaddr = linear;
        if !is_canonical_4level(vaddr) {
            return Err(TranslateError::GeneralProtection { vaddr });
        }

        if let Some(entry) = self.tlb.lookup(vaddr) {
            if let Some(err) = check_tlb_permissions(&entry, &self.mmu, vaddr, access) {
                return Err(err);
            }
            return Ok(entry.translate(vaddr));
        }

        let res = long::translate_4level(
            bus,
            vaddr,
            access,
            self.mmu.cr3,
            self.mmu.cr0,
            self.mmu.efer,
            self.mmu.cpl,
        )?;

        let paddr = res.paddr;
        if let Some(entry) = build_long_tlb_entry(bus, &self.mmu, vaddr, paddr, res.page_size) {
            self.tlb.insert(entry);
        }

        Ok(paddr)
    }
}
