use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_bind_shaders_payload_le, AerogpuCmdOpcode, BindShadersEx,
};

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn build_packet(opcode: u32, payload: Vec<u8>) -> Vec<u8> {
    assert!(payload.len().is_multiple_of(4));
    let size_bytes = (8 + payload.len()) as u32;
    assert!(size_bytes.is_multiple_of(4));

    let mut packet = Vec::new();
    push_u32(&mut packet, opcode);
    push_u32(&mut packet, size_bytes);
    packet.extend_from_slice(&payload);
    packet
}

#[test]
fn bind_shaders_decodes_base_packet() {
    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // vs
    push_u32(&mut payload, 2); // ps
    push_u32(&mut payload, 3); // cs
    push_u32(&mut payload, 0xAABB_CCDD); // reserved0

    let packet = build_packet(AerogpuCmdOpcode::BindShaders as u32, payload);

    let (cmd, ex) = decode_cmd_bind_shaders_payload_le(&packet).unwrap();
    // Avoid creating references to packed fields in `assert_eq!`.
    let opcode = cmd.hdr.opcode;
    let size_bytes = cmd.hdr.size_bytes;
    let vs = cmd.vs;
    let ps = cmd.ps;
    let cs = cmd.cs;
    let reserved0 = cmd.reserved0;
    assert_eq!(opcode, AerogpuCmdOpcode::BindShaders as u32);
    assert_eq!(size_bytes, 24);
    assert_eq!(vs, 1);
    assert_eq!(ps, 2);
    assert_eq!(cs, 3);
    assert_eq!(reserved0, 0xAABB_CCDD);
    assert_eq!(ex, None);
}

#[test]
fn bind_shaders_decodes_extended_packet() {
    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // vs
    push_u32(&mut payload, 2); // ps
    push_u32(&mut payload, 3); // cs
    push_u32(&mut payload, 0); // reserved0
    push_u32(&mut payload, 4); // gs
    push_u32(&mut payload, 5); // hs
    push_u32(&mut payload, 6); // ds

    let packet = build_packet(AerogpuCmdOpcode::BindShaders as u32, payload);

    let (cmd, ex) = decode_cmd_bind_shaders_payload_le(&packet).unwrap();
    let size_bytes = cmd.hdr.size_bytes;
    assert_eq!(size_bytes, 36);
    assert_eq!(
        ex,
        Some(BindShadersEx {
            gs: 4,
            hs: 5,
            ds: 6
        })
    );
}

#[test]
fn bind_shaders_extended_packet_allows_trailing_bytes() {
    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // vs
    push_u32(&mut payload, 2); // ps
    push_u32(&mut payload, 3); // cs
    push_u32(&mut payload, 0); // reserved0
    push_u32(&mut payload, 4); // gs
    push_u32(&mut payload, 5); // hs
    push_u32(&mut payload, 6); // ds
    push_u32(&mut payload, 0xDEAD_BEEF); // trailing extension (ignored)

    let packet = build_packet(AerogpuCmdOpcode::BindShaders as u32, payload);

    let (cmd, ex) = decode_cmd_bind_shaders_payload_le(&packet).unwrap();
    let size_bytes = cmd.hdr.size_bytes;
    assert_eq!(size_bytes, 40);
    assert_eq!(
        ex,
        Some(BindShadersEx {
            gs: 4,
            hs: 5,
            ds: 6
        })
    );
}

#[test]
fn bind_shaders_base_packet_allows_trailing_bytes() {
    // Forward-compat: if the packet grows by appending new fields, older decoders should accept
    // the packet as long as the base payload is present. In this case we do not have enough bytes
    // to decode the `{gs, hs, ds}` extension (12 bytes), so `ex` remains `None`.
    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // vs
    push_u32(&mut payload, 2); // ps
    push_u32(&mut payload, 3); // cs
    push_u32(&mut payload, 0xAABB_CCDD); // reserved0
    push_u32(&mut payload, 0xDEAD_BEEF); // trailing extension (ignored)

    let packet = build_packet(AerogpuCmdOpcode::BindShaders as u32, payload);

    let (cmd, ex) = decode_cmd_bind_shaders_payload_le(&packet).unwrap();
    let size_bytes = cmd.hdr.size_bytes;
    let reserved0 = cmd.reserved0;
    assert_eq!(size_bytes, 28);
    assert_eq!(reserved0, 0xAABB_CCDD);
    assert_eq!(ex, None);
}
