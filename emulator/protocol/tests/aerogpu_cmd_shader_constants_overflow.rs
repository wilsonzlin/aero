use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_set_shader_constants_f_payload_le, decode_cmd_set_shader_constants_i_payload_le,
    AerogpuCmdDecodeError, AerogpuCmdOpcode, AerogpuCmdSetShaderConstantsF,
    AerogpuCmdSetShaderConstantsI,
};

fn build_set_shader_constants_packet(opcode: AerogpuCmdOpcode, vec4_count: u32) -> Vec<u8> {
    let size_bytes = AerogpuCmdSetShaderConstantsF::SIZE_BYTES as u32;
    debug_assert_eq!(
        AerogpuCmdSetShaderConstantsF::SIZE_BYTES,
        AerogpuCmdSetShaderConstantsI::SIZE_BYTES,
        "test assumes SET_SHADER_CONSTANTS_F/I have the same base header size"
    );

    let mut buf = Vec::with_capacity(AerogpuCmdSetShaderConstantsF::SIZE_BYTES);
    buf.extend_from_slice(&(opcode as u32).to_le_bytes());
    buf.extend_from_slice(&size_bytes.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // stage
    buf.extend_from_slice(&0u32.to_le_bytes()); // start_register
    buf.extend_from_slice(&vec4_count.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    debug_assert_eq!(buf.len(), AerogpuCmdSetShaderConstantsF::SIZE_BYTES);
    buf
}

#[test]
fn set_shader_constants_f_vec4_count_overflow_returns_count_overflow() {
    let buf = build_set_shader_constants_packet(AerogpuCmdOpcode::SetShaderConstantsF, u32::MAX);
    let err = match decode_cmd_set_shader_constants_f_payload_le(&buf) {
        Ok(_) => panic!("expected decode to fail with CountOverflow"),
        Err(err) => err,
    };
    assert!(matches!(err, AerogpuCmdDecodeError::CountOverflow));
}

#[test]
fn set_shader_constants_i_vec4_count_overflow_returns_count_overflow() {
    let buf = build_set_shader_constants_packet(AerogpuCmdOpcode::SetShaderConstantsI, u32::MAX);
    let err = match decode_cmd_set_shader_constants_i_payload_le(&buf) {
        Ok(_) => panic!("expected decode to fail with CountOverflow"),
        Err(err) => err,
    };
    assert!(matches!(err, AerogpuCmdDecodeError::CountOverflow));
}
