use crate::bus::MemoryBus;
use crate::mmu::{AccessType, TranslateError, PFEC_P, PFEC_RSVD, PFEC_US, PFEC_WR};

use super::helpers::{new_bus, new_mmu_legacy32};

#[test]
fn legacy32_minimal_4k_mapping_translation_and_ad_bits() {
    let mut bus = new_bus();

    let cr3 = 0x1000;
    let pt_base = 0x2000;
    let phys_page = 0x3000;
    let vaddr = 0x0040_0123u64;

    // PDE[1] -> PT
    bus.write_u32(cr3 + 1 * 4, (pt_base as u32) | 0x003); // P|RW
                                                          // PTE[0] -> phys
    bus.write_u32(pt_base + 0 * 4, (phys_page as u32) | 0x003); // P|RW

    let mut mmu = new_mmu_legacy32(cr3);
    let paddr = mmu
        .translate(&mut bus, vaddr, AccessType::Write, mmu.cpl)
        .unwrap();
    assert_eq!(paddr, phys_page + (vaddr & 0xFFF));

    let pde = bus.read_u32(cr3 + 1 * 4);
    let pte = bus.read_u32(pt_base + 0 * 4);
    assert_ne!(pde & (1 << 5), 0, "PDE.A should be set");
    assert_ne!(pte & (1 << 5), 0, "PTE.A should be set");
    assert_ne!(pte & (1 << 6), 0, "PTE.D should be set on write");
}

#[test]
fn legacy32_4mb_large_page_translation() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let vaddr = 0x0080_1234u64;
    let phys_base = 0x0100_0000u64; // 4MB aligned

    // PDE[2] maps 4MB page
    bus.write_u32(cr3 + 2 * 4, (phys_base as u32) | 0x083); // P|RW|PS

    let mut mmu = new_mmu_legacy32(cr3);
    let paddr = mmu
        .translate(&mut bus, vaddr, AccessType::Read, mmu.cpl)
        .unwrap();
    assert_eq!(paddr, phys_base + (vaddr & 0x3F_FFFF));
}

#[test]
fn legacy32_not_present_fault() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let mut mmu = new_mmu_legacy32(cr3);

    let err = mmu
        .translate(&mut bus, 0x0040_0000, AccessType::Read, mmu.cpl)
        .unwrap_err();

    match err {
        TranslateError::PageFault(pf) => assert_eq!(pf.error_code & PFEC_P, 0),
        other => panic!("expected page fault, got {other:?}"),
    }
}

#[test]
fn legacy32_protection_fault_on_user_write_to_read_only() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let pt_base = 0x2000;
    let phys_page = 0x3000;
    let vaddr = 0x0040_0000u64;

    // User-accessible mapping, read-only (RW=0).
    bus.write_u32(cr3 + 1 * 4, (pt_base as u32) | 0x005); // P|US
    bus.write_u32(pt_base + 0 * 4, (phys_page as u32) | 0x005); // P|US

    let mut mmu = new_mmu_legacy32(cr3);
    mmu.cpl = 3;
    let err = mmu
        .translate(&mut bus, vaddr, AccessType::Write, mmu.cpl)
        .unwrap_err();

    match err {
        TranslateError::PageFault(pf) => assert_eq!(pf.error_code, PFEC_P | PFEC_WR | PFEC_US),
        other => panic!("expected page fault, got {other:?}"),
    }
}

#[test]
fn legacy32_rsvd_fault_on_misaligned_4mb_pde() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let vaddr = 0x0080_0000u64;

    // Misaligned bits 21:13 set.
    bus.write_u32(cr3 + 2 * 4, 0x0000_2000u32 | 0x083);

    let mut mmu = new_mmu_legacy32(cr3);
    let err = mmu
        .translate(&mut bus, vaddr, AccessType::Read, mmu.cpl)
        .unwrap_err();

    match err {
        TranslateError::PageFault(pf) => assert_ne!(pf.error_code & PFEC_RSVD, 0),
        other => panic!("expected page fault, got {other:?}"),
    }
}
