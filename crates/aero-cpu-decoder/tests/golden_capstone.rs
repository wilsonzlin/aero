use aero_cpu_decoder::{decode_one, DecodeMode, MAX_INSTRUCTION_LEN};
use capstone::prelude::*;

// Tiny deterministic PRNG for test input generation.
struct XorShift64(u64);

impl XorShift64 {
    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&v[..n]);
        }
    }
}

#[test]
fn golden_decode_len_matches_capstone_x86_64() {
    let mut cs = Capstone::new()
        .x86()
        .mode(arch::x86::ArchMode::Mode64)
        .syntax(arch::x86::ArchSyntax::Intel)
        .detail(false)
        .build()
        .expect("capstone init");

    // Ensure Capstone doesn't try to "skipdata" over invalid bytes; we want strict decode.
    let _ = cs.set_skipdata(false);

    let mut rng = XorShift64(0xD1CE_BA5E_C0DE_CAFE);

    // Collect a large set of random byte sequences and compare decoded length for all
    // cases where both decoders agree the first instruction is valid.
    const TARGET_MATCHES: usize = 10_000;
    const MAX_ATTEMPTS: usize = 500_000;

    let mut matches = 0usize;
    let mut attempts = 0usize;
    while matches < TARGET_MATCHES && attempts < MAX_ATTEMPTS {
        attempts += 1;

        let mut bytes = [0u8; MAX_INSTRUCTION_LEN];
        rng.fill(&mut bytes);

        let ip = 0x1000u64;
        let ours = decode_one(DecodeMode::Bits64, ip, &bytes);
        let cap = cs.disasm_count(&bytes, ip, 1);

        let (Ok(ours), Ok(cap)) = (ours, cap) else {
            continue;
        };
        let Some(cap_ins) = cap.iter().next() else {
            continue;
        };

        let our_len = ours.len() as usize;
        let cap_len = cap_ins.bytes().len();

        // Sanity: architectural max length.
        assert!(our_len >= 1 && our_len <= MAX_INSTRUCTION_LEN);
        assert!(cap_len >= 1 && cap_len <= MAX_INSTRUCTION_LEN);

        assert_eq!(
            our_len, cap_len,
            "length mismatch @attempt={attempts}, bytes={:02X?}",
            bytes
        );
        matches += 1;
    }

    assert!(
        matches >= TARGET_MATCHES,
        "only matched {matches} instructions after {attempts} attempts"
    );
}
