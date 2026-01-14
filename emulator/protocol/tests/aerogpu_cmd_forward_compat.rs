use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdBindShaders, AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuCmdStreamIter,
    BindShadersEx, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn build_bind_shaders_stream_with_reserved0(
    extended: bool,
    with_extra_trailing: bool,
    reserved0: u32,
) -> Vec<u8> {
    let mut bytes = Vec::new();

    // Stream header.
    push_u32(&mut bytes, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut bytes, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut bytes, 0); // size_bytes (patched later)
    push_u32(&mut bytes, 0); // flags
    push_u32(&mut bytes, 0); // reserved0
    push_u32(&mut bytes, 0); // reserved1

    // BIND_SHADERS packet.
    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // vs
    push_u32(&mut payload, 2); // ps
    push_u32(&mut payload, 3); // cs
    push_u32(&mut payload, reserved0); // reserved0 (legacy GS handle)
    if extended {
        // Append-only extension: gs/hs/ds handles.
        push_u32(&mut payload, 4); // gs
        push_u32(&mut payload, 5); // hs
        push_u32(&mut payload, 6); // ds
    }
    if with_extra_trailing {
        // Forward-compatible extension beyond known fields (ignored by current decoders).
        push_u32(&mut payload, 0xDEAD_BEEF);
    }

    let size_bytes = (8 + payload.len()) as u32;
    assert!(size_bytes.is_multiple_of(4));
    push_u32(&mut bytes, AerogpuCmdOpcode::BindShaders as u32);
    push_u32(&mut bytes, size_bytes);
    bytes.extend_from_slice(&payload);

    // Patch stream header size_bytes.
    let stream_size_bytes = bytes.len() as u32;
    bytes[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&stream_size_bytes.to_le_bytes());

    bytes
}

fn build_bind_shaders_stream(extended: bool, with_extra_trailing: bool) -> Vec<u8> {
    build_bind_shaders_stream_with_reserved0(extended, with_extra_trailing, 0)
}

#[test]
fn cmd_stream_accepts_extended_bind_shaders_packet() {
    let base = build_bind_shaders_stream(false, false);
    let base_with_trailing = build_bind_shaders_stream(false, true);
    let extended = build_bind_shaders_stream(true, false);
    let extended_with_trailing = build_bind_shaders_stream(true, true);

    let decode = |stream: &[u8]| {
        let iter = AerogpuCmdStreamIter::new(stream).unwrap();
        assert_eq!(iter.header().size_bytes as usize, stream.len());

        let packets = iter.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].opcode, Some(AerogpuCmdOpcode::BindShaders));
        packets[0].decode_bind_shaders_payload_le().unwrap()
    };

    let (base_cmd, base_ex) = decode(&base);
    let base_size_bytes = base_cmd.hdr.size_bytes;
    let (base_vs, base_ps, base_cs) = (base_cmd.vs, base_cmd.ps, base_cmd.cs);
    assert_eq!(base_size_bytes as usize, AerogpuCmdBindShaders::SIZE_BYTES);
    assert_eq!((base_vs, base_ps, base_cs), (1, 2, 3));
    assert_eq!(base_ex, None);

    let (base_trailing_cmd, base_trailing_ex) = decode(&base_with_trailing);
    let base_trailing_size_bytes = base_trailing_cmd.hdr.size_bytes;
    let (base_trailing_vs, base_trailing_ps, base_trailing_cs) = (
        base_trailing_cmd.vs,
        base_trailing_cmd.ps,
        base_trailing_cmd.cs,
    );
    assert_eq!(
        base_trailing_size_bytes as usize,
        AerogpuCmdBindShaders::SIZE_BYTES + 4
    );
    assert_eq!(
        (base_trailing_vs, base_trailing_ps, base_trailing_cs),
        (1, 2, 3)
    );
    assert_eq!(base_trailing_ex, None);

    let (ext_cmd, ext_ex) = decode(&extended);
    let ext_size_bytes = ext_cmd.hdr.size_bytes;
    let (ext_vs, ext_ps, ext_cs) = (ext_cmd.vs, ext_cmd.ps, ext_cmd.cs);
    assert_eq!(
        ext_size_bytes as usize,
        AerogpuCmdBindShaders::EX_SIZE_BYTES
    );
    assert_eq!((ext_vs, ext_ps, ext_cs), (1, 2, 3));
    assert_eq!(
        ext_ex,
        Some(BindShadersEx {
            gs: 4,
            hs: 5,
            ds: 6,
        })
    );

    // Additional trailing bytes beyond the known `{gs, hs, ds}` fields should be ignored.
    let (ext_trailing_cmd, ext_trailing_ex) = decode(&extended_with_trailing);
    let ext_trailing_size_bytes = ext_trailing_cmd.hdr.size_bytes;
    let (ext_trailing_vs, ext_trailing_ps, ext_trailing_cs) = (
        ext_trailing_cmd.vs,
        ext_trailing_cmd.ps,
        ext_trailing_cmd.cs,
    );
    assert_eq!(
        ext_trailing_size_bytes as usize,
        AerogpuCmdBindShaders::EX_SIZE_BYTES + 4
    );
    assert_eq!(
        (ext_trailing_vs, ext_trailing_ps, ext_trailing_cs),
        (1, 2, 3)
    );
    assert_eq!(
        ext_trailing_ex,
        Some(BindShadersEx {
            gs: 4,
            hs: 5,
            ds: 6,
        })
    );

    // Legacy decode (vs/ps/cs) must remain stable between base and extended packets.
    assert_eq!((base_vs, base_ps, base_cs), (ext_vs, ext_ps, ext_cs));
    assert_eq!(
        (base_vs, base_ps, base_cs),
        (base_trailing_vs, base_trailing_ps, base_trailing_cs),
    );
}

#[test]
fn bind_shaders_gs_legacy_decoding_is_size_gated() {
    let gs_mirror = 0x1234_5678;
    let legacy = build_bind_shaders_stream_with_reserved0(false, false, gs_mirror);
    let legacy_with_trailing = build_bind_shaders_stream_with_reserved0(false, true, gs_mirror);

    let decode = |stream: &[u8]| {
        let iter = AerogpuCmdStreamIter::new(stream).unwrap();
        let packets = iter.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(packets.len(), 1);
        packets[0].decode_bind_shaders_payload_le().unwrap()
    };

    // Legacy 24-byte packet: reserved0 is GS.
    let (cmd, ex) = decode(&legacy);
    assert_eq!(cmd.hdr.size_bytes as usize, AerogpuCmdBindShaders::SIZE_BYTES);
    assert_eq!(ex, None);
    assert_eq!(cmd.gs(), gs_mirror);

    // Forward-compat: base packet with unknown trailing bytes (24 < size_bytes < 36).
    // reserved0 must be ignored.
    let (cmd, ex) = decode(&legacy_with_trailing);
    assert_eq!(
        cmd.hdr.size_bytes as usize,
        AerogpuCmdBindShaders::SIZE_BYTES + 4
    );
    assert_eq!(ex, None);
    // Avoid taking references to packed fields.
    let reserved0 = cmd.reserved0;
    assert_eq!(reserved0, gs_mirror);
    assert_eq!(cmd.gs(), 0);
}
