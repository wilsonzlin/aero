#![cfg(not(target_arch = "wasm32"))]

use aero_x86::tier1::{decode_one_mode, InstKind};
use proptest::prelude::*;

fn arch_ip_mask(bitness: u32) -> u64 {
    match bitness {
        16 => 0xffff,
        32 => 0xffff_ffff,
        64 => u64::MAX,
        _ => unreachable!("bitness must be one of 16/32/64"),
    }
}

fn tier1_inputs() -> impl Strategy<Value = (u32, u64, Vec<u8>)> {
    let bitness = prop_oneof![Just(16u32), Just(32u32), Just(64u32)];
    bitness.prop_flat_map(|bitness| {
        let rip = match bitness {
            16 => (0u64..=0xffff).boxed(),
            32 => (0u64..=0xffff_ffff).boxed(),
            64 => any::<u64>().boxed(),
            _ => unreachable!(),
        };
        let bytes = proptest::collection::vec(any::<u8>(), 1..=15);
        (Just(bitness), rip, bytes)
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 4096,
        .. ProptestConfig::default()
    })]

    #[test]
    fn tier1_decode_one_mode_never_panics_and_has_sane_addrs((bitness, rip, bytes) in tier1_inputs()) {
        let inst = decode_one_mode(rip, &bytes, bitness);

        prop_assert!(inst.len >= 1, "inst.len={} bitness={} rip=0x{:x} bytes={:02x?}", inst.len, bitness, rip, bytes);
        prop_assert!(inst.len <= 15, "inst.len={} bitness={} rip=0x{:x} bytes={:02x?}", inst.len, bitness, rip, bytes);

        // 64-bit mode uses full u64 RIP. In 16/32-bit modes, verify the decoder doesn't emit
        // out-of-range IPs for fallthrough or relative branches.
        let mask = arch_ip_mask(bitness);
        if bitness != 64 {
            prop_assert_eq!(
                inst.next_rip() & !mask,
                0,
                "next_rip=0x{:x} mask=0x{:x} bitness={} rip=0x{:x} bytes={:02x?}",
                inst.next_rip(),
                mask,
                bitness,
                rip,
                bytes
            );

            match inst.kind {
                InstKind::JmpRel { target } | InstKind::CallRel { target } => {
                    prop_assert_eq!(target & !mask, 0, "target=0x{:x} mask=0x{:x} bitness={} rip=0x{:x} bytes={:02x?}", target, mask, bitness, rip, bytes);
                }
                InstKind::JccRel { target, .. } => {
                    prop_assert_eq!(target & !mask, 0, "target=0x{:x} mask=0x{:x} bitness={} rip=0x{:x} bytes={:02x?}", target, mask, bitness, rip, bytes);
                }
                _ => {}
            }
        }
    }
}
