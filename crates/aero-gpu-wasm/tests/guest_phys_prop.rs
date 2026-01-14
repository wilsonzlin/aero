use aero_guest_phys::{GuestRamChunk, HIGH_RAM_START, LOW_RAM_END};

/// Simple deterministic PRNG (xorshift64*) to avoid pulling in `rand`/`proptest` as dev-deps.
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_usize_range(&mut self, start_inclusive: usize, end_inclusive: usize) -> usize {
        debug_assert!(start_inclusive <= end_inclusive);
        let span = end_inclusive - start_inclusive + 1;
        start_inclusive + (self.next_u64() as usize % span)
    }
}

#[test]
fn range_to_offset_agrees_with_chunk_classification_for_random_ranges() {
    let mut rng = Rng::new(0x2a7b_3f91_104d_6a9d);

    for _ in 0..20_000 {
        // Generate RAM sizes with a bias around the interesting PC/Q35 boundaries, but also include
        // some "pathological" large values to ensure we never panic on overflow.
        let ram_bytes = match rng.next_u64() % 5 {
            0 => rng.next_u64() % 0x10000,                           // tiny
            1 => rng.next_u64() % (LOW_RAM_END + 0x10000),            // around ECAM base
            2 => LOW_RAM_END + (rng.next_u64() % 0x20000),            // slightly above ECAM base
            3 => LOW_RAM_END.saturating_sub(rng.next_u64() % 0x20000), // slightly below ECAM base
            _ => rng.next_u64(),                                     // full range
        };

        // Generate addresses biased toward region boundaries.
        let paddr = match rng.next_u64() % 7 {
            0 => rng.next_u64(),
            1 => rng.next_u64() % (LOW_RAM_END + 0x20000),
            2 => LOW_RAM_END.saturating_sub(rng.next_u64() % 0x1000),
            3 => LOW_RAM_END.saturating_add(rng.next_u64() % 0x1000),
            4 => HIGH_RAM_START.saturating_sub(rng.next_u64() % 0x1000),
            5 => HIGH_RAM_START.saturating_add(rng.next_u64() % 0x1000),
            _ => u64::MAX.saturating_sub(rng.next_u64() % 0x1000),
        };

        // Keep lengths small so the test stays fast and does not require large allocations.
        //
        // Include zero-length ranges: real DMA paths won't use them, but they are a useful edge case
        // for validating boundary classification behaviour.
        let len = rng.next_usize_range(0, 4096);

        let chunk = aero_guest_phys::translate_guest_paddr_chunk(ram_bytes, paddr, len);
        let offset =
            aero_guest_phys::translate_guest_paddr_range_to_offset(ram_bytes, paddr, len as u64);

        match offset {
            Some(ram_offset) => {
                assert_eq!(
                    chunk,
                    GuestRamChunk::Ram {
                        ram_offset,
                        len
                    },
                    "offset=Some implies a full-length RAM chunk: ram_bytes={ram_bytes:#x} paddr={paddr:#x} len={len}"
                );
            }
            None => {
                if len == 0 {
                    assert!(
                        !matches!(chunk, GuestRamChunk::Ram { .. }),
                        "offset=None must not classify as RAM even for len=0: ram_bytes={ram_bytes:#x} paddr={paddr:#x}"
                    );
                } else if let GuestRamChunk::Ram { len: chunk_len, .. } = chunk {
                    assert!(
                        chunk_len < len,
                        "offset=None must not coincide with a full-length RAM chunk: ram_bytes={ram_bytes:#x} paddr={paddr:#x} len={len} chunk_len={chunk_len}"
                    );
                }
            }
        }
    }
}
