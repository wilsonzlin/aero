use super::*;

use core::convert::TryInto;

#[derive(Clone)]
struct TestMemory {
    data: Vec<u8>,
    reads: usize,
    writes: usize,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            reads: 0,
            writes: 0,
        }
    }

    fn reset_counters(&mut self) {
        self.reads = 0;
        self.writes = 0;
    }

    fn reads(&self) -> usize {
        self.reads
    }

    fn writes(&self) -> usize {
        self.writes
    }

    fn write_u32_raw(&mut self, paddr: u64, value: u32) {
        let off = paddr as usize;
        self.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64_raw(&mut self, paddr: u64, value: u64) {
        let off = paddr as usize;
        self.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn read_u32_raw(&self, paddr: u64) -> u32 {
        let off = paddr as usize;
        u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
    }

    fn read_u64_raw(&self, paddr: u64) -> u64 {
        let off = paddr as usize;
        u64::from_le_bytes(self.data[off..off + 8].try_into().unwrap())
    }
}

impl MemoryBus for TestMemory {
    fn read_u8(&mut self, paddr: u64) -> u8 {
        self.reads += 1;
        self.data[paddr as usize]
    }

    fn read_u16(&mut self, paddr: u64) -> u16 {
        self.reads += 1;
        let off = paddr as usize;
        u16::from_le_bytes(self.data[off..off + 2].try_into().unwrap())
    }

    fn read_u32(&mut self, paddr: u64) -> u32 {
        self.reads += 1;
        let off = paddr as usize;
        u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
    }

    fn read_u64(&mut self, paddr: u64) -> u64 {
        self.reads += 1;
        let off = paddr as usize;
        u64::from_le_bytes(self.data[off..off + 8].try_into().unwrap())
    }

    fn write_u8(&mut self, paddr: u64, value: u8) {
        self.writes += 1;
        self.data[paddr as usize] = value;
    }

    fn write_u16(&mut self, paddr: u64, value: u16) {
        self.writes += 1;
        let off = paddr as usize;
        self.data[off..off + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(&mut self, paddr: u64, value: u32) {
        self.writes += 1;
        let off = paddr as usize;
        self.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(&mut self, paddr: u64, value: u64) {
        self.writes += 1;
        let off = paddr as usize;
        self.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }
}

fn read_u8_through_generic<B: MemoryBus>(mut bus: B, paddr: u64) -> u8 {
    bus.read_u8(paddr)
}

#[test]
fn memory_bus_is_implemented_for_mut_refs() {
    // Compile-time assertion: `&mut TestMemory` should satisfy `MemoryBus` bounds. This is relied
    // on by adapters like `PagingBus` that want to store a `&mut` physical bus directly.
    let mut mem = TestMemory::new(0x10);
    mem.data[0] = 0xaa;
    mem.reset_counters();

    // `B` is inferred as `&mut TestMemory`, so this only compiles if `&mut TestMemory: MemoryBus`.
    let got = read_u8_through_generic(&mut mem, 0);
    assert_eq!(got, 0xaa);
    assert_eq!(mem.reads(), 1);
}

#[test]
fn no_paging_is_identity() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x10000);

    mmu.set_cr0(0);

    assert_eq!(
        mmu.translate(&mut mem, 0x1234, AccessType::Read, 0),
        Ok(0x1234)
    );
    assert_eq!(
        mmu.translate(&mut mem, 0xdead_beef, AccessType::Write, 3),
        Ok(0xdead_beef)
    );
    // Linear addresses are 32-bit when paging is disabled.
    assert_eq!(
        mmu.translate(&mut mem, 0x1_0000_0000u64 + 0x5678, AccessType::Read, 0),
        Ok(0x5678)
    );
}

#[test]
fn legacy32_4kb_translation_sets_accessed_and_dirty() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x10000);

    let pd_base = 0x1000u64;
    let pt_base = 0x2000u64;
    let page_base = 0x3000u64;

    // PDE[0] -> PT
    mem.write_u32_raw(pd_base, (pt_base as u32) | (PTE_P | PTE_RW | PTE_US) as u32);
    // PTE[0] -> page
    mem.write_u32_raw(
        pt_base,
        (page_base as u32) | (PTE_P | PTE_RW | PTE_US) as u32,
    );

    mmu.set_cr3(pd_base);
    mmu.set_cr4(0);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x123u64;
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );

    let pde = mem.read_u32_raw(pd_base);
    let pte = mem.read_u32_raw(pt_base);
    assert_ne!(pde & (PTE_A as u32), 0);
    assert_ne!(pte & (PTE_A as u32), 0);
    assert_eq!(pte & (PTE_D as u32), 0);

    // Write should set the dirty bit even on a TLB hit.
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Write, 3),
        Ok(page_base + vaddr)
    );
    let pte2 = mem.read_u32_raw(pt_base);
    assert_ne!(pte2 & (PTE_D as u32), 0);
}

