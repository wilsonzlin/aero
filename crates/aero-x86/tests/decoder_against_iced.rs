use aero_x86::decoder::{decode, DecodeMode};
use iced_x86::{Code, Decoder, DecoderError, DecoderOptions, OpKind};
use proptest::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BasicOpKind {
    Reg,
    Mem,
    Imm,
    Rel,
    Other,
}

fn iced_valid_and_info(
    bitness: u32,
    bytes: &[u8],
) -> (bool, usize, Vec<(BasicOpKind, Option<u64>)>) {
    let mut decoder = Decoder::with_ip(bitness, bytes, 0, DecoderOptions::NONE);
    let inst = decoder.decode();

    let valid = decoder.last_error() == DecoderError::None && inst.code() != Code::INVALID;
    if !valid {
        return (false, 0, Vec::new());
    }

    let len = inst.len();

    let mut ops = Vec::new();
    for i in 0..inst.op_count() {
        let kind = inst.op_kind(i);
        let dbg_kind = format!("{kind:?}");
        let (basic, rel) = match kind {
            OpKind::Register => (BasicOpKind::Reg, None),
            OpKind::Memory => (BasicOpKind::Mem, None),

            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                (BasicOpKind::Rel, Some(inst.near_branch_target()))
            }

            // Far branches are modelled as "other" for now.
            OpKind::FarBranch16 | OpKind::FarBranch32 => (BasicOpKind::Other, None),

            OpKind::Immediate8
            | OpKind::Immediate8_2nd
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate64
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64 => (BasicOpKind::Imm, None),

            _ if dbg_kind.starts_with("Memory") => (BasicOpKind::Mem, None),
            _ => (BasicOpKind::Other, None),
        };
        ops.push((basic, rel));
    }

    (true, len, ops)
}

fn ours_valid_and_info(
    mode: DecodeMode,
    bytes: &[u8],
) -> (bool, usize, Vec<(BasicOpKind, Option<u64>)>) {
    let inst = match decode(bytes, mode, 0) {
        Ok(inst) => inst,
        Err(_) => return (false, 0, Vec::new()),
    };

    let mut ops = Vec::new();
    for op in &inst.operands {
        use aero_x86::inst::Operand as O;
        let (kind, rel) = match op {
            O::Gpr { .. }
            | O::Xmm { .. }
            | O::OtherReg { .. }
            | O::Segment { .. }
            | O::Control { .. }
            | O::Debug { .. } => (BasicOpKind::Reg, None),
            O::Memory(_) => (BasicOpKind::Mem, None),
            O::Immediate(_) => (BasicOpKind::Imm, None),
            O::Relative { target, .. } => (BasicOpKind::Rel, Some(*target)),
        };
        ops.push((kind, rel));
    }

    (true, inst.length as usize, ops)
}

fn arb_mode() -> impl Strategy<Value = DecodeMode> {
    prop_oneof![
        Just(DecodeMode::Bits16),
        Just(DecodeMode::Bits32),
        Just(DecodeMode::Bits64),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, .. ProptestConfig::default() })]

    #[test]
    fn differential_decode_matches_iced(
        bytes in proptest::collection::vec(any::<u8>(), 1..=15),
        mode in arb_mode(),
    ) {
        let bitness = match mode {
            DecodeMode::Bits16 => 16,
            DecodeMode::Bits32 => 32,
            DecodeMode::Bits64 => 64,
        };

        let (iced_ok, iced_len, iced_ops) = iced_valid_and_info(bitness, &bytes);
        let (ours_ok, ours_len, ours_ops) = ours_valid_and_info(mode, &bytes);

        prop_assert_eq!(ours_ok, iced_ok);
        if !iced_ok {
            return Ok(());
        }

        prop_assert_eq!(ours_len, iced_len);

        let mut ours_counts = [0u32; 5];
        let mut iced_counts = [0u32; 5];
        for (k, _) in &ours_ops {
            ours_counts[*k as usize] += 1;
        }
        for (k, _) in &iced_ops {
            iced_counts[*k as usize] += 1;
        }
        // Only validate the basic operand kinds (reg/mem/imm/rel). "Other" operands include far
        // pointers, special string-op memory kinds, etc, and are intentionally not compared here.
        for i in 0..4 {
            prop_assert!(ours_counts[i] >= iced_counts[i], "missing operand kind {i}: ours={:?} iced={:?}", ours_counts, iced_counts);
        }

        let mut ours_rels: Vec<u64> = ours_ops.iter().filter_map(|(_, t)| *t).collect();
        let mut iced_rels: Vec<u64> = iced_ops.iter().filter_map(|(_, t)| *t).collect();
        ours_rels.sort_unstable();
        iced_rels.sort_unstable();
        prop_assert_eq!(ours_rels, iced_rels);
    }
}
