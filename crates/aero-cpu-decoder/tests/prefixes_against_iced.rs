#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_decoder::{decode_one, DecodeMode, Segment, MAX_INSTRUCTION_LEN};
use iced_x86::{EncodingKind, MandatoryPrefix, OpCodeInfo, OpCodeTableKind, Register};

const CASES_PER_MODE: usize = 2_000;

/// Small deterministic PRNG (xorshift64*) to keep the test reproducible without
/// pulling in extra dependencies.
fn next_u64(state: &mut u64) -> u64 {
    // xorshift64*
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545F4914F6CDD1D)
}

fn gen_bytes(state: &mut u64) -> Vec<u8> {
    let len = (next_u64(state) as usize % MAX_INSTRUCTION_LEN) + 1;
    let mut bytes = vec![0u8; len];
    for b in &mut bytes {
        *b = next_u64(state) as u8;
    }
    bytes
}

fn expected_segment(mode: DecodeMode, iced: Register) -> Option<Segment> {
    // Iced reports the *encoded* segment prefix. In long mode, most segment
    // overrides are ignored by the CPU (only FS/GS are meaningful), so our
    // metadata intentionally drops ES/CS/SS/DS.
    match iced {
        Register::None => None,
        Register::FS => Some(Segment::Fs),
        Register::GS => Some(Segment::Gs),
        Register::ES => {
            if mode == DecodeMode::Bits64 {
                None
            } else {
                Some(Segment::Es)
            }
        }
        Register::CS => {
            if mode == DecodeMode::Bits64 {
                None
            } else {
                Some(Segment::Cs)
            }
        }
        Register::SS => {
            if mode == DecodeMode::Bits64 {
                None
            } else {
                Some(Segment::Ss)
            }
        }
        Register::DS => {
            if mode == DecodeMode::Bits64 {
                None
            } else {
                Some(Segment::Ds)
            }
        }
        other => panic!("unexpected iced segment register: {other:?}"),
    }
}

fn is_vex_evex_xop(encoding: EncodingKind) -> bool {
    matches!(encoding, EncodingKind::VEX | EncodingKind::EVEX | EncodingKind::XOP)
}

fn op_code_bytes(op_code: &OpCodeInfo) -> Option<Vec<u8>> {
    // This is best-effort and intentionally only supports the common legacy
    // opcode tables we expect to see in random decode fuzzing.
    let mut out = Vec::new();
    match op_code.table() {
        OpCodeTableKind::Normal => {}
        OpCodeTableKind::T0F => out.push(0x0F),
        OpCodeTableKind::T0F38 => out.extend([0x0F, 0x38]),
        OpCodeTableKind::T0F3A => out.extend([0x0F, 0x3A]),
        // VEX/XOP/EVEX tables etc. We don't try to reconstruct their opcode bytes
        // here since we only use this helper for legacy encodings.
        _ => return None,
    }

    let op = op_code.op_code();
    let len = op_code.op_code_len() as usize;
    if len == 0 || len > 4 {
        return None;
    }
    // `op_code.op_code()` stores the opcode bytes in the low bits, with the
    // most-significant byte first (big endian).
    for i in (0..len).rev() {
        out.push(((op >> (i * 8)) & 0xFF) as u8);
    }
    Some(out)
}

fn legacy_prefix_bytes<'a>(inst_bytes: &'a [u8], op_code: &OpCodeInfo) -> Option<&'a [u8]> {
    let op_bytes = op_code_bytes(op_code)?;
    if op_bytes.is_empty() || inst_bytes.len() < op_bytes.len() {
        return None;
    }
    for i in 0..=inst_bytes.len() - op_bytes.len() {
        if inst_bytes[i..i + op_bytes.len()] == op_bytes[..] {
            return Some(&inst_bytes[..i]);
        }
    }
    None
}

fn legacy_prefix_bytes_before_modern_prefix<'a>(
    inst_bytes: &'a [u8],
    encoding: EncodingKind,
) -> Option<&'a [u8]> {
    let lead = match encoding {
        EncodingKind::VEX => {
            // 2-byte VEX: C5, 3-byte VEX: C4
            return inst_bytes
                .iter()
                .position(|&b| b == 0xC4 || b == 0xC5)
                .map(|i| &inst_bytes[..i]);
        }
        EncodingKind::EVEX => 0x62,
        EncodingKind::XOP => 0x8F,
        _ => return None,
    };
    inst_bytes.iter().position(|&b| b == lead).map(|i| &inst_bytes[..i])
}