#[test]
fn legacy32_4mb_translation_sets_dirty_on_tlb_hit() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x20000);

    let pd_base = 0x1000u64;

    // Map linear 0x0040_0000..0x0080_0000 (PDE index 1) to physical 0x0000_0000..0x0040_0000.
    let pde_index = 1u64;
    let pde_addr = pd_base + pde_index * 4;
    let pde_val = (PTE_P | PTE_RW | PTE_US | PTE_PS) as u32; // base=0 for simplicity
    mem.write_u32_raw(pde_addr, pde_val);

    mmu.set_cr3(pd_base);
    mmu.set_cr4(CR4_PSE);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x0040_1234u64;
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(vaddr & 0x3f_ffff)
    );

    // Dirty must be set on a subsequent write even if the translation hits in the TLB.
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Write, 3),
        Ok(vaddr & 0x3f_ffff)
    );
    let pde_after = mem.read_u32_raw(pde_addr);
    assert_ne!(pde_after & (PTE_D as u32), 0);
}

#[test]
fn pae_4kb_translation_sets_accessed_and_dirty() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x20000);

    let pdpt_base = 0x1000u64;
    let pd_base = 0x2000u64;
    let pt_base = 0x3000u64;
    let page_base = 0x4000u64;

    // PDPTE[0] -> PD
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64);
    // PDE[0] -> PT
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    // PTE[0] -> page
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64);

    mmu.set_cr3(pdpt_base);
    mmu.set_cr4(CR4_PAE);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x456u64;
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );

    // PDPT entries do not have an accessed bit in IA-32 PAE paging.
    assert_eq!(mem.read_u64_raw(pdpt_base) & PTE_A64, 0);
    assert_ne!(mem.read_u64_raw(pd_base) & PTE_A64, 0);
    assert_ne!(mem.read_u64_raw(pt_base) & PTE_A64, 0);
    assert_eq!(mem.read_u64_raw(pt_base) & PTE_D64, 0);

    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Write, 3),
        Ok(page_base + vaddr)
    );
    assert_ne!(mem.read_u64_raw(pt_base) & PTE_D64, 0);
}

#[test]
fn long4_canonical_check_and_nx() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x5000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    // Leaf is NX.
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64 | PTE_NX);

    mmu.set_cr3(pml4_base);
    mmu.set_cr4(CR4_PAE);
    mmu.set_efer(EFER_LME | EFER_NXE);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x789u64;
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );

    let pf = mmu
        .translate(&mut mem, vaddr, AccessType::Execute, 3)
        .unwrap_err();
    assert_eq!(
        pf,
        TranslateFault::PageFault(PageFault {
            addr: vaddr,
            error_code: pf_error_code(true, AccessType::Execute, true, false),
        })
    );

    // Non-canonical addresses should fail before paging.
    let non_canonical = 0x0001_0000_0000_0000u64;
    assert_eq!(
        mmu.translate(&mut mem, non_canonical, AccessType::Read, 0),
        Err(TranslateFault::NonCanonical(non_canonical))
    );
}

