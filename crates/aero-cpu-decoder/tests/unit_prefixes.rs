use aero_cpu_decoder::{
    decode_one, decode_prefixes, scan_prefixes, DecodeError, DecodeMode, Segment,
    MAX_INSTRUCTION_LEN,
};
use iced_x86::{EncodingKind, MandatoryPrefix, Mnemonic};

fn assert_prefix_api_matches_decode_one(mode: DecodeMode, bytes: &[u8]) {
    let decoded = decode_one(mode, 0, bytes).expect("decode_one");

    let prefixes_only = decode_prefixes(mode, bytes).expect("decode_prefixes");
    assert_eq!(prefixes_only, decoded.prefixes);

    let (prefixes, _consumed) = scan_prefixes(mode, bytes).expect("scan_prefixes");
    assert_eq!(prefixes, decoded.prefixes);
}

#[test]
fn reports_expected_consumed_prefix_lengths() {
    // no prefix
    assert_eq!(scan_prefixes(DecodeMode::Bits64, &[0x90]).unwrap().1, 0);
    // 66
    assert_eq!(
        scan_prefixes(DecodeMode::Bits64, &[0x66, 0x90]).unwrap().1,
        1
    );
    // 66 67
    assert_eq!(
        scan_prefixes(DecodeMode::Bits64, &[0x66, 0x67, 0x90])
            .unwrap()
            .1,
        2
    );
    // REX
    assert_eq!(
        scan_prefixes(DecodeMode::Bits64, &[0x48, 0x90]).unwrap().1,
        1
    );
}

#[test]
fn parses_basic_legacy_prefixes() {
    // lock add dword ptr [eax], 1
    let bytes = [0xF0, 0x83, 0x00, 0x01];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits32, &bytes);

    let decoded = decode_one(DecodeMode::Bits32, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.lock);
    assert!(!decoded.prefixes.rep);
    assert!(!decoded.prefixes.repne);

    let (_p, consumed) = scan_prefixes(DecodeMode::Bits32, &bytes).expect("scan_prefixes");
    assert_eq!(consumed, 1);
}

#[test]
fn parses_segment_and_size_prefixes() {
    // 64 66 67 8B 04 25 00 00 00 00
    // FS + operand-size override + address-size override + MOV AX, [0]
    let bytes = [0x64, 0x66, 0x67, 0x8B, 0x04, 0x25, 0, 0, 0, 0];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits64, &bytes);

    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode_one");
    assert_eq!(decoded.prefixes.segment, Some(Segment::Fs));
    assert!(decoded.prefixes.operand_size_override);
    assert!(decoded.prefixes.address_size_override);

    let (_p, consumed) = scan_prefixes(DecodeMode::Bits64, &bytes).expect("scan_prefixes");
    assert_eq!(consumed, 3);
}

#[test]
fn parses_gs_override_prefix() {
    // GS + NOP
    let bytes = [0x65, 0x90];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits64, &bytes);

    let (p, consumed) = scan_prefixes(DecodeMode::Bits64, &bytes).expect("scan_prefixes");
    assert_eq!(p.segment, Some(Segment::Gs));
    assert_eq!(consumed, 1);
}

#[test]
fn ignores_ds_segment_override_in_64bit_mode_without_clobbering_fs() {
    // 64 3E 8B 00
    // FS override + (ignored) DS override + MOV EAX, [RAX]
    let bytes = [0x64, 0x3E, 0x8B, 0x00];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert_eq!(decoded.prefixes.segment, Some(Segment::Fs));
}

#[test]
fn ignores_ds_segment_override_in_64bit_mode() {
    // 3E 8B 00
    // (ignored) DS override + MOV EAX, [RAX]
    let bytes = [0x3E, 0x8B, 0x00];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert_eq!(decoded.prefixes.segment, None);
}

#[test]
fn group1_prefix_last_wins_lock_vs_rep() {
    // 01 00 => add dword ptr [rax], eax
    // LOCK; REP; <opcode> => REP wins
    let bytes = [0xF0, 0xF3, 0x01, 0x00];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert!(!decoded.prefixes.lock);
    assert!(decoded.prefixes.rep);
    assert!(!decoded.prefixes.repne);

    // REP; LOCK; <opcode> => LOCK wins
    let bytes = [0xF3, 0xF0, 0x01, 0x00];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert!(decoded.prefixes.lock);
    assert!(!decoded.prefixes.rep);
    assert!(!decoded.prefixes.repne);
}

