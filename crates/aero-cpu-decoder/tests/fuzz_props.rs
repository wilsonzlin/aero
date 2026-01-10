use aero_cpu_decoder::{decode_one, DecodeMode, MAX_INSTRUCTION_LEN};
use proptest::prelude::*;

proptest! {
    // Property test: decoder must never panic, and if it succeeds it must
    // report a sane instruction length.
    #[test]
    fn decode_never_panics_and_len_is_sane(bytes in proptest::collection::vec(any::<u8>(), 0..=MAX_INSTRUCTION_LEN)) {
        let res = decode_one(DecodeMode::Bits64, 0x1000, &bytes);
        if let Ok(inst) = res {
            let len = inst.len() as usize;
            prop_assert!(len >= 1);
            prop_assert!(len <= MAX_INSTRUCTION_LEN);
        }
    }
}

