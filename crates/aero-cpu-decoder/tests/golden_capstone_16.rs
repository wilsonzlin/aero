#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_decoder::{decode_one, DecodeMode, MAX_INSTRUCTION_LEN};
use capstone::prelude::*;

mod common;
use common::XorShift64;

#[test]
fn golden_decode_len_matches_capstone_x86_16() {
    let mut cs = Capstone::new()
        .x86()
        .mode(arch::x86::ArchMode::Mode16)
        .syntax(arch::x86::ArchSyntax::Intel)
        .detail(false)
        .build()
        .expect("capstone init");

    // Ensure Capstone doesn't try to "skipdata" over invalid bytes; we want strict decode.
    let _ = cs.set_skipdata(false);

    let mut rng = XorShift64(0x16B1_75E5_B005_16B5u64);

    // Collect a large set of random byte sequences and compare decoded length for all
    // cases where both decoders agree the first instruction is valid.
    const TARGET_MATCHES: usize = 5_000;
    const MAX_ATTEMPTS: usize = 500_000;

    let mut matches = 0usize;
    let mut attempts = 0usize;
    while matches < TARGET_MATCHES && attempts < MAX_ATTEMPTS {
        attempts += 1;

        let mut bytes = [0u8; MAX_INSTRUCTION_LEN];
        rng.fill(&mut bytes);

        let ip = 0x1000u64;
        let ours = decode_one(DecodeMode::Bits16, ip, &bytes);
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
        assert!((1..=MAX_INSTRUCTION_LEN).contains(&our_len));
        assert!((1..=MAX_INSTRUCTION_LEN).contains(&cap_len));

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