#[test]
fn parses_rex_prefix_in_64bit_mode() {
    // 4C 8B D0  => mov r10, rax (REX.WRXB=0100_1100)
    let bytes = [0x4C, 0x8B, 0xD0];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits64, &bytes);

    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode_one");
    let rex = decoded.prefixes.rex.expect("rex");
    assert!(rex.w());
    assert!(rex.r());
    assert!(!rex.x());
    assert!(!rex.b());

    let (_p, consumed) = scan_prefixes(DecodeMode::Bits64, &bytes).expect("scan_prefixes");
    assert_eq!(consumed, 1);
}

#[test]
fn parses_vex2_prefix() {
    // C5 F8 77 => vzeroupper
    let bytes = [0xC5, 0xF8, 0x77];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits64, &bytes);

    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.vex.is_some());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Vzeroupper);

    let (_p, consumed) = scan_prefixes(DecodeMode::Bits64, &bytes).expect("scan_prefixes");
    assert_eq!(consumed, 2);
}

#[test]
fn parses_vex3_prefix() {
    // C4 E2 7D 58 C0 => vpbroadcastd ymm0, xmm0
    let bytes = [0xC4, 0xE2, 0x7D, 0x58, 0xC0];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits64, &bytes);

    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.vex.is_some());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Vpbroadcastd);

    let (p, consumed) = scan_prefixes(DecodeMode::Bits64, &bytes).expect("scan_prefixes");
    assert!(p.vex.is_some());
    assert_eq!(consumed, 3);
}

#[test]
fn parses_evex_prefix() {
    // 62 F1 7C 48 58 C0 => vaddps zmm0, zmm0, zmm0
    //
    // (The `0x62` lead byte is ambiguous with `BOUND` in non-64-bit modes, so
    // this test also guards our EVEX-vs-BOUND disambiguation logic.)
    let bytes = [0x62, 0xF1, 0x7C, 0x48, 0x58, 0xC0];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits32, &bytes);

    let decoded = decode_one(DecodeMode::Bits32, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.evex.is_some());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Vaddps);

    let (_p, consumed) = scan_prefixes(DecodeMode::Bits32, &bytes).expect("scan_prefixes");
    assert_eq!(consumed, 4);
}

#[test]
fn parses_xop_prefix() {
    // 8F A9 A8 90 C0 => vprotb xmm0, xmm10, xmm0
    //
    // XOP shares its lead byte (`0x8F`) with the legacy `POP r/m` encoding, so
    // this is a positive test for the POP-vs-XOP disambiguation rule.
    let bytes = [0x8F, 0xA9, 0xA8, 0x90, 0xC0];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits16, &bytes);

    let decoded = decode_one(DecodeMode::Bits16, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.xop.is_some());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Vprotb);

    let (_p, consumed) = scan_prefixes(DecodeMode::Bits16, &bytes).expect("scan_prefixes");
    assert_eq!(consumed, 3);
}

#[test]
fn does_not_misdetect_pop_as_xop() {
    // 8F 00 => pop qword ptr [rax]
    let bytes = [0x8F, 0x00];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits64, &bytes);

    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.xop.is_none());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Pop);

    let (p, consumed) = scan_prefixes(DecodeMode::Bits64, &bytes).expect("scan_prefixes");
    assert!(p.xop.is_none());
    assert_eq!(consumed, 0);
}

#[test]
fn does_not_misdetect_bound_as_evex_in_32bit_mode() {
    // 62 00 => bound eax, [eax]  (valid in 32-bit mode; 0x62 is not EVEX here)
    let bytes = [0x62, 0x00];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits32, &bytes);

    let decoded = decode_one(DecodeMode::Bits32, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.evex.is_none());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Bound);

    let (p, consumed) = scan_prefixes(DecodeMode::Bits32, &bytes).expect("scan_prefixes");
    assert!(p.evex.is_none());
    assert_eq!(consumed, 0);
}

#[test]
fn does_not_misdetect_lds_as_vex_in_32bit_mode() {
    // C5 00 => lds eax, [eax] (valid in 32-bit mode; 0xC5 is not VEX here)
    let bytes = [0xC5, 0x00];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits32, &bytes);

    let decoded = decode_one(DecodeMode::Bits32, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.vex.is_none());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Lds);

    let (p, consumed) = scan_prefixes(DecodeMode::Bits32, &bytes).expect("scan_prefixes");
    assert!(p.vex.is_none());
    assert_eq!(consumed, 0);
}

