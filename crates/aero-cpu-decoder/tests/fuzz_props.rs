#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_decoder::{decode_one, DecodeMode, MAX_INSTRUCTION_LEN};
use proptest::prelude::*;

proptest! {
    // Property test: decoder must never panic, and if it succeeds it must
    // report a sane instruction length.
    #[test]
    fn decode_never_panics_and_len_is_sane_bits64(bytes in proptest::collection::vec(any::<u8>(), 0..=MAX_INSTRUCTION_LEN)) {
        let res = decode_one(DecodeMode::Bits64, 0x1000, &bytes);
        if let Ok(inst) = res {
            let len = inst.len() as usize;
            prop_assert!(len >= 1);
            prop_assert!(len <= MAX_INSTRUCTION_LEN);
        }
    }

    #[test]
    fn decode_never_panics_and_len_is_sane_bits32(bytes in proptest::collection::vec(any::<u8>(), 0..=MAX_INSTRUCTION_LEN)) {
        let res = decode_one(DecodeMode::Bits32, 0x1000, &bytes);
        if let Ok(inst) = res {
            let len = inst.len() as usize;
            prop_assert!(len >= 1);
            prop_assert!(len <= MAX_INSTRUCTION_LEN);
        }
    }

    #[test]
    fn decode_never_panics_and_len_is_sane_bits16(bytes in proptest::collection::vec(any::<u8>(), 0..=MAX_INSTRUCTION_LEN)) {
        let res = decode_one(DecodeMode::Bits16, 0x1000, &bytes);
        if let Ok(inst) = res {
            let len = inst.len() as usize;
            prop_assert!(len >= 1);
            prop_assert!(len <= MAX_INSTRUCTION_LEN);
        }
    }
}