#[test]
fn tlb_hit_avoids_page_walk_and_invlpg_forces_miss() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x8000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64);

    mmu.set_cr3(pml4_base);
    mmu.set_cr4(CR4_PAE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x234u64;

    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert!(mem.reads() > 0);

    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert_eq!(mem.reads(), 0);
    assert_eq!(mem.writes(), 0);

    mmu.invlpg(vaddr);

    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert!(mem.reads() > 0);
}

#[test]
fn pae_2mb_large_page_translation() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pdpt_base = 0x1000u64;
    let pd_base = 0x2000u64;

    // PDPTE[0] -> PD
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64);

    // PDE[0] maps a 2MB page with base 0 (aligned), PS=1.
    mem.write_u64_raw(pd_base, PTE_P64 | PTE_RW64 | PTE_US64 | PTE_PS64);

    mmu.set_cr3(pdpt_base);
    mmu.set_cr4(CR4_PAE | CR4_PSE);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x0010_1234u64; // within first 2MB
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(vaddr)
    );
}

#[test]
fn pae_pdpt_reserved_bits_cause_rsvd_page_fault() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x20000);

    let pdpt_base = 0x1000u64;
    let pd_base = 0x2000u64;

    // Set a reserved bit (bit 1) in the PDPTE; in IA-32 PAE this must generate
    // a reserved-bit violation page fault when used.
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64);

    mmu.set_cr3(pdpt_base);
    mmu.set_cr4(CR4_PAE);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x1234u64;
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 0),
        Err(TranslateFault::PageFault(PageFault {
            addr: vaddr,
            error_code: pf_error_code(true, AccessType::Read, false, true),
        }))
    );
}

#[test]
fn long4_large_pages_2mb_and_1gb_translation() {
    // 2MB translation via PDE.PS
    {
        let mut mmu = Mmu::new();
        let mut mem = TestMemory::new(0x40000);

        let pml4_base = 0x1000u64;
        let pdpt_base = 0x2000u64;
        let pd_base = 0x3000u64;

        mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
        mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
        // PDE[0] maps 2MB page base=0.
        mem.write_u64_raw(pd_base, PTE_P64 | PTE_RW64 | PTE_US64 | PTE_PS64);

        mmu.set_cr3(pml4_base);
        mmu.set_cr4(CR4_PAE | CR4_PSE);
        mmu.set_efer(EFER_LME);
        mmu.set_cr0(CR0_PG);

        let vaddr = 0x0010_5678u64;
        assert_eq!(
            mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
            Ok(vaddr)
        );
    }

    // 1GB translation via PDPTE.PS
    {
        let mut mmu = Mmu::new();
        let mut mem = TestMemory::new(0x40000);

        let pml4_base = 0x1000u64;
        let pdpt_base = 0x2000u64;

        mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
        // PDPTE[0] maps 1GB page base=0.
        mem.write_u64_raw(pdpt_base, PTE_P64 | PTE_RW64 | PTE_US64 | PTE_PS64);

        mmu.set_cr3(pml4_base);
        mmu.set_cr4(CR4_PAE | CR4_PSE);
        mmu.set_efer(EFER_LME);
        mmu.set_cr0(CR0_PG);

        let vaddr = 0x0020_1234u64;
        assert_eq!(
            mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
            Ok(vaddr)
        );
    }
}

#[test]
fn permission_faults_and_wp_semantics() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x8000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);

    // Supervisor-only, read-only page.
    mem.write_u64_raw(pt_base, page_base | PTE_P64);

    mmu.set_cr3(pml4_base);
    mmu.set_cr4(CR4_PAE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x123u64;

    // User read should fault (#PF protection, user=1).
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Err(TranslateFault::PageFault(PageFault {
            addr: vaddr,
            error_code: pf_error_code(true, AccessType::Read, true, false),
        }))
    );

    // Supervisor write should succeed with WP=0 (CR0.WP=0) and set dirty.
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Write, 0),
        Ok(page_base + vaddr)
    );
    assert_ne!(mem.read_u64_raw(pt_base) & PTE_D64, 0);

    // Enabling CR0.WP should make supervisor writes fault.
    mmu.set_cr0(CR0_PG | CR0_WP);
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Write, 0),
        Err(TranslateFault::PageFault(PageFault {
            addr: vaddr,
            error_code: pf_error_code(true, AccessType::Write, false, false),
        }))
    );
}

