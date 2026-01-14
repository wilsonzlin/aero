use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_set_shader_constants_b_payload_le, decode_cmd_set_shader_constants_i_payload_le,
    AerogpuCmdDecodeError, AerogpuCmdOpcode,
};

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn push_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn build_packet(opcode: AerogpuCmdOpcode, payload: Vec<u8>) -> Vec<u8> {
    let size_bytes = (8 + payload.len()) as u32;
    assert!(size_bytes.is_multiple_of(4));

    let mut packet = Vec::new();
    push_u32(&mut packet, opcode as u32);
    push_u32(&mut packet, size_bytes);
    packet.extend_from_slice(&payload);
    packet
}

#[test]
fn decode_set_shader_constants_i_decodes_payload_and_allows_trailing_bytes() {
    // vec4_count=2 => i32_count=8
    let stage = 2u32;
    let start_register = 7u32;
    let vec4_count = 2u32;
    let reserved0 = 3u32;
    let values: [i32; 8] = [1, -2, 3, -4, 5, -6, 7, -8];

    let mut payload = Vec::new();
    push_u32(&mut payload, stage);
    push_u32(&mut payload, start_register);
    push_u32(&mut payload, vec4_count);
    push_u32(&mut payload, reserved0);
    for v in values {
        push_i32(&mut payload, v);
    }
    // Forward-compat: append unknown trailing bytes.
    push_u32(&mut payload, 0xDEAD_BEEFu32);

    let packet = build_packet(AerogpuCmdOpcode::SetShaderConstantsI, payload);
    let (cmd, data) = decode_cmd_set_shader_constants_i_payload_le(&packet).unwrap();
    let cmd_opcode = cmd.hdr.opcode;
    let cmd_stage = cmd.stage;
    let cmd_start_register = cmd.start_register;
    let cmd_vec4_count = cmd.vec4_count;
    let cmd_reserved0 = cmd.reserved0;
    assert_eq!(cmd_opcode, AerogpuCmdOpcode::SetShaderConstantsI as u32);
    assert_eq!(cmd_stage, stage);
    assert_eq!(cmd_start_register, start_register);
    assert_eq!(cmd_vec4_count, vec4_count);
    assert_eq!(cmd_reserved0, reserved0);
    assert_eq!(data, values.to_vec());
}

#[test]
fn decode_set_shader_constants_i_rejects_wrong_opcode() {
    // Minimal well-formed header/payload for the I decoder, but opcode is different.
    let mut payload = Vec::new();
    push_u32(&mut payload, 2); // stage
    push_u32(&mut payload, 0); // start_register
    push_u32(&mut payload, 0); // vec4_count
    push_u32(&mut payload, 0); // reserved0

    let packet = build_packet(AerogpuCmdOpcode::SetShaderConstantsB, payload);
    match decode_cmd_set_shader_constants_i_payload_le(&packet) {
        Ok(_) => panic!("expected decode_cmd_set_shader_constants_i_payload_le to fail"),
        Err(err) => assert!(matches!(
            err,
            AerogpuCmdDecodeError::UnexpectedOpcode {
                found,
                expected: AerogpuCmdOpcode::SetShaderConstantsI
            } if found == AerogpuCmdOpcode::SetShaderConstantsB as u32
        )),
    }
}

#[test]
fn decode_set_shader_constants_b_decodes_payload_and_allows_trailing_bytes() {
    // bool_count=2 => 2 scalar u32 values (0/1).
    let stage = 0u32;
    let start_register = 3u32;
    let bool_count = 2u32;
    let reserved0 = 0u32;
    let values: [u32; 2] = [0, 1];

    let mut payload = Vec::new();
    push_u32(&mut payload, stage);
    push_u32(&mut payload, start_register);
    push_u32(&mut payload, bool_count);
    push_u32(&mut payload, reserved0);
    for v in values {
        push_u32(&mut payload, v);
    }
    // Forward-compat: append unknown trailing bytes.
    push_u32(&mut payload, 0xC0FF_EE00u32);

    let packet = build_packet(AerogpuCmdOpcode::SetShaderConstantsB, payload);
    let (cmd, data) = decode_cmd_set_shader_constants_b_payload_le(&packet).unwrap();
    let cmd_opcode = cmd.hdr.opcode;
    let cmd_stage = cmd.stage;
    let cmd_start_register = cmd.start_register;
    let cmd_bool_count = cmd.bool_count;
    let cmd_reserved0 = cmd.reserved0;
    assert_eq!(cmd_opcode, AerogpuCmdOpcode::SetShaderConstantsB as u32);
    assert_eq!(cmd_stage, stage);
    assert_eq!(cmd_start_register, start_register);
    assert_eq!(cmd_bool_count, bool_count);
    assert_eq!(cmd_reserved0, reserved0);
    assert_eq!(data, values.to_vec());
}

#[test]
fn decode_set_shader_constants_b_rejects_wrong_opcode() {
    let mut payload = Vec::new();
    push_u32(&mut payload, 0); // stage
    push_u32(&mut payload, 0); // start_register
    push_u32(&mut payload, 0); // bool_count
    push_u32(&mut payload, 0); // reserved0

    let packet = build_packet(AerogpuCmdOpcode::SetShaderConstantsI, payload);
    match decode_cmd_set_shader_constants_b_payload_le(&packet) {
        Ok(_) => panic!("expected decode_cmd_set_shader_constants_b_payload_le to fail"),
        Err(err) => assert!(matches!(
            err,
            AerogpuCmdDecodeError::UnexpectedOpcode {
                found,
                expected: AerogpuCmdOpcode::SetShaderConstantsB
            } if found == AerogpuCmdOpcode::SetShaderConstantsI as u32
        )),
    }
}
