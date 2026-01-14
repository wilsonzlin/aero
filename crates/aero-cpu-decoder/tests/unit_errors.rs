use aero_cpu_decoder::{decode_instruction, DecodeError, DecodeMode, MAX_INSTRUCTION_LEN};

#[test]
fn no_more_bytes_with_full_15_byte_window_is_invalid_instruction() {
    // A stream of only prefix bytes can never form a valid instruction because x86 instructions
    // must include at least one opcode byte within the architectural 15-byte maximum length.
    //
    // When the caller provides a full 15-byte decode window, "need more bytes" implies the
    // instruction would exceed the length limit and is therefore invalid (not truncated input).
    let bytes = [0x66u8; MAX_INSTRUCTION_LEN];
    assert_eq!(
        decode_instruction(DecodeMode::Bits64, 0, &bytes).unwrap_err(),
        DecodeError::InvalidInstruction
    );
}