#[test]
fn global_pages_survive_cr3_reload_with_pge() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x8000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64 | PTE_G64);

    mmu.set_cr3(pml4_base);
    mmu.set_cr4(CR4_PAE | CR4_PGE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x234u64;

    // Fill the TLB.
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );

    // CR3 reload should keep global entries.
    mmu.set_cr3(pml4_base);

    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert_eq!(mem.reads(), 0);
    assert_eq!(mem.writes(), 0);
}

#[test]
fn fuzz_long4_4kb_walk_matches_reference_model() {
    #[derive(Clone)]
    struct XorShift64(u64);

    impl XorShift64 {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn gen_bool(&mut self) -> bool {
            self.next_u64() & 1 != 0
        }
    }

    fn ref_translate_long4_4kb(
        mem: &TestMemory,
        pml4_base: u64,
        wp: bool,
        nx_enabled: bool,
        vaddr: u64,
        access: AccessType,
        is_user: bool,
    ) -> Result<u64, PageFault> {
        let addr_mask = (1u64 << 52) - 1;
        let pml4e = mem.read_u64_raw(pml4_base + (((vaddr >> 39) & 0x1ff) * 8));
        if pml4e & PTE_P64 == 0 {
            return Err(PageFault {
                addr: vaddr,
                error_code: pf_error_code(false, access, is_user, false),
            });
        }
        let mut user_ok = pml4e & PTE_US64 != 0;
        let mut writable_ok = pml4e & PTE_RW64 != 0;
        let mut nx = nx_enabled && (pml4e & PTE_NX != 0);

        let pdpt_base = (pml4e & addr_mask) & !0xfff;
        let pdpte = mem.read_u64_raw(pdpt_base + (((vaddr >> 30) & 0x1ff) * 8));
        if pdpte & PTE_P64 == 0 {
            return Err(PageFault {
                addr: vaddr,
                error_code: pf_error_code(false, access, is_user, false),
            });
        }
        user_ok &= pdpte & PTE_US64 != 0;
        writable_ok &= pdpte & PTE_RW64 != 0;
        nx |= nx_enabled && (pdpte & PTE_NX != 0);

        let pd_base = (pdpte & addr_mask) & !0xfff;
        let pde = mem.read_u64_raw(pd_base + (((vaddr >> 21) & 0x1ff) * 8));
        if pde & PTE_P64 == 0 {
            return Err(PageFault {
                addr: vaddr,
                error_code: pf_error_code(false, access, is_user, false),
            });
        }
        user_ok &= pde & PTE_US64 != 0;
        writable_ok &= pde & PTE_RW64 != 0;
        nx |= nx_enabled && (pde & PTE_NX != 0);

        let pt_base = (pde & addr_mask) & !0xfff;
        let pte = mem.read_u64_raw(pt_base + (((vaddr >> 12) & 0x1ff) * 8));
        if pte & PTE_P64 == 0 {
            return Err(PageFault {
                addr: vaddr,
                error_code: pf_error_code(false, access, is_user, false),
            });
        }
        user_ok &= pte & PTE_US64 != 0;
        writable_ok &= pte & PTE_RW64 != 0;
        nx |= nx_enabled && (pte & PTE_NX != 0);

        if is_user && !user_ok {
            return Err(PageFault {
                addr: vaddr,
                error_code: pf_error_code(true, access, is_user, false),
            });
        }
        if access.is_write() && !writable_ok && (is_user || wp) {
            return Err(PageFault {
                addr: vaddr,
                error_code: pf_error_code(true, access, is_user, false),
            });
        }
        if access.is_execute() && nx_enabled && nx {
            return Err(PageFault {
                addr: vaddr,
                error_code: pf_error_code(true, access, is_user, false),
            });
        }

        let pbase = (pte & addr_mask) & !0xfff;
        Ok(pbase + (vaddr & 0xfff))
    }

    let mut rng = XorShift64(0x1234_5678_9abc_def0);
    for _case in 0..50 {
        let mut mmu = Mmu::new();
        let mut mem = TestMemory::new(0x40000);

        let pml4_base = 0x1000u64;
        let pdpt_base = 0x2000u64;
        let pd_base = 0x3000u64;
        let pt_base = 0x4000u64;

        let wp = rng.gen_bool();
        let nx_enabled = rng.gen_bool();

        let mut pml4_flags = PTE_P64;
        if rng.gen_bool() {
            pml4_flags |= PTE_RW64;
        }
        if rng.gen_bool() {
            pml4_flags |= PTE_US64;
        }
        if nx_enabled && rng.gen_bool() {
            pml4_flags |= PTE_NX;
        }
        mem.write_u64_raw(pml4_base, pdpt_base | pml4_flags);

        let mut pdpt_flags = PTE_P64;
        if rng.gen_bool() {
            pdpt_flags |= PTE_RW64;
        }
        if rng.gen_bool() {
            pdpt_flags |= PTE_US64;
        }
        if nx_enabled && rng.gen_bool() {
            pdpt_flags |= PTE_NX;
        }
        mem.write_u64_raw(pdpt_base, pd_base | pdpt_flags);

        let mut pd_flags = PTE_P64;
        if rng.gen_bool() {
            pd_flags |= PTE_RW64;
        }
        if rng.gen_bool() {
            pd_flags |= PTE_US64;
        }
        if nx_enabled && rng.gen_bool() {
            pd_flags |= PTE_NX;
        }
        mem.write_u64_raw(pd_base, pt_base | pd_flags);

        for i in 0..512u64 {
            let present = rng.gen_bool();
            let mut pte = 0u64;
            if present {
                pte |= PTE_P64;
                if rng.gen_bool() {
                    pte |= PTE_RW64;
                }
                if rng.gen_bool() {
                    pte |= PTE_US64;
                }
                if nx_enabled && rng.gen_bool() {
                    pte |= PTE_NX;
                }

                // Pick a physical frame within the test memory.
                let frame = (rng.next_u64() as usize % 32) as u64;
                let paddr = 0x8000 + (frame * 0x1000);
                pte |= paddr;
            }
            mem.write_u64_raw(pt_base + i * 8, pte);
        }

        mmu.set_cr3(pml4_base);
        mmu.set_cr4(CR4_PAE);
        mmu.set_efer(EFER_LME | if nx_enabled { EFER_NXE } else { 0 });
        mmu.set_cr0(CR0_PG | if wp { CR0_WP } else { 0 });

        for _ in 0..200 {
            let vaddr = rng.next_u64() & 0x1ff_fff;
            let access = match rng.next_u64() % 3 {
                0 => AccessType::Read,
                1 => AccessType::Write,
                _ => AccessType::Execute,
            };
            let is_user = rng.gen_bool();

            let expected =
                ref_translate_long4_4kb(&mem, pml4_base, wp, nx_enabled, vaddr, access, is_user)
                    .map_err(TranslateFault::PageFault);
            let got = mmu.translate(&mut mem, vaddr, access, if is_user { 3 } else { 0 });

            assert_eq!(
                got, expected,
                "mismatch at vaddr=0x{:x} access={access:?} user={is_user}",
                vaddr
            );
        }
    }
}

