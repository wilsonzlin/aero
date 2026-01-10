use crate::bus::MemoryBus;
use crate::mmu::{AccessType, TranslateError, PFEC_ID, PFEC_P, PFEC_RSVD, CR4_PGE};

use super::helpers::{new_bus, new_mmu_long, TlbMmu};

#[test]
fn long_mode_minimal_4k_mapping_translation_and_ad_bits() {
    let mut bus = new_bus();

    let cr3 = 0x1000;
    let pdpt_base = 0x2000;
    let pd_base = 0x3000;
    let pt_base = 0x4000;
    let phys_page = 0x5000;

    let vaddr = 0x0000_0000_0040_0123u64;

    // PML4E[0] -> PDPT
    bus.write_u64(cr3 + 0 * 8, pdpt_base | 0x003);
    // PDPTE[0] -> PD
    bus.write_u64(pdpt_base + 0 * 8, pd_base | 0x003);
    // PDE[2] -> PT
    bus.write_u64(pd_base + 2 * 8, pt_base | 0x003);
    // PTE[0] -> phys
    bus.write_u64(pt_base + 0 * 8, phys_page | 0x003);

    let mut mmu = new_mmu_long(cr3, true);
    let paddr = mmu.translate(&mut bus, vaddr, AccessType::Write, mmu.cpl).unwrap();
    assert_eq!(paddr, phys_page + (vaddr & 0xFFF));

    let pml4e = bus.read_u64(cr3);
    let pdpte = bus.read_u64(pdpt_base);
    let pde = bus.read_u64(pd_base + 2 * 8);
    let pte = bus.read_u64(pt_base);
    assert_ne!(pml4e & (1 << 5), 0, "PML4E.A should be set");
    assert_ne!(pdpte & (1 << 5), 0, "PDPTE.A should be set");
    assert_ne!(pde & (1 << 5), 0, "PDE.A should be set");
    assert_ne!(pte & (1 << 5), 0, "PTE.A should be set");
    assert_ne!(pte & (1 << 6), 0, "PTE.D should be set on write");
}

#[test]
fn long_mode_large_pages_2mb_and_1gb() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let pdpt_base = 0x2000;
    let pd_base = 0x3000;

    // Shared PML4E[0]
    bus.write_u64(cr3 + 0 * 8, pdpt_base | 0x003);

    // 1GB page via PDPTE[1]
    let phys_1g = 0x4000_0000u64; // 1GB aligned
    let vaddr_1g = 0x0000_0000_4000_1234u64;
    bus.write_u64(pdpt_base + 1 * 8, phys_1g | 0x083);

    // 2MB page via PDE[0] under PDPTE[0]
    let phys_2m = 0x0080_0000u64; // 2MB aligned
    let vaddr_2m = 0x0000_0000_0000_5678u64;
    bus.write_u64(pdpt_base + 0 * 8, pd_base | 0x003);
    bus.write_u64(pd_base + 0 * 8, phys_2m | 0x083);

    let mut mmu = new_mmu_long(cr3, true);
    let paddr_1g = mmu.translate(&mut bus, vaddr_1g, AccessType::Read, mmu.cpl).unwrap();
    assert_eq!(paddr_1g, phys_1g + (vaddr_1g & 0x3FFF_FFFF));

    let paddr_2m = mmu.translate(&mut bus, vaddr_2m, AccessType::Read, mmu.cpl).unwrap();
    assert_eq!(paddr_2m, phys_2m + (vaddr_2m & 0x1F_FFFF));
}

#[test]
fn long_mode_nx_fault() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let pdpt_base = 0x2000;
    let pd_base = 0x3000;
    let pt_base = 0x4000;
    let phys_page = 0x5000;
    let vaddr = 0x0000_0000_0040_0000u64;

    bus.write_u64(cr3, pdpt_base | 0x003);
    bus.write_u64(pdpt_base, pd_base | 0x003);
    bus.write_u64(pd_base + 2 * 8, pt_base | 0x003);
    bus.write_u64(pt_base, phys_page | 0x8000_0000_0000_0003u64); // NX + P + RW

    let mut mmu = new_mmu_long(cr3, true);
    let err = mmu
        .translate(&mut bus, vaddr, AccessType::Execute, mmu.cpl)
        .unwrap_err();

    match err {
        TranslateError::PageFault(pf) => assert_eq!(pf.error_code, PFEC_P | PFEC_ID),
        other => panic!("expected page fault, got {other:?}"),
    }
}

