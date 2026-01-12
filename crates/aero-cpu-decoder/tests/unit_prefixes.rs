use aero_cpu_decoder::{decode_one, DecodeError, DecodeMode, Segment, MAX_INSTRUCTION_LEN};
use iced_x86::Mnemonic;

#[test]
fn parses_basic_legacy_prefixes() {
    // lock add dword ptr [eax], 1
    let bytes = [0xF0, 0x83, 0x00, 0x01];
    let decoded = decode_one(DecodeMode::Bits32, 0, &bytes).expect("decode");
    assert!(decoded.prefixes.lock);
    assert!(!decoded.prefixes.rep);
    assert!(!decoded.prefixes.repne);
}

#[test]
fn parses_segment_and_size_prefixes() {
    // 64 66 67 8B 04 25 00 00 00 00
    // FS + operand-size override + address-size override + MOV AX, [0]
    let bytes = [0x64, 0x66, 0x67, 0x8B, 0x04, 0x25, 0, 0, 0, 0];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert_eq!(decoded.prefixes.segment, Some(Segment::Fs));
    assert!(decoded.prefixes.operand_size_override);
    assert!(decoded.prefixes.address_size_override);
}

#[test]
fn parses_rex_prefix_in_64bit_mode() {
    // 4C 8B D0  => mov r10, rax (REX.WRXB=0100_1100)
    let bytes = [0x4C, 0x8B, 0xD0];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    let rex = decoded.prefixes.rex.expect("rex");
    assert!(rex.w());
    assert!(rex.r());
    assert!(!rex.x());
    assert!(!rex.b());
}

#[test]
fn parses_vex2_prefix() {
    // C5 F8 77 => vzeroupper
    let bytes = [0xC5, 0xF8, 0x77];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert!(decoded.prefixes.vex.is_some());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Vzeroupper);
}

#[test]
fn parses_evex_prefix() {
    // 62 F1 7C 48 58 C0 => vaddps zmm0, zmm0, zmm0
    //
    // (The `0x62` lead byte is ambiguous with `BOUND` in non-64-bit modes, so
    // this test also guards our EVEX-vs-BOUND disambiguation logic.)
    let bytes = [0x62, 0xF1, 0x7C, 0x48, 0x58, 0xC0];
    let decoded = decode_one(DecodeMode::Bits32, 0, &bytes).expect("decode");
    assert!(decoded.prefixes.evex.is_some());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Vaddps);
}

#[test]
fn parses_xop_prefix() {
    // 8F A9 A8 90 C0 => vprotb xmm0, xmm10, xmm0
    //
    // XOP shares its lead byte (`0x8F`) with the legacy `POP r/m` encoding, so
    // this is a positive test for the POP-vs-XOP disambiguation rule.
    let bytes = [0x8F, 0xA9, 0xA8, 0x90, 0xC0];
    let decoded = decode_one(DecodeMode::Bits16, 0, &bytes).expect("decode");
    assert!(decoded.prefixes.xop.is_some());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Vprotb);
}

#[test]
fn does_not_misdetect_pop_as_xop() {
    // 8F 00 => pop qword ptr [rax]
    let bytes = [0x8F, 0x00];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert!(decoded.prefixes.xop.is_none());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Pop);
}

#[test]
fn does_not_misdetect_bound_as_evex_in_32bit_mode() {
    // 62 00 => bound eax, [eax]  (valid in 32-bit mode; 0x62 is not EVEX here)
    let bytes = [0x62, 0x00];
    let decoded = decode_one(DecodeMode::Bits32, 0, &bytes).expect("decode");
    assert!(decoded.prefixes.evex.is_none());
    assert_eq!(decoded.instruction.mnemonic(), Mnemonic::Bound);
}

#[test]
fn rejects_empty_input() {
    assert_eq!(
        decode_one(DecodeMode::Bits64, 0, &[]).unwrap_err(),
        DecodeError::EmptyInput
    );
}

#[test]
fn never_returns_length_over_15() {
    let bytes = [0x90u8; MAX_INSTRUCTION_LEN];
    let decoded = decode_one(DecodeMode::Bits64, 0, &bytes).expect("decode");
    assert!(decoded.len() as usize <= MAX_INSTRUCTION_LEN);
}