#[test]
fn pcid_tags_tlb_entries_and_invpcid_flushes_single_context() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x8000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64);

    mmu.set_cr4(CR4_PAE | CR4_PCIDE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);

    let base_cr3 = pml4_base;
    let vaddr = 0x234u64;

    // PCID=1: populate TLB.
    mmu.set_cr3(base_cr3 | 1);
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert!(mem.reads() > 0);

    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert_eq!(mem.reads(), 0);
    assert_eq!(mem.writes(), 0);

    // Switching to a different PCID should not match existing entries.
    mmu.set_cr3(base_cr3 | 2);
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert!(mem.reads() > 0);

    // Switching back with CR3[63]=1 (no-flush) preserves PCID=1 entries.
    mmu.set_cr3(base_cr3 | 1 | (1u64 << 63));
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert_eq!(mem.reads(), 0);
    assert_eq!(mem.writes(), 0);

    // INVPCID single-context should drop only the requested PCID.
    mmu.invpcid(1, InvpcidType::SingleContext);
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert!(mem.reads() > 0);
}

#[test]
fn pcid_invlpg_invalidates_current_pcid_only() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x8000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64);

    mmu.set_cr4(CR4_PAE | CR4_PCIDE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);

    let base_cr3 = pml4_base;
    let vaddr = 0x234u64;

    // Fill PCID=1.
    mmu.set_cr3(base_cr3 | 1);
    let _ = mmu.translate(&mut mem, vaddr, AccessType::Read, 3).unwrap();

    // Fill PCID=2.
    mmu.set_cr3(base_cr3 | 2);
    let _ = mmu.translate(&mut mem, vaddr, AccessType::Read, 3).unwrap();

    // Switch back to PCID=1 without flushing it, then INVLPG.
    mmu.set_cr3(base_cr3 | 1 | (1u64 << 63));
    mmu.invlpg(vaddr);

    // PCID=1 should miss now.
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert!(mem.reads() > 0);

    // PCID=2 should still hit (INVLPG shouldn't invalidate other PCIDs).
    mmu.set_cr3(base_cr3 | 2 | (1u64 << 63));
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert_eq!(mem.reads(), 0);
    assert_eq!(mem.writes(), 0);
}

