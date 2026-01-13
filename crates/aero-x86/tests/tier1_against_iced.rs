#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_decoder::{decode_instruction, DecodeMode, Mnemonic};
use aero_x86::tier1::{self, InstKind};
use proptest::prelude::*;

fn arb_bitness() -> impl Strategy<Value = u32> {
    prop_oneof![Just(16u32), Just(32u32), Just(64u32)]
}

fn ip_mask(bitness: u32) -> u64 {
    match bitness {
        16 => 0xffff,
        32 => 0xffff_ffff,
        64 => u64::MAX,
        _ => unreachable!("bitness must be 16/32/64"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, .. ProptestConfig::default() })]

    #[test]
    fn tier1_supported_subset_matches_iced(
        bytes in proptest::collection::vec(any::<u8>(), 1..=15),
        bitness in arb_bitness(),
        rip in any::<u64>(),
    ) {
        let rip = rip & ip_mask(bitness);

        let t1 = tier1::decode_one_mode(rip, &bytes, bitness);
        if matches!(t1.kind, InstKind::Invalid) {
            // Tier-1 intentionally bails out to the interpreter for unsupported
            // opcodes/prefixes/addressing modes.
            return Ok(());
        }

        let mode = match bitness {
            16 => DecodeMode::Bits16,
            32 => DecodeMode::Bits32,
            64 => DecodeMode::Bits64,
            _ => unreachable!(),
        };

        let iced = decode_instruction(mode, rip, &bytes);
        prop_assert!(
            iced.is_ok(),
            "tier1 decoded as supported but iced failed: bitness={} rip={:#x} bytes={:?} tier1={:?} iced={:?}",
            bitness,
            rip,
            bytes,
            t1,
            iced,
        );
        let iced = iced.unwrap();

        prop_assert_eq!(
            t1.len as usize,
            iced.len(),
            "instruction length mismatch: bitness={} rip={:#x} bytes={:?} tier1={:?} iced={:?}",
            bitness,
            rip,
            bytes,
            t1,
            iced,
        );

        match t1.kind {
            InstKind::JmpRel { target }
            | InstKind::CallRel { target }
            | InstKind::JccRel { target, .. } => {
                prop_assert_eq!(
                    target,
                    iced.near_branch_target(),
                    "near branch target mismatch: bitness={} rip={:#x} bytes={:?} tier1={:?} iced={:?}",
                    bitness,
                    rip,
                    bytes,
                    t1,
                    iced,
                );
            }
            InstKind::Nop => {
                // Tier-1 treats NOP-ish encodings (eg. `0x90`, `F3 90`/`PAUSE`, multi-byte NOPs)
                // as a single `Nop` kind. Ensure iced did not decode it as a real `XCHG` (e.g.
                // `REX.B 90`) or some other instruction with side effects.
                prop_assert!(
                    matches!(iced.mnemonic(), Mnemonic::Nop | Mnemonic::Pause),
                    "tier1 decoded Nop but iced decoded as {:?} ({:?})",
                    iced.mnemonic(),
                    iced.code(),
                );
            }
            _ => {}
        }
    }
}