#[test]
fn does_not_misdetect_les_as_vex_in_32bit_mode() {
    // C4 00 => les eax, [eax] (valid in 32-bit mode; 0xC4 is not VEX here)
    let bytes = [0xC4, 0x00];
    assert_prefix_api_matches_decode_one(DecodeMode::Bits32, &bytes);

    let decoded = decode_one(DecodeMode::Bits32, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.vex.is_none());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Les);

    let (p, consumed) = scan_prefixes(DecodeMode::Bits32, &bytes).expect("scan_prefixes");
    assert!(p.vex.is_none());
    assert_eq!(consumed, 0);
}

#[test]
fn reports_truncated_multibyte_prefixes_as_eof() {
    assert_eq!(
        scan_prefixes(DecodeMode::Bits64, &[0xC5]).unwrap_err(),
        DecodeError::UnexpectedEof
    );
    assert_eq!(
        scan_prefixes(DecodeMode::Bits64, &[0xC4, 0xE2]).unwrap_err(),
        DecodeError::UnexpectedEof
    );
    assert_eq!(
        scan_prefixes(DecodeMode::Bits64, &[0x62, 0xF1, 0x7C]).unwrap_err(),
        DecodeError::UnexpectedEof
    );
    assert_eq!(
        scan_prefixes(DecodeMode::Bits16, &[0x8F, 0xA9]).unwrap_err(),
        DecodeError::UnexpectedEof
    );
}

#[test]
fn rejects_empty_input() {
    assert_eq!(
        decode_one(DecodeMode::Bits64, 0, &[]).unwrap_err(),
        DecodeError::EmptyInput
    );

    assert_eq!(
        decode_prefixes(DecodeMode::Bits64, &[]).unwrap_err(),
        DecodeError::EmptyInput
    );
    assert_eq!(
        scan_prefixes(DecodeMode::Bits64, &[]).unwrap_err(),
        DecodeError::EmptyInput
    );
}

#[test]
fn never_returns_length_over_15() {
    let bytes = [0x90u8; MAX_INSTRUCTION_LEN];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert!(decoded.len() as usize <= MAX_INSTRUCTION_LEN);

    // Also ensure prefix scanning honors the same architectural cap.
    let bytes = [0x66u8; MAX_INSTRUCTION_LEN + 4];
    let (_p, consumed) = scan_prefixes(DecodeMode::Bits64, &bytes).expect("scan_prefixes");
    assert!(consumed <= MAX_INSTRUCTION_LEN);
}

#[test]
fn mandatory_f2_f3_prefixes_are_byte_based_for_prefix_metadata() {
    // iced-x86 distinguishes between *explicit* REP/REPNE prefix bytes and mandatory
    // PF2/PF3 prefixes (e.g. PAUSE / MOVSS / MOVSD). Our `Prefixes` metadata is
    // byte-based, so it reports explicit F2/F3 bytes as REPNE/REP regardless of
    // whether the opcode treats them as "mandatory".

    // Legacy PAUSE: F3 is an actual prefix byte, but iced does not report it via
    // `has_rep_prefix()` since it's mandatory for this opcode.
    let bytes = [0xF3, 0x90];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode_one");
    assert!(decoded.prefixes.rep);
    assert!(!decoded.prefixes.repne);
    assert!(!decoded.instruction.has_rep_prefix());
    assert_eq!(
        decoded.instruction.op_code().mandatory_prefix(),
        MandatoryPrefix::PF3
    );
    assert_eq!(decoded.instruction.op_code().encoding(), EncodingKind::Legacy);

    // VEX VMOVSS: mandatory PF3 is encoded in the VEX prefix `pp` field, so there
    // is no F3 prefix byte in the instruction stream and `Prefixes::rep` stays
    // false.
    let bytes = [0xC5, 0xFA, 0x10, 0xC0];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode_one");
    assert!(!decoded.prefixes.rep);
    assert!(!decoded.prefixes.repne);
    assert!(!decoded.instruction.has_rep_prefix());
    assert_eq!(
        decoded.instruction.op_code().mandatory_prefix(),
        MandatoryPrefix::PF3
    );
    assert_eq!(decoded.instruction.op_code().encoding(), EncodingKind::VEX);
}