#[test]
fn pcid_invlpg_invalidates_global_for_all_pcids() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x8000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64 | PTE_G64);

    mmu.set_cr4(CR4_PAE | CR4_PCIDE | CR4_PGE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);

    let base_cr3 = pml4_base;
    let vaddr = 0x234u64;

    // Populate global entry under PCID=1.
    mmu.set_cr3(base_cr3 | 1);
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );

    // Should hit under a different PCID because it's global.
    mmu.set_cr3(base_cr3 | 2);
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert_eq!(mem.reads(), 0);
    assert_eq!(mem.writes(), 0);

    // INVLPG must invalidate global translations too.
    mmu.invlpg(vaddr);
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert!(mem.reads() > 0);
}

#[test]
fn invpcid_individual_address_targets_only_specified_pcid() {
    let mut mmu = Mmu::new();
    let mut mem = TestMemory::new(0x40000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x8000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64);

    mmu.set_cr4(CR4_PAE | CR4_PCIDE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);

    let base_cr3 = pml4_base;
    let vaddr = 0x234u64;

    // Fill PCID=1.
    mmu.set_cr3(base_cr3 | 1);
    let _ = mmu.translate(&mut mem, vaddr, AccessType::Read, 3).unwrap();

    // Fill PCID=2.
    mmu.set_cr3(base_cr3 | 2);
    let _ = mmu.translate(&mut mem, vaddr, AccessType::Read, 3).unwrap();

    // Invalidate only the mapping for PCID=1.
    mmu.invpcid(1, InvpcidType::IndividualAddress(vaddr));

    // PCID=1 should miss now.
    mmu.set_cr3(base_cr3 | 1 | (1u64 << 63));
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert!(mem.reads() > 0);

    // PCID=2 should still hit.
    mmu.set_cr3(base_cr3 | 2 | (1u64 << 63));
    mem.reset_counters();
    assert_eq!(
        mmu.translate(&mut mem, vaddr, AccessType::Read, 3),
        Ok(page_base + vaddr)
    );
    assert_eq!(mem.reads(), 0);
    assert_eq!(mem.writes(), 0);
}
