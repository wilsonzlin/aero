//! x86/x86-64 MMU: virtual → physical translation with a software TLB.
//!
//! This crate implements the paging modes needed by Windows bootloaders/kernels:
//! - No paging (identity mapping)
//! - 32-bit paging (4KB / 4MB pages)
//! - PAE paging (4KB / 2MB pages)
//! - 4-level long mode paging (4KB / 2MB / 1GB pages) with canonical checks

mod tlb;

use tlb::{PageSize, Tlb, TlbEntry, TlbEntryAttributes};

/// Physical memory access used for page-table walking.
///
/// This is intentionally minimal; the CPU can wrap a richer memory bus and
/// forward physical reads/writes used for paging.
pub trait MemoryBus {
    fn read_u8(&mut self, paddr: u64) -> u8;
    fn read_u16(&mut self, paddr: u64) -> u16;
    fn read_u32(&mut self, paddr: u64) -> u32;
    fn read_u64(&mut self, paddr: u64) -> u64;

    fn write_u8(&mut self, paddr: u64, value: u8);
    fn write_u16(&mut self, paddr: u64, value: u16);
    fn write_u32(&mut self, paddr: u64, value: u32);
    fn write_u64(&mut self, paddr: u64, value: u64);
}

impl<T: MemoryBus + ?Sized> MemoryBus for &mut T {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        <T as MemoryBus>::read_u8(&mut **self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        <T as MemoryBus>::read_u16(&mut **self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        <T as MemoryBus>::read_u32(&mut **self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        <T as MemoryBus>::read_u64(&mut **self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        <T as MemoryBus>::write_u8(&mut **self, paddr, value)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        <T as MemoryBus>::write_u16(&mut **self, paddr, value)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        <T as MemoryBus>::write_u32(&mut **self, paddr, value)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        <T as MemoryBus>::write_u64(&mut **self, paddr, value)
    }
}

/// Enable use of [`memory::MemoryBus`] (the emulator's physical bus trait) as the MMU
/// page-walk backend.
#[cfg(feature = "memory-bus")]
impl MemoryBus for memory::Bus {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        memory::MemoryBus::read_u8(self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        memory::MemoryBus::read_u16(self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        memory::MemoryBus::read_u32(self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        memory::MemoryBus::read_u64(self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        memory::MemoryBus::write_u8(self, paddr, value)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        memory::MemoryBus::write_u16(self, paddr, value)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        memory::MemoryBus::write_u32(self, paddr, value)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        memory::MemoryBus::write_u64(self, paddr, value)
    }
}

/// Enable use of [`memory::PhysicalMemoryBus`] as the MMU page-walk backend.
#[cfg(feature = "memory-bus")]
impl MemoryBus for memory::PhysicalMemoryBus {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        memory::MemoryBus::read_u8(self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        memory::MemoryBus::read_u16(self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        memory::MemoryBus::read_u32(self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        memory::MemoryBus::read_u64(self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        memory::MemoryBus::write_u8(self, paddr, value)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        memory::MemoryBus::write_u16(self, paddr, value)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        memory::MemoryBus::write_u32(self, paddr, value)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        memory::MemoryBus::write_u64(self, paddr, value)
    }
}

/// Enable use of a trait object `dyn memory::MemoryBus` as the MMU page-walk backend.
#[cfg(feature = "memory-bus")]
impl MemoryBus for dyn memory::MemoryBus {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        memory::MemoryBus::read_u8(self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        memory::MemoryBus::read_u16(self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        memory::MemoryBus::read_u32(self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        memory::MemoryBus::read_u64(self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        memory::MemoryBus::write_u8(self, paddr, value)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        memory::MemoryBus::write_u16(self, paddr, value)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        memory::MemoryBus::write_u32(self, paddr, value)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        memory::MemoryBus::write_u64(self, paddr, value)
    }
}

/// Enable use of [`aero_mem::MemoryBus`] (the new shared physical address router) as the MMU
/// page-walk backend.
#[cfg(feature = "aero-mem-bus")]
impl MemoryBus for aero_mem::MemoryBus {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        (&*self).try_read_u8(paddr).unwrap_or(0xFF)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        (&*self).try_read_u16(paddr).unwrap_or(0xFFFF)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        (&*self).try_read_u32(paddr).unwrap_or(0xFFFF_FFFF)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        (&*self)
            .try_read_u64(paddr)
            .unwrap_or(0xFFFF_FFFF_FFFF_FFFF)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        let _ = (&*self).try_write_u8(paddr, value);
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        let _ = (&*self).try_write_u16(paddr, value);
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        let _ = (&*self).try_write_u32(paddr, value);
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        let _ = (&*self).try_write_u64(paddr, value);
    }
}

/// Type of memory access being translated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    Read,
    Write,
    Execute,
}

impl AccessType {
    #[inline]
    fn is_write(self) -> bool {
        matches!(self, AccessType::Write)
    }

    #[inline]
    fn is_execute(self) -> bool {
        matches!(self, AccessType::Execute)
    }
}

/// A translation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateFault {
    /// #PF with CR2 and the error code already computed.
    PageFault(PageFault),
    /// Non-canonical linear address in long mode (would raise #GP(0)).
    NonCanonical(u64),
}

/// #PF details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFault {
    /// Faulting linear address (CR2).
    pub addr: u64,
    /// Error code as per Intel SDM.
    pub error_code: u32,
}

impl PageFault {
    #[inline]
    fn new(addr: u64, error_code: u32) -> Self {
        Self { addr, error_code }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PagingMode {
    Disabled,
    Legacy32,
    Pae,
    Long4,
}

/// x86 MMU with a software TLB.
#[derive(Debug, Clone)]
pub struct Mmu {
    cr0: u64,
    cr2: u64,
    cr3: u64,
    cr4: u64,
    efer: u64,
    max_phys_bits: u8,
    tlb: Tlb,
}

impl Default for Mmu {
    fn default() -> Self {
        Self::new()
    }
}

impl Mmu {
    pub fn new() -> Self {
        Self {
            cr0: 0,
            cr2: 0,
            cr3: 0,
            cr4: 0,
            efer: 0,
            max_phys_bits: 52,
            tlb: Tlb::new(),
        }
    }

    /// CR2 is architecturally written by the CPU on #PF injection; the MMU
    /// stores it for convenience so the CPU can fetch it after translation.
    #[inline]
    pub fn cr2(&self) -> u64 {
        self.cr2
    }

    #[inline]
    pub fn cr0(&self) -> u64 {
        self.cr0
    }

    #[inline]
    pub fn cr3(&self) -> u64 {
        self.cr3
    }

    #[inline]
    pub fn cr4(&self) -> u64 {
        self.cr4
    }

    #[inline]
    pub fn efer(&self) -> u64 {
        self.efer
    }

    pub fn set_max_phys_bits(&mut self, bits: u8) {
        assert!((1..=52).contains(&bits), "max_phys_bits must be 1..=52");
        if self.max_phys_bits != bits {
            self.max_phys_bits = bits;
            self.tlb.flush_all();
        }
    }

    pub fn set_cr0(&mut self, value: u64) {
        let old_pg = self.cr0 & CR0_PG != 0;
        self.cr0 = value;
        let new_pg = self.cr0 & CR0_PG != 0;
        if old_pg != new_pg {
            self.tlb.flush_all();
        }
    }

    pub fn set_cr3(&mut self, value: u64) {
        self.cr3 = value;
        self.tlb.on_cr3_write(
            self.cr4_pge(),
            self.pcid_enabled(),
            self.current_pcid(),
            self.cr3_no_flush(),
        );
    }

    pub fn set_cr4(&mut self, value: u64) {
        let old_relevant = self.cr4 & (CR4_PAE | CR4_PSE | CR4_PGE | CR4_PCIDE);
        self.cr4 = value;
        let new_relevant = self.cr4 & (CR4_PAE | CR4_PSE | CR4_PGE | CR4_PCIDE);
        if old_relevant != new_relevant {
            // These bits affect translation semantics and/or TLB global behaviour.
            self.tlb.flush_all();
        }
    }

    pub fn set_efer(&mut self, value: u64) {
        let old_relevant = self.efer & (EFER_LME | EFER_NXE);
        self.efer = value;
        let new_relevant = self.efer & (EFER_LME | EFER_NXE);
        if old_relevant != new_relevant {
            self.tlb.flush_all();
        }
    }

    /// INVLPG.
    pub fn invlpg(&mut self, vaddr: u64) {
        if self.pcid_enabled() {
            // In PCID mode, INVLPG invalidates the current PCID's translation and
            // any global translation for the address. Other PCIDs are unaffected.
            self.tlb
                .invalidate_address_pcid(vaddr, self.current_pcid(), true);
        } else {
            self.tlb.invalidate_address_all(vaddr);
        }
    }

    /// Optional extension point for INVPCID (not all invalidation types are
    /// required by the project yet).
    pub fn invpcid(&mut self, pcid: u16, kind: InvpcidType) {
        self.tlb.invpcid(pcid, kind);
    }

    /// Translate a linear address to a physical address.
    ///
    /// `cpl` is the current privilege level (0..=3). Only CPL==3 is treated as
    /// "user"; all others are "supervisor".
    pub fn translate(
        &mut self,
        bus: &mut impl MemoryBus,
        vaddr: u64,
        access: AccessType,
        cpl: u8,
    ) -> Result<u64, TranslateFault> {
        let is_user = cpl == 3;
        let mode = self.paging_mode();

        if mode == PagingMode::Disabled {
            // With paging disabled, x86 uses a 32-bit linear address space (long
            // mode cannot be active without paging).
            return Ok(vaddr & 0xffff_ffff);
        }

        // In non-long paging modes, the linear address is 32-bit.
        let vaddr = match mode {
            PagingMode::Legacy32 | PagingMode::Pae => vaddr as u32 as u64,
            PagingMode::Long4 => vaddr,
            PagingMode::Disabled => unreachable!(),
        };

        if mode == PagingMode::Long4 && !is_canonical_48(vaddr) {
            return Err(TranslateFault::NonCanonical(vaddr));
        }

        if let Some(entry) = self
            .tlb
            .lookup(vaddr, access.is_execute(), self.current_pcid())
        {
            match self.check_perms_from_tlb(vaddr, entry, access, is_user) {
                Ok(()) => {
                    let paddr = entry.translate(vaddr);
                    let needs_dirty = access.is_write() && !entry.dirty;
                    let leaf_addr = entry.leaf_addr;
                    let leaf_is_64 = entry.leaf_is_64;

                    if needs_dirty {
                        if leaf_is_64 {
                            let val = bus.read_u64(leaf_addr);
                            bus.write_u64(leaf_addr, val | PTE_D64);
                        } else {
                            let val = bus.read_u32(leaf_addr);
                            bus.write_u32(leaf_addr, val | (PTE_D as u32));
                        }
                        self.tlb
                            .set_dirty(vaddr, access.is_execute(), self.current_pcid());
                    }

                    return Ok(paddr);
                }
                Err(pf) => {
                    self.cr2 = pf.addr;
                    return Err(TranslateFault::PageFault(pf));
                }
            }
        }

        let walk_res = match mode {
            PagingMode::Disabled => unreachable!(),
            PagingMode::Legacy32 => self.walk_legacy32(bus, vaddr, access, is_user),
            PagingMode::Pae => self.walk_pae(bus, vaddr, access, is_user),
            PagingMode::Long4 => self.walk_long4(bus, vaddr, access, is_user),
        };

        match walk_res {
            Ok((entry, paddr)) => {
                self.tlb.insert(access.is_execute(), entry);
                Ok(paddr)
            }
            Err(pf) => {
                self.cr2 = pf.addr;
                Err(TranslateFault::PageFault(pf))
            }
        }
    }

    fn paging_mode(&self) -> PagingMode {
        if self.cr0 & CR0_PG == 0 {
            return PagingMode::Disabled;
        }
        if self.cr4 & CR4_PAE == 0 {
            return PagingMode::Legacy32;
        }
        if self.efer & EFER_LME != 0 {
            return PagingMode::Long4;
        }
        PagingMode::Pae
    }

    #[inline]
    fn cr4_pse(&self) -> bool {
        self.cr4 & CR4_PSE != 0
    }

    #[inline]
    fn cr4_pge(&self) -> bool {
        self.cr4 & CR4_PGE != 0
    }

    #[inline]
    fn pcid_enabled(&self) -> bool {
        self.cr4 & CR4_PCIDE != 0
    }

    #[inline]
    fn current_pcid(&self) -> u16 {
        if self.pcid_enabled() {
            (self.cr3 & 0xfff) as u16
        } else {
            0
        }
    }

    #[inline]
    fn cr3_no_flush(&self) -> bool {
        self.pcid_enabled() && (self.cr3 >> 63) & 1 != 0
    }

    #[inline]
    fn nx_enabled(&self) -> bool {
        self.efer & EFER_NXE != 0
    }

    #[inline]
    fn wp_enabled(&self) -> bool {
        self.cr0 & CR0_WP != 0
    }

    #[inline]
    fn phys_addr_mask(&self) -> u64 {
        if self.max_phys_bits == 64 {
            !0
        } else {
            (1u64 << self.max_phys_bits) - 1
        }
    }

    fn check_perms_from_tlb(
        &self,
        vaddr: u64,
        entry: &TlbEntry,
        access: AccessType,
        is_user: bool,
    ) -> Result<(), PageFault> {
        self.check_perms(vaddr, entry.user, entry.writable, entry.nx, access, is_user)
    }

    fn check_perms(
        &self,
        vaddr: u64,
        user_ok: bool,
        writable_ok: bool,
        nx: bool,
        access: AccessType,
        is_user: bool,
    ) -> Result<(), PageFault> {
        if is_user && !user_ok {
            return Err(PageFault::new(
                vaddr,
                pf_error_code(true, access, is_user, false),
            ));
        }

        if access.is_write() && !writable_ok && (is_user || self.wp_enabled()) {
            return Err(PageFault::new(
                vaddr,
                pf_error_code(true, access, is_user, false),
            ));
        }

        if access.is_execute() && self.nx_enabled() && nx {
            return Err(PageFault::new(
                vaddr,
                pf_error_code(true, access, is_user, false),
            ));
        }

        Ok(())
    }

    fn walk_legacy32(
        &mut self,
        bus: &mut impl MemoryBus,
        vaddr: u64,
        access: AccessType,
        is_user: bool,
    ) -> Result<(TlbEntry, u64), PageFault> {
        let pd_base = (self.cr3 & 0xffff_ffff) & !0xfff;
        let pd_index = (vaddr >> 22) & 0x3ff;
        let pde_addr = pd_base + pd_index * 4;
        let pde_raw = bus.read_u32(pde_addr) as u64;
        if pde_raw & PTE_P == 0 {
            return Err(self.page_fault_not_present(vaddr, access, is_user));
        }

        let pde_ps = pde_raw & PTE_PS != 0;
        if pde_ps {
            // 4MB pages require CR4.PSE; otherwise PS is treated as reserved.
            if !self.cr4_pse() {
                return Err(self.page_fault_rsvd(vaddr, access, is_user));
            }
            if (pde_raw & LEGACY32_4MB_RESERVED_MASK) != 0 {
                return Err(self.page_fault_rsvd(vaddr, access, is_user));
            }
        }

        let pde = self
            .check_entry32(bus, pde_addr, pde_raw)
            .expect("present already checked");

        if pde_ps {
            let user_ok = pde & PTE_US != 0;
            let writable_ok = pde & PTE_RW != 0;
            let nx = false;

            self.check_perms(vaddr, user_ok, writable_ok, nx, access, is_user)?;

            // Dirty only on successful write.
            let mut new_pde = pde;
            if access.is_write() {
                new_pde |= PTE_D;
            }
            if new_pde != pde {
                bus.write_u32(pde_addr, new_pde as u32);
            }

            let page_size = PageSize::Size4M;
            let vbase = vaddr & !(page_size.bytes() - 1);
            let pbase = pde & 0xffc0_0000;
            let global = self.cr4_pge() && (pde & PTE_G != 0);
            let dirty = new_pde & PTE_D != 0;
            let entry = TlbEntry::new(
                vbase,
                pbase,
                page_size,
                self.current_pcid(),
                TlbEntryAttributes {
                    user: user_ok,
                    writable: writable_ok,
                    nx,
                    global,
                    leaf_addr: pde_addr,
                    leaf_is_64: false,
                    dirty,
                },
            );
            let paddr = pbase + (vaddr - vbase);
            return Ok((entry, paddr));
        }

        // 4KB pages via PT.
        let pt_base = pde & 0xffff_f000;
        let pt_index = (vaddr >> 12) & 0x3ff;
        let pte_addr = pt_base + pt_index * 4;
        let pte_raw = bus.read_u32(pte_addr) as u64;
        let pte = match self.check_entry32(bus, pte_addr, pte_raw) {
            Some(v) => v,
            None => return Err(self.page_fault_not_present(vaddr, access, is_user)),
        };

        let user_ok = (pde & PTE_US != 0) && (pte & PTE_US != 0);
        let writable_ok = (pde & PTE_RW != 0) && (pte & PTE_RW != 0);
        let nx = false;

        self.check_perms(vaddr, user_ok, writable_ok, nx, access, is_user)?;

        let mut new_pte = pte;
        if access.is_write() {
            new_pte |= PTE_D;
        }
        if new_pte != pte {
            bus.write_u32(pte_addr, new_pte as u32);
        }

        let page_size = PageSize::Size4K;
        let vbase = vaddr & !(page_size.bytes() - 1);
        let pbase = pte & 0xffff_f000;
        let global = self.cr4_pge() && (pte & PTE_G != 0);
        let dirty = new_pte & PTE_D != 0;
        let entry = TlbEntry::new(
            vbase,
            pbase,
            page_size,
            self.current_pcid(),
            TlbEntryAttributes {
                user: user_ok,
                writable: writable_ok,
                nx,
                global,
                leaf_addr: pte_addr,
                leaf_is_64: false,
                dirty,
            },
        );
        let paddr = pbase + (vaddr - vbase);
        Ok((entry, paddr))
    }

    fn walk_pae(
        &mut self,
        bus: &mut impl MemoryBus,
        vaddr: u64,
        access: AccessType,
        is_user: bool,
    ) -> Result<(TlbEntry, u64), PageFault> {
        let nx_enabled = self.nx_enabled();
        let addr_mask = self.phys_addr_mask();
        let ctx = EntryAccessContext {
            vaddr,
            access,
            is_user,
        };

        let pdpt_base = (self.cr3 & 0xffff_ffff) & !0x1f;
        let pdpt_index = (vaddr >> 30) & 0x3;
        let pdpte_addr = pdpt_base + pdpt_index * 8;
        let pdpte = bus.read_u64(pdpte_addr);

        let pdpte = match self.check_entry64(bus, pdpte_addr, pdpte, ctx, EntryKind64::PdptePae)? {
            Some(v) => v,
            None => return Err(self.page_fault_not_present(vaddr, access, is_user)),
        };

        // In IA-32 PAE paging, the PDPT entry does not participate in U/S or
        // R/W protection checks (bits 1 and 2 are reserved). It can, however,
        // contribute NX when EFER.NXE is enabled.
        let mut eff_user = true;
        let mut eff_writable = true;
        let mut eff_nx = nx_enabled && (pdpte & PTE_NX != 0);

        let pd_base = (pdpte & addr_mask) & !0xfff;
        let pd_index = (vaddr >> 21) & 0x1ff;
        let pde_addr = pd_base + pd_index * 8;
        let pde = bus.read_u64(pde_addr);

        let pde = match self.check_entry64(bus, pde_addr, pde, ctx, EntryKind64::PdePae)? {
            Some(v) => v,
            None => return Err(self.page_fault_not_present(vaddr, access, is_user)),
        };

        eff_user &= pde & PTE_US64 != 0;
        eff_writable &= pde & PTE_RW64 != 0;
        eff_nx |= nx_enabled && (pde & PTE_NX != 0);

        let pde_ps = pde & PTE_PS64 != 0;
        if pde_ps {
            self.check_perms(vaddr, eff_user, eff_writable, eff_nx, access, is_user)?;

            let mut new_pde = pde;
            if access.is_write() {
                new_pde |= PTE_D64;
            }
            if new_pde != pde {
                bus.write_u64(pde_addr, new_pde);
            }

            let page_size = PageSize::Size2M;
            let vbase = vaddr & !(page_size.bytes() - 1);
            let pbase = (pde & addr_mask) & !(page_size.bytes() - 1);
            let global = self.cr4_pge() && (pde & PTE_G64 != 0);
            let dirty = new_pde & PTE_D64 != 0;
            let entry = TlbEntry::new(
                vbase,
                pbase,
                page_size,
                self.current_pcid(),
                TlbEntryAttributes {
                    user: eff_user,
                    writable: eff_writable,
                    nx: eff_nx,
                    global,
                    leaf_addr: pde_addr,
                    leaf_is_64: true,
                    dirty,
                },
            );
            let paddr = pbase + (vaddr - vbase);
            return Ok((entry, paddr));
        }

        let pt_base = (pde & addr_mask) & !0xfff;
        let pt_index = (vaddr >> 12) & 0x1ff;
        let pte_addr = pt_base + pt_index * 8;
        let pte = bus.read_u64(pte_addr);

        let pte = match self.check_entry64(bus, pte_addr, pte, ctx, EntryKind64::PtePae)? {
            Some(v) => v,
            None => return Err(self.page_fault_not_present(vaddr, access, is_user)),
        };

        eff_user &= pte & PTE_US64 != 0;
        eff_writable &= pte & PTE_RW64 != 0;
        eff_nx |= nx_enabled && (pte & PTE_NX != 0);

        self.check_perms(vaddr, eff_user, eff_writable, eff_nx, access, is_user)?;

        let mut new_pte = pte;
        if access.is_write() {
            new_pte |= PTE_D64;
        }
        if new_pte != pte {
            bus.write_u64(pte_addr, new_pte);
        }

        let page_size = PageSize::Size4K;
        let vbase = vaddr & !(page_size.bytes() - 1);
        let pbase = (pte & addr_mask) & !0xfff;
        let global = self.cr4_pge() && (pte & PTE_G64 != 0);
        let dirty = new_pte & PTE_D64 != 0;
        let entry = TlbEntry::new(
            vbase,
            pbase,
            page_size,
            self.current_pcid(),
            TlbEntryAttributes {
                user: eff_user,
                writable: eff_writable,
                nx: eff_nx,
                global,
                leaf_addr: pte_addr,
                leaf_is_64: true,
                dirty,
            },
        );
        let paddr = pbase + (vaddr - vbase);
        Ok((entry, paddr))
    }

    fn walk_long4(
        &mut self,
        bus: &mut impl MemoryBus,
        vaddr: u64,
        access: AccessType,
        is_user: bool,
    ) -> Result<(TlbEntry, u64), PageFault> {
        let nx_enabled = self.nx_enabled();
        let addr_mask = self.phys_addr_mask();
        let ctx = EntryAccessContext {
            vaddr,
            access,
            is_user,
        };

        let pml4_base = (self.cr3 & addr_mask) & !0xfff;
        let pml4_index = (vaddr >> 39) & 0x1ff;
        let pml4e_addr = pml4_base + pml4_index * 8;
        let pml4e = bus.read_u64(pml4e_addr);

        let pml4e = match self.check_entry64(bus, pml4e_addr, pml4e, ctx, EntryKind64::Pml4e)? {
            Some(v) => v,
            None => return Err(self.page_fault_not_present(vaddr, access, is_user)),
        };

        let mut eff_user = pml4e & PTE_US64 != 0;
        let mut eff_writable = pml4e & PTE_RW64 != 0;
        let mut eff_nx = nx_enabled && (pml4e & PTE_NX != 0);

        let pdpt_base = (pml4e & addr_mask) & !0xfff;
        let pdpt_index = (vaddr >> 30) & 0x1ff;
        let pdpte_addr = pdpt_base + pdpt_index * 8;
        let pdpte = bus.read_u64(pdpte_addr);

        let pdpte = match self.check_entry64(bus, pdpte_addr, pdpte, ctx, EntryKind64::PdpteLong)? {
            Some(v) => v,
            None => return Err(self.page_fault_not_present(vaddr, access, is_user)),
        };

        eff_user &= pdpte & PTE_US64 != 0;
        eff_writable &= pdpte & PTE_RW64 != 0;
        eff_nx |= nx_enabled && (pdpte & PTE_NX != 0);

        let pdpte_ps = pdpte & PTE_PS64 != 0;
        if pdpte_ps {
            self.check_perms(vaddr, eff_user, eff_writable, eff_nx, access, is_user)?;

            let mut new_pdpte = pdpte;
            if access.is_write() {
                new_pdpte |= PTE_D64;
            }
            if new_pdpte != pdpte {
                bus.write_u64(pdpte_addr, new_pdpte);
            }

            let page_size = PageSize::Size1G;
            let vbase = vaddr & !(page_size.bytes() - 1);
            let pbase = (pdpte & addr_mask) & !(page_size.bytes() - 1);
            let global = self.cr4_pge() && (pdpte & PTE_G64 != 0);
            let dirty = new_pdpte & PTE_D64 != 0;
            let entry = TlbEntry::new(
                vbase,
                pbase,
                page_size,
                self.current_pcid(),
                TlbEntryAttributes {
                    user: eff_user,
                    writable: eff_writable,
                    nx: eff_nx,
                    global,
                    leaf_addr: pdpte_addr,
                    leaf_is_64: true,
                    dirty,
                },
            );
            let paddr = pbase + (vaddr - vbase);
            return Ok((entry, paddr));
        }

        let pd_base = (pdpte & addr_mask) & !0xfff;
        let pd_index = (vaddr >> 21) & 0x1ff;
        let pde_addr = pd_base + pd_index * 8;
        let pde = bus.read_u64(pde_addr);

        let pde = match self.check_entry64(bus, pde_addr, pde, ctx, EntryKind64::PdeLong)? {
            Some(v) => v,
            None => return Err(self.page_fault_not_present(vaddr, access, is_user)),
        };

        eff_user &= pde & PTE_US64 != 0;
        eff_writable &= pde & PTE_RW64 != 0;
        eff_nx |= nx_enabled && (pde & PTE_NX != 0);

        let pde_ps = pde & PTE_PS64 != 0;
        if pde_ps {
            self.check_perms(vaddr, eff_user, eff_writable, eff_nx, access, is_user)?;

            let mut new_pde = pde;
            if access.is_write() {
                new_pde |= PTE_D64;
            }
            if new_pde != pde {
                bus.write_u64(pde_addr, new_pde);
            }

            let page_size = PageSize::Size2M;
            let vbase = vaddr & !(page_size.bytes() - 1);
            let pbase = (pde & addr_mask) & !(page_size.bytes() - 1);
            let global = self.cr4_pge() && (pde & PTE_G64 != 0);
            let dirty = new_pde & PTE_D64 != 0;
            let entry = TlbEntry::new(
                vbase,
                pbase,
                page_size,
                self.current_pcid(),
                TlbEntryAttributes {
                    user: eff_user,
                    writable: eff_writable,
                    nx: eff_nx,
                    global,
                    leaf_addr: pde_addr,
                    leaf_is_64: true,
                    dirty,
                },
            );
            let paddr = pbase + (vaddr - vbase);
            return Ok((entry, paddr));
        }

        let pt_base = (pde & addr_mask) & !0xfff;
        let pt_index = (vaddr >> 12) & 0x1ff;
        let pte_addr = pt_base + pt_index * 8;
        let pte = bus.read_u64(pte_addr);

        let pte = match self.check_entry64(bus, pte_addr, pte, ctx, EntryKind64::PteLong)? {
            Some(v) => v,
            None => return Err(self.page_fault_not_present(vaddr, access, is_user)),
        };

        eff_user &= pte & PTE_US64 != 0;
        eff_writable &= pte & PTE_RW64 != 0;
        eff_nx |= nx_enabled && (pte & PTE_NX != 0);

        self.check_perms(vaddr, eff_user, eff_writable, eff_nx, access, is_user)?;

        let mut new_pte = pte;
        if access.is_write() {
            new_pte |= PTE_D64;
        }
        if new_pte != pte {
            bus.write_u64(pte_addr, new_pte);
        }

        let page_size = PageSize::Size4K;
        let vbase = vaddr & !(page_size.bytes() - 1);
        let pbase = (pte & addr_mask) & !0xfff;
        let global = self.cr4_pge() && (pte & PTE_G64 != 0);
        let dirty = new_pte & PTE_D64 != 0;
        let entry = TlbEntry::new(
            vbase,
            pbase,
            page_size,
            self.current_pcid(),
            TlbEntryAttributes {
                user: eff_user,
                writable: eff_writable,
                nx: eff_nx,
                global,
                leaf_addr: pte_addr,
                leaf_is_64: true,
                dirty,
            },
        );
        let paddr = pbase + (vaddr - vbase);
        Ok((entry, paddr))
    }

    fn page_fault_not_present(&self, vaddr: u64, access: AccessType, is_user: bool) -> PageFault {
        PageFault::new(vaddr, pf_error_code(false, access, is_user, false))
    }

    fn page_fault_rsvd(&self, vaddr: u64, access: AccessType, is_user: bool) -> PageFault {
        PageFault::new(vaddr, pf_error_code(true, access, is_user, true))
    }

    fn check_entry32(&self, bus: &mut impl MemoryBus, entry_addr: u64, entry: u64) -> Option<u64> {
        if entry & PTE_P == 0 {
            return None;
        }

        let mut entry = entry;
        if entry & PTE_A == 0 {
            entry |= PTE_A;
            bus.write_u32(entry_addr, entry as u32);
        }

        Some(entry)
    }

    #[allow(clippy::too_many_arguments)]
    fn check_entry64(
        &self,
        bus: &mut impl MemoryBus,
        entry_addr: u64,
        entry: u64,
        ctx: EntryAccessContext,
        kind: EntryKind64,
    ) -> Result<Option<u64>, PageFault> {
        if entry & PTE_P64 == 0 {
            return Ok(None);
        }

        if self.has_reserved_bits64(entry, kind) {
            return Err(self.page_fault_rsvd(ctx.vaddr, ctx.access, ctx.is_user));
        }

        // IA-32 PAE PDPT entries do not have Accessed/Dirty bits; all other
        // paging-structure entries we emulate do.
        let mut entry = entry;
        if kind != EntryKind64::PdptePae && (entry & PTE_A64 == 0) {
            entry |= PTE_A64;
            bus.write_u64(entry_addr, entry);
        }

        Ok(Some(entry))
    }

    fn has_reserved_bits64(&self, entry: u64, kind: EntryKind64) -> bool {
        if entry & PTE_P64 == 0 {
            return false;
        }

        // NX bit reserved if NXE=0.
        let nx_enabled = self.nx_enabled();
        if !nx_enabled && (entry & PTE_NX != 0) {
            return true;
        }

        // PS is reserved at certain levels.
        match kind {
            EntryKind64::Pml4e | EntryKind64::PdptePae => {
                if entry & PTE_PS64 != 0 {
                    return true;
                }
            }
            _ => {}
        }

        // Large-page support is controlled by CR4.PSE in all paging modes we
        // emulate. If it's clear, treat PS as a reserved bit.
        if !self.cr4_pse() {
            match kind {
                EntryKind64::PdpteLong | EntryKind64::PdePae | EntryKind64::PdeLong => {
                    if entry & PTE_PS64 != 0 {
                        return true;
                    }
                }
                _ => {}
            }
        }

        let addr_mask = self.phys_addr_mask();

        if kind == EntryKind64::PdptePae {
            // IA-32 PAE PDPT entry format:
            //   - bit 0: Present
            //   - bit 3: PWT
            //   - bit 4: PCD
            //   - bits 9..=11: available to software (AVL)
            //   - bits 12..MAXPHYADDR-1: physical address of the PD base
            //   - bit 63: NX (only if EFER.NXE=1)
            //
            // Bits 1,2,5..=8 are reserved and must be 0.
            let allowed_flags = PTE_P64 | (1 << 3) | (1 << 4) | (0x7 << 9);
            let allowed_addr = addr_mask & !0xfff;
            let mut allowed = allowed_flags | allowed_addr;
            if nx_enabled {
                allowed |= PTE_NX;
            }
            return entry & !allowed != 0;
        }

        let page_align = match kind {
            EntryKind64::Pml4e => 0x1000u64,
            EntryKind64::PdptePae => 0x1000u64,
            EntryKind64::PdpteLong => {
                if entry & PTE_PS64 != 0 {
                    PageSize::Size1G.bytes()
                } else {
                    0x1000
                }
            }
            EntryKind64::PdePae | EntryKind64::PdeLong => {
                if entry & PTE_PS64 != 0 {
                    PageSize::Size2M.bytes()
                } else {
                    0x1000
                }
            }
            EntryKind64::PtePae | EntryKind64::PteLong => 0x1000,
        };

        let allowed_addr = addr_mask & !(page_align - 1);
        let mut allowed = allowed_addr | 0x1fff;
        if nx_enabled {
            allowed |= PTE_NX;
        }

        entry & !allowed != 0
    }
}

/// INVPCID invalidation types (subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvpcidType {
    /// Invalidate a single linear address for the given PCID.
    IndividualAddress(u64),
    /// Invalidate all mappings associated with the given PCID.
    SingleContext,
    /// Invalidate all mappings, including global.
    AllIncludingGlobal,
    /// Invalidate all mappings except global.
    AllExcludingGlobal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind64 {
    Pml4e,
    PdpteLong,
    PdeLong,
    PteLong,
    PdptePae,
    PdePae,
    PtePae,
}

#[derive(Debug, Clone, Copy)]
struct EntryAccessContext {
    vaddr: u64,
    access: AccessType,
    is_user: bool,
}

#[inline]
fn pf_error_code(present: bool, access: AccessType, is_user: bool, rsvd: bool) -> u32 {
    let mut code = 0u32;
    if present {
        code |= 1 << 0;
    }
    if access.is_write() {
        code |= 1 << 1;
    }
    if is_user {
        code |= 1 << 2;
    }
    if rsvd {
        code |= 1 << 3;
    }
    if access.is_execute() {
        code |= 1 << 4;
    }
    code
}

#[inline]
fn is_canonical_48(vaddr: u64) -> bool {
    let sign = (vaddr >> 47) & 1;
    let top = vaddr >> 48;
    if sign == 0 {
        top == 0
    } else {
        top == 0xffff
    }
}

const CR0_WP: u64 = 1 << 16;
const CR0_PG: u64 = 1 << 31;

const CR4_PSE: u64 = 1 << 4;
const CR4_PAE: u64 = 1 << 5;
const CR4_PGE: u64 = 1 << 7;
const CR4_PCIDE: u64 = 1 << 17;

const EFER_LME: u64 = 1 << 8;
const EFER_NXE: u64 = 1 << 11;

const PTE_P: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_US: u64 = 1 << 2;
const PTE_A: u64 = 1 << 5;
const PTE_D: u64 = 1 << 6;
const PTE_PS: u64 = 1 << 7;
const PTE_G: u64 = 1 << 8;

const PTE_P64: u64 = 1 << 0;
const PTE_RW64: u64 = 1 << 1;
const PTE_US64: u64 = 1 << 2;
const PTE_A64: u64 = 1 << 5;
const PTE_D64: u64 = 1 << 6;
const PTE_PS64: u64 = 1 << 7;
const PTE_G64: u64 = 1 << 8;
const PTE_NX: u64 = 1 << 63;

const LEGACY32_4MB_RESERVED_MASK: u64 = 0x003f_e000;

#[cfg(test)]
mod tests;
