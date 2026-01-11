use crate::bus::MemoryBus;
use crate::mmu::{AccessType, TranslateError, PFEC_RSVD};

use super::helpers::{new_bus, new_mmu_pae};

#[test]
fn pae_minimal_4k_mapping_translation_and_ad_bits() {
    let mut bus = new_bus();
    let cr3 = 0x1000; // PDPT (32-byte aligned)
    let pd_base = 0x2000;
    let pt_base = 0x3000;
    let phys_page = 0x4000;
    let vaddr = 0x0040_0123u64;

    // PDPT[0] -> PD
    bus.write_u64(cr3 + 0 * 8, (pd_base as u64) | 0x001);
    // PDE[2] -> PT
    bus.write_u64(pd_base + 2 * 8, (pt_base as u64) | 0x003);
    // PTE[0] -> phys
    bus.write_u64(pt_base + 0 * 8, (phys_page as u64) | 0x003);

    let mut mmu = new_mmu_pae(cr3);
    let cpl = mmu.cpl;
    let paddr = mmu
        .translate(&mut bus, vaddr, AccessType::Write, cpl)
        .unwrap();
    assert_eq!(paddr, phys_page + (vaddr & 0xFFF));

    let pde = bus.read_u64(pd_base + 2 * 8);
    let pte = bus.read_u64(pt_base + 0 * 8);
    assert_ne!(pde & (1 << 5), 0, "PDE.A should be set");
    assert_ne!(pte & (1 << 5), 0, "PTE.A should be set");
    assert_ne!(pte & (1 << 6), 0, "PTE.D should be set on write");
}

#[test]
fn pae_2mb_large_page_translation() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let pd_base = 0x2000;
    let vaddr = 0x0060_1234u64;
    let phys_base = 0x0080_0000u64; // 2MB aligned

    bus.write_u64(cr3 + 0 * 8, (pd_base as u64) | 0x001);
    // PDE[3] maps 2MB page
    bus.write_u64(pd_base + 3 * 8, phys_base | 0x083); // P|RW|PS

    let mut mmu = new_mmu_pae(cr3);
    let cpl = mmu.cpl;
    let paddr = mmu
        .translate(&mut bus, vaddr, AccessType::Read, cpl)
        .unwrap();
    assert_eq!(paddr, phys_base + (vaddr & 0x1F_FFFF));
}

#[test]
fn pae_rsvd_fault_on_misaligned_2mb_pde() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let pd_base = 0x2000;
    let vaddr = 0x0060_1234u64;

    bus.write_u64(cr3 + 0 * 8, (pd_base as u64) | 0x001);
    // Misaligned base.
    bus.write_u64(pd_base + 3 * 8, 0x0000_2000u64 | 0x083);

    let mut mmu = new_mmu_pae(cr3);
    let cpl = mmu.cpl;
    let err = mmu
        .translate(&mut bus, vaddr, AccessType::Read, cpl)
        .unwrap_err();

    match err {
        TranslateError::PageFault(pf) => assert_ne!(pf.error_code & PFEC_RSVD, 0),
        other => panic!("expected page fault, got {other:?}"),
    }
}
