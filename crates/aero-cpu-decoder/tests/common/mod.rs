// Shared test helpers (integration tests compile as separate crates, so put
// common code in a submodule to avoid it becoming its own test target).

/// Tiny deterministic PRNG for test input generation.
///
/// We want deterministic pseudo-random bytes for golden tests so failures are
/// reproducible across machines/CI.
pub struct XorShift64(pub u64);

impl XorShift64 {
    pub fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&v[..n]);
        }
    }
}