#[test]
fn long_mode_rsvd_fault_when_nx_disabled() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let pdpt_base = 0x2000;
    let pd_base = 0x3000;
    let pt_base = 0x4000;
    let phys_page = 0x5000;
    let vaddr = 0x0000_0000_0040_0000u64;

    bus.write_u64(cr3, pdpt_base | 0x003);
    bus.write_u64(pdpt_base, pd_base | 0x003);
    bus.write_u64(pd_base + 2 * 8, pt_base | 0x003);
    bus.write_u64(pt_base, phys_page | 0x8000_0000_0000_0003u64); // NX set but NXE disabled

    let mut mmu = new_mmu_long(cr3, false);
    let err = mmu.translate(&mut bus, vaddr, AccessType::Read, mmu.cpl).unwrap_err();

    match err {
        TranslateError::PageFault(pf) => assert_eq!(pf.error_code, PFEC_P | PFEC_RSVD),
        other => panic!("expected page fault, got {other:?}"),
    }
}

#[test]
fn long_mode_non_canonical_address_is_gp() {
    let mut bus = new_bus();
    let mut mmu = new_mmu_long(0x1000, true);

    let err = mmu
        .translate(&mut bus, 0x0000_8000_0000_0000u64, AccessType::Read, mmu.cpl)
        .unwrap_err();
    assert!(matches!(err, TranslateError::GeneralProtection { .. }));
}

#[test]
fn tlb_global_vs_non_global_flush_on_cr3_switch() {
    let mut bus = new_bus();
    let cr3_a = 0x1000;
    let cr3_b = 0x2000;
    let pdpt_a = 0x3000;
    let pdpt_b = 0x4000;
    let pd_a = 0x5000;
    let pd_b = 0x6000;
    let pt_a = 0x7000;
    let pt_b = 0x8000;

    let v_global = 0x0000_0000_0000_1000u64;
    let v_local = 0x0000_0000_0000_2000u64;

    let p_global_a = 0x9000;
    let p_global_b = 0xA000;
    let p_local_a = 0xB000;
    let p_local_b = 0xC000;

    // Build CR3 A tables.
    bus.write_u64(cr3_a, pdpt_a | 0x003);
    bus.write_u64(pdpt_a, pd_a | 0x003);
    bus.write_u64(pd_a, pt_a | 0x003);
    bus.write_u64(pt_a + 1 * 8, p_global_a | 0x103); // global mapping: P|RW|G
    bus.write_u64(pt_a + 2 * 8, p_local_a | 0x003); // local mapping: P|RW

    // Build CR3 B tables.
    bus.write_u64(cr3_b, pdpt_b | 0x003);
    bus.write_u64(pdpt_b, pd_b | 0x003);
    bus.write_u64(pd_b, pt_b | 0x003);
    bus.write_u64(pt_b + 1 * 8, p_global_b | 0x103);
    bus.write_u64(pt_b + 2 * 8, p_local_b | 0x003);

    let mut mmu = TlbMmu::new(new_mmu_long(cr3_a, true));
    mmu.mmu.cr4 |= CR4_PGE;

    assert_eq!(
        mmu.translate_with_tlb(&mut bus, v_global, AccessType::Read).unwrap(),
        p_global_a
    );
    assert_eq!(
        mmu.translate_with_tlb(&mut bus, v_local, AccessType::Read).unwrap(),
        p_local_a
    );
    assert_eq!(mmu.tlb_len(), 2);

    // Switch address space; global should remain cached, local should be flushed.
    mmu.set_cr3(cr3_b);

    assert_eq!(
        mmu.translate_with_tlb(&mut bus, v_global, AccessType::Read).unwrap(),
        p_global_a
    );
    assert_eq!(
        mmu.translate_with_tlb(&mut bus, v_local, AccessType::Read).unwrap(),
        p_local_b
    );
}