fn check_one(mode: DecodeMode, bytes: &[u8]) {
    let Ok(decoded) = decode_one(mode, 0x1234, bytes) else {
        return;
    };

    let ours = decoded.prefixes;
    let iced = decoded.instruction;
    let inst_bytes = &bytes[..iced.len()];
    let op_code = iced.op_code();
    let encoding = op_code.encoding();
    let mandatory = op_code.mandatory_prefix();

    // --- Group 1 prefix semantics (LOCK/REP/REPNE) ---
    // `Instruction::{has_rep_prefix,has_repne_prefix}` exclude *mandatory* `F3`/`F2`
    // prefix bytes (e.g. `PAUSE` and many SSE instructions). Since our prefix
    // parser is byte-based, we treat those mandatory bytes as present.
    //
    // We also avoid comparing HLE/XACQUIRE/XRELEASE cases since iced can report
    // multiple group1 flags simultaneously there, while our metadata enforces a
    // single "effective" group1 prefix ("last prefix wins").
    let expected_lock = iced.has_lock_prefix();
    let expected_rep = iced.has_rep_prefix() || mandatory == MandatoryPrefix::PF3;
    let expected_repne = iced.has_repne_prefix() || mandatory == MandatoryPrefix::PF2;
    let expected_group1_count = expected_lock as u8 + expected_rep as u8 + expected_repne as u8;
    if expected_group1_count <= 1 {
        assert_eq!(
            ours.lock, expected_lock,
            "LOCK mismatch: mode={mode:?} bytes={bytes:02X?} mandatory={mandatory:?} inst={iced:?}",
        );
        assert_eq!(
            ours.rep, expected_rep,
            "REP mismatch: mode={mode:?} bytes={bytes:02X?} mandatory={mandatory:?} inst={iced:?}",
        );
        assert_eq!(
            ours.repne, expected_repne,
            "REPNE mismatch: mode={mode:?} bytes={bytes:02X?} mandatory={mandatory:?} inst={iced:?}",
        );
    }
    let group1_count = ours.lock as u8 + ours.rep as u8 + ours.repne as u8;
    assert!(
        group1_count <= 1,
        "group1 mutual exclusion violated: mode={mode:?} bytes={bytes:02X?} prefixes={ours:?} inst={iced:?}"
    );

    // --- Operand/address size overrides ---
    // Our metadata is based on the raw legacy prefix bytes. Iced doesn't expose
    // "HAS66/HAS67" directly in the `Instruction` API, so we derive whether the
    // *decoded instruction bytes* include `66` / `67` in the legacy prefix
    // region using opcode metadata.
    //
    // For VEX/EVEX/XOP, the mandatory-prefix byte (66/F2/F3) is encoded inside
    // the VEX/EVEX/XOP prefix, so we only compare operand-size override for
    // non-VEX/EVEX/XOP encodings.
    if !is_vex_evex_xop(encoding) && encoding == EncodingKind::Legacy {
        if let Some(pfx) = legacy_prefix_bytes(inst_bytes, op_code) {
            assert_eq!(
                ours.operand_size_override,
                pfx.contains(&0x66),
                "operand-size override mismatch: mode={mode:?} bytes={bytes:02X?} prefixes={ours:?} inst={iced:?}",
            );
        }
    }

    match encoding {
        EncodingKind::Legacy => {
            if let Some(pfx) = legacy_prefix_bytes(inst_bytes, op_code) {
                assert_eq!(
                    ours.address_size_override,
                    pfx.contains(&0x67),
                    "address-size override mismatch: mode={mode:?} bytes={bytes:02X?} prefixes={ours:?} inst={iced:?}",
                );
            }
        }
        _ if is_vex_evex_xop(encoding) => {
            if let Some(pfx) = legacy_prefix_bytes_before_modern_prefix(inst_bytes, encoding) {
                assert_eq!(
                    ours.address_size_override,
                    pfx.contains(&0x67),
                    "address-size override mismatch: mode={mode:?} bytes={bytes:02X?} prefixes={ours:?} inst={iced:?}",
                );
            }
        }
        _ => {
            // Unknown/unsupported encoding kind; skip address-size comparison.
        }
    }

    // --- Segment override semantics ---
    let iced_seg = iced.segment_prefix();
    let expected = expected_segment(mode, iced_seg);
    assert_eq!(
        ours.segment,
        expected,
        "segment override mismatch: mode={mode:?} bytes={bytes:02X?} iced_segment={iced_seg:?} prefixes={ours:?} inst={iced:?}",
    );
}

#[test]
fn prefixes_match_iced_for_valid_decodes() {
    for (mode, seed) in [
        (DecodeMode::Bits16, 0xF00D_F00D_0123_4567u64),
        (DecodeMode::Bits32, 0xDEAD_BEEF_0123_4567u64),
        (DecodeMode::Bits64, 0x0123_4567_89AB_CDEFu64),
    ] {
        let mut state = seed;
        for _ in 0..CASES_PER_MODE {
            let bytes = gen_bytes(&mut state);
            check_one(mode, &bytes);
        }
    }
}
