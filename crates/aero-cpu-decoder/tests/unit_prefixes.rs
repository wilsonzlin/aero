use aero_cpu_decoder::{decode_one, DecodeError, DecodeMode, Segment, MAX_INSTRUCTION_LEN};

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
