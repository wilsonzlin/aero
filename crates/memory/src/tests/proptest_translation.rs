use crate::bus::MemoryBus;
use crate::mmu::AccessType;
use proptest::prelude::*;

use super::helpers::{new_bus, new_mmu_long, TlbMmu};

#[derive(Clone, Debug)]
struct Mapping {
    present: bool,
    writable: bool,
    user: bool,
    nx: bool,
    phys_page: u64,
}

prop_compose! {
    fn arb_mapping(max_phys_pages: u64)(
        present in any::<bool>(),
        writable in any::<bool>(),
        user in any::<bool>(),
        nx in any::<bool>(),
        phys_page in 0u64..max_phys_pages,
    ) -> Mapping {
        Mapping {
            present,
            writable,
            user,
            nx,
            phys_page: phys_page << 12,
        }
    }
}

fn build_long_mode_tables(mappings: &[Mapping]) -> (crate::Bus, crate::mmu::Mmu) {
    let mut bus = new_bus();

    let cr3 = 0x1000u64;
    let pdpt = 0x2000u64;
    let pd = 0x3000u64;
    let pt = 0x4000u64;

    let top_flags = 0x007u64; // P|RW|US
    bus.write_u64(cr3, pdpt | top_flags);
    bus.write_u64(pdpt, pd | top_flags);
    bus.write_u64(pd, pt | top_flags);

    for (i, mapping) in mappings.iter().enumerate() {
        let mut entry = mapping.phys_page;
        if mapping.present {
            entry |= 1;
        }
        if mapping.writable {
            entry |= 1 << 1;
        }
        if mapping.user {
            entry |= 1 << 2;
        }
        if mapping.nx {
            entry |= 1u64 << 63;
        }
        bus.write_u64(pt + (i as u64) * 8, entry);
    }

    let mut mmu = new_mmu_long(cr3, true);
    mmu.cpl = 3;
    (bus, mmu)
}

fn arb_access() -> impl Strategy<Value = AccessType> {
    prop_oneof![
        Just(AccessType::Read),
        Just(AccessType::Write),
        Just(AccessType::Execute),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn translate_with_tlb_matches_without_tlb(
        mappings in prop::collection::vec(arb_mapping(64), 1..16),
        accesses in prop::collection::vec((0usize..16usize, 0u16..4096u16, arb_access()), 1..32),
    ) {
        let (mut bus_walk, mut mmu_walk) = build_long_mode_tables(&mappings);
        let (mut bus_tlb, mmu_tlb) = build_long_mode_tables(&mappings);
        let mut mmu_tlb = TlbMmu::new(mmu_tlb);

        for (page_idx, offset, access) in accesses {
            let vaddr = ((page_idx as u64) << 12) | (offset as u64);

            let cpl = mmu_walk.cpl;
            let res_walk = mmu_walk.translate(&mut bus_walk, vaddr, access, cpl);
            let res_tlb = mmu_tlb.translate_with_tlb(&mut bus_tlb, vaddr, access);
            prop_assert_eq!(res_walk, res_tlb);
        }
    }
}
