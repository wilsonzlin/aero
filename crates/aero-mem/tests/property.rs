use aero_mem::{MemoryBus, PhysicalMemory, PhysicalMemoryOptions};
use proptest::prelude::*;
use std::sync::Arc;

proptest! {
    #[test]
    fn physical_memory_read_write_coherence(
        size in 1usize..=64 * 1024,
        ops in proptest::collection::vec(
            (0usize..=64 * 1024, proptest::collection::vec(any::<u8>(), 0usize..=256)),
            0usize..=128,
        )
    ) {
        let mem = PhysicalMemory::with_options(
            size as u64,
            PhysicalMemoryOptions { chunk_size: 4096 },
        ).unwrap();

        let mut model = vec![0u8; size];

        for (addr_raw, data) in ops {
            let addr = addr_raw % size;
            let max_len = size - addr;
            let len = data.len().min(max_len);
            if len == 0 {
                continue;
            }

            mem.try_write_bytes(addr as u64, &data[..len]).unwrap();
            model[addr..addr + len].copy_from_slice(&data[..len]);
        }

        let mut out = vec![0u8; size];
        mem.try_read_bytes(0, &mut out).unwrap();
        prop_assert_eq!(out, model);
    }

    #[test]
    fn memory_bus_bulk_matches_ram(
        size in 1usize..=64 * 1024,
        ops in proptest::collection::vec(
            (0usize..=64 * 1024, proptest::collection::vec(any::<u8>(), 0usize..=256)),
            0usize..=128,
        )
    ) {
        let ram = Arc::new(PhysicalMemory::with_options(
            size as u64,
            PhysicalMemoryOptions { chunk_size: 4096 },
        ).unwrap());
        let bus = MemoryBus::new(ram.clone());

        let mut model = vec![0u8; size];

        for (addr_raw, data) in ops {
            let addr = addr_raw % size;
            let max_len = size - addr;
            let len = data.len().min(max_len);
            if len == 0 {
                continue;
            }

            bus.try_write_bytes(addr as u64, &data[..len]).unwrap();
            model[addr..addr + len].copy_from_slice(&data[..len]);
        }

        let mut out = vec![0u8; size];
        bus.try_read_bytes(0, &mut out).unwrap();
        prop_assert_eq!(out, model);
    }
}
