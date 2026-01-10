use crate::bus::MemoryBus;
use crate::mmu::AccessType;

use super::helpers::{new_bus, new_mmu_long, TlbMmu};

#[test]
fn invlpg_invalidation_forces_rewalk() {
    let mut bus = new_bus();
    let cr3 = 0x1000;
    let pdpt = 0x2000;
    let pd = 0x3000;
    let pt = 0x4000;
    let vaddr = 0x0000_0000_0000_1234u64;

    bus.write_u64(cr3, pdpt | 0x003);
    bus.write_u64(pdpt, pd | 0x003);
    bus.write_u64(pd, pt | 0x003);

    let p1 = 0x5000u64;
    let p2 = 0x6000u64;
    let pt_index = (vaddr >> 12) & 0x1FF;
    bus.write_u64(pt + pt_index * 8, p1 | 0x003);

    let mut mmu = TlbMmu::new(new_mmu_long(cr3, true));

    assert_eq!(
        mmu.translate_with_tlb(&mut bus, vaddr, AccessType::Read)
            .unwrap(),
        p1 | (vaddr & 0xFFF)
    );

    // Change the PTE; without invalidation, TLB should keep the old mapping.
    bus.write_u64(pt + pt_index * 8, p2 | 0x003);
    assert_eq!(
        mmu.translate_with_tlb(&mut bus, vaddr, AccessType::Read)
            .unwrap(),
        p1 | (vaddr & 0xFFF)
    );

    mmu.invalidate_page(vaddr);
    assert_eq!(
        mmu.translate_with_tlb(&mut bus, vaddr, AccessType::Read)
            .unwrap(),
        p2 | (vaddr & 0xFFF)
    );
}
