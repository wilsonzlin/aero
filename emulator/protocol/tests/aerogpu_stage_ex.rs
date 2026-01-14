use core::mem::{offset_of, size_of};

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_hdr_le, decode_stage_ex, encode_stage_ex, AerogpuCmdOpcode,
    AerogpuCmdSetConstantBuffers, AerogpuCmdSetSamplers, AerogpuCmdSetShaderConstantsF,
    AerogpuCmdSetShaderResourceBuffers, AerogpuCmdSetTexture, AerogpuCmdSetUnorderedAccessBuffers,
    AerogpuCmdStreamHeader, AerogpuCmdStreamIter, AerogpuConstantBufferBinding,
    AerogpuShaderResourceBufferBinding, AerogpuShaderStage, AerogpuShaderStageEx,
    AerogpuUnorderedAccessBufferBinding,
};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn stage_ex_encode_decode_roundtrip_nonzero() {
    for stage_ex in [
        AerogpuShaderStageEx::Vertex,
        AerogpuShaderStageEx::Geometry,
        AerogpuShaderStageEx::Hull,
        AerogpuShaderStageEx::Domain,
        AerogpuShaderStageEx::Compute,
    ] {
        let (shader_stage, reserved0) = encode_stage_ex(stage_ex);
        assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
        assert_eq!(reserved0, stage_ex as u32);
        assert_eq!(decode_stage_ex(shader_stage, reserved0), Some(stage_ex));
    }

    // Legacy compute: `reserved0 == 0` means the real compute stage (no stage_ex).
    // Backwards-compat: this must *not* be interpreted as Pixel.
    assert_eq!(decode_stage_ex(AerogpuShaderStage::Compute as u32, 0), None);

    // Non-compute legacy stage never uses stage_ex.
    assert_eq!(
        decode_stage_ex(
            AerogpuShaderStage::Vertex as u32,
            AerogpuShaderStageEx::Geometry as u32
        ),
        None
    );
}

#[test]
fn stage_ex_legacy_compute_reserved0_zero_does_not_decode_as_pixel() {
    // Legacy compute bindings use (shader_stage=COMPUTE, reserved0=0); this must not decode as
    // `stage_ex = Pixel` (DXBC program-type 0).
    assert_eq!(
        decode_stage_ex(AerogpuShaderStage::Compute as u32, 0),
        None
    );
}

#[test]
fn cmd_writer_stage_ex_option_overrides_shader_stage() {
    let mut w = AerogpuCmdWriter::new();
    w.set_texture_stage_ex(
        AerogpuShaderStage::Vertex,
        Some(AerogpuShaderStageEx::Geometry),
        0,
        99,
    );
    let buf = w.finish();
    let cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    assert_eq!(opcode, AerogpuCmdOpcode::SetTexture as u32);

    let shader_stage = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetTexture, shader_stage)
            ..cursor + offset_of!(AerogpuCmdSetTexture, shader_stage) + 4]
            .try_into()
            .unwrap(),
    );
    let reserved0 = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetTexture, reserved0)
            ..cursor + offset_of!(AerogpuCmdSetTexture, reserved0) + 4]
            .try_into()
            .unwrap(),
    );

    // When stage_ex is provided, the packet must be encoded as a compute-stage packet with a
    // non-zero reserved0 discriminator (see `encode_stage_ex`).
    assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
    assert_eq!(reserved0, AerogpuShaderStageEx::Geometry as u32);
}

#[test]
fn cmd_writer_legacy_bindings_write_reserved0_zero() {
    let mut w = AerogpuCmdWriter::new();
    w.set_texture(AerogpuShaderStage::Pixel, 0, 99);
    w.set_samplers(AerogpuShaderStage::Vertex, 2, &[10, 11]);
    w.set_constant_buffers(
        AerogpuShaderStage::Pixel,
        0,
        &[AerogpuConstantBufferBinding {
            buffer: 123,
            offset_bytes: 0,
            size_bytes: 64,
            reserved0: 0,
        }],
    );
    w.set_shader_resource_buffers(
        AerogpuShaderStage::Pixel,
        0,
        &[AerogpuShaderResourceBufferBinding {
            buffer: 456,
            offset_bytes: 0,
            size_bytes: 16,
            reserved0: 0,
        }],
    );
    w.set_unordered_access_buffers(
        AerogpuShaderStage::Compute,
        0,
        &[AerogpuUnorderedAccessBufferBinding {
            buffer: 789,
            offset_bytes: 0,
            size_bytes: 16,
            initial_count: 0,
        }],
    );
    w.set_shader_constants_f(AerogpuShaderStage::Vertex, 0, &[1.0, 2.0, 3.0, 4.0]);

    let buf = w.finish();
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // SET_TEXTURE
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetTexture as u32);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetTexture, reserved0)
                ..cursor + offset_of!(AerogpuCmdSetTexture, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );
    cursor += size_bytes as usize;

    // SET_SAMPLERS
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetSamplers as u32);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetSamplers, reserved0)
                ..cursor + offset_of!(AerogpuCmdSetSamplers, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );
    cursor += size_bytes as usize;

    // SET_CONSTANT_BUFFERS
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetConstantBuffers as u32);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0)
                ..cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );
    cursor += size_bytes as usize;

    // SET_SHADER_RESOURCE_BUFFERS
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetShaderResourceBuffers as u32);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0)
                ..cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );
    cursor += size_bytes as usize;

    // SET_UNORDERED_ACCESS_BUFFERS
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0)
                ..cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );
    cursor += size_bytes as usize;

    // SET_SHADER_CONSTANTS_F
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetShaderConstantsF as u32);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0)
                ..cursor + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );
    cursor += size_bytes as usize;

    assert_eq!(cursor, buf.len());
}

#[test]
fn cmd_writer_stage_ex_encodes_compute_and_reserved0() {
    let mut w = AerogpuCmdWriter::new();
    w.set_texture_ex(AerogpuShaderStageEx::Geometry, 3, 44);
    w.set_samplers_ex(AerogpuShaderStageEx::Hull, 0, &[1, 2, 3]);
    w.set_constant_buffers_ex(
        AerogpuShaderStageEx::Domain,
        1,
        &[AerogpuConstantBufferBinding {
            buffer: 7,
            offset_bytes: 0,
            size_bytes: 16,
            reserved0: 0,
        }],
    );
    w.set_shader_resource_buffers_ex(
        AerogpuShaderStageEx::Hull,
        0,
        &[AerogpuShaderResourceBufferBinding {
            buffer: 8,
            offset_bytes: 0,
            size_bytes: 16,
            reserved0: 0,
        }],
    );
    w.set_unordered_access_buffers_ex(
        AerogpuShaderStageEx::Domain,
        1,
        &[AerogpuUnorderedAccessBufferBinding {
            buffer: 9,
            offset_bytes: 0,
            size_bytes: 16,
            initial_count: 0,
        }],
    );
    w.set_shader_constants_f_ex(AerogpuShaderStageEx::Compute, 0, &[1.0, 2.0, 3.0, 4.0]);

    let buf = w.finish();
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // SET_TEXTURE (stage_ex)
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetTexture as u32);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetTexture, shader_stage)
                ..cursor + offset_of!(AerogpuCmdSetTexture, shader_stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Compute as u32
    );
    let reserved0 = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetTexture, reserved0)
            ..cursor + offset_of!(AerogpuCmdSetTexture, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        decode_stage_ex(AerogpuShaderStage::Compute as u32, reserved0),
        Some(AerogpuShaderStageEx::Geometry)
    );
    cursor += size_bytes as usize;

    // SET_SAMPLERS (stage_ex)
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetSamplers as u32);
    let reserved0 = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetSamplers, reserved0)
            ..cursor + offset_of!(AerogpuCmdSetSamplers, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        decode_stage_ex(AerogpuShaderStage::Compute as u32, reserved0),
        Some(AerogpuShaderStageEx::Hull)
    );
    cursor += size_bytes as usize;

    // SET_CONSTANT_BUFFERS (stage_ex)
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetConstantBuffers as u32);
    let reserved0 = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0)
            ..cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        decode_stage_ex(AerogpuShaderStage::Compute as u32, reserved0),
        Some(AerogpuShaderStageEx::Domain)
    );
    cursor += size_bytes as usize;

    // SET_SHADER_RESOURCE_BUFFERS (stage_ex)
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetShaderResourceBuffers as u32);
    let reserved0 = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0)
            ..cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        decode_stage_ex(AerogpuShaderStage::Compute as u32, reserved0),
        Some(AerogpuShaderStageEx::Hull)
    );
    cursor += size_bytes as usize;

    // SET_UNORDERED_ACCESS_BUFFERS (stage_ex)
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32);
    let reserved0 = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0)
            ..cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(
        decode_stage_ex(AerogpuShaderStage::Compute as u32, reserved0),
        Some(AerogpuShaderStageEx::Domain)
    );
    cursor += size_bytes as usize;

    // SET_SHADER_CONSTANTS_F (stage_ex)
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::SetShaderConstantsF as u32);
    assert_eq!(size_bytes as usize, size_of::<AerogpuCmdSetShaderConstantsF>() + 16);
    let stage = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetShaderConstantsF, stage)
            ..cursor + offset_of!(AerogpuCmdSetShaderConstantsF, stage) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(stage, AerogpuShaderStage::Compute as u32);
    let reserved0 = u32::from_le_bytes(
        buf[cursor + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0)
            ..cursor + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(reserved0, AerogpuShaderStageEx::Compute as u32);
    assert_eq!(
        decode_stage_ex(stage, reserved0),
        Some(AerogpuShaderStageEx::Compute)
    );
    cursor += size_bytes as usize;

    assert_eq!(cursor, buf.len());
}

#[test]
fn legacy_compute_packets_do_not_decode_reserved0_zero_as_stage_ex() {
    let mut w = AerogpuCmdWriter::new();

    // Legacy compute packets: shader_stage == COMPUTE and reserved0 == 0.
    w.set_texture(AerogpuShaderStage::Compute, 0, 99);
    let bindings: [AerogpuConstantBufferBinding; 1] = [AerogpuConstantBufferBinding {
        buffer: 1,
        offset_bytes: 0,
        size_bytes: 16,
        reserved0: 0,
    }];
    w.set_constant_buffers(AerogpuShaderStage::Compute, 0, &bindings);

    // Extended stage example: shader_stage == COMPUTE and reserved0 != 0.
    w.set_texture_stage_ex(
        AerogpuShaderStage::Compute,
        Some(AerogpuShaderStageEx::Geometry),
        1,
        100,
    );

    w.flush();

    let bytes = w.finish();
    let packets = AerogpuCmdStreamIter::new(&bytes)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    // SET_TEXTURE (legacy compute).
    assert_eq!(packets[0].opcode, Some(AerogpuCmdOpcode::SetTexture));
    {
        let payload = packets[0].payload;
        assert_eq!(payload.len(), 16);
        let shader_stage = u32::from_le_bytes(payload[0..4].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(payload[12..16].try_into().unwrap());
        assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
        assert_eq!(reserved0, 0);
        assert_eq!(decode_stage_ex(shader_stage, reserved0), None);
    }

    // SET_CONSTANT_BUFFERS (legacy compute).
    assert_eq!(
        packets[1].opcode,
        Some(AerogpuCmdOpcode::SetConstantBuffers)
    );
    {
        let (cmd, _bindings) = packets[1].decode_set_constant_buffers_payload_le().unwrap();
        // Copy out packed fields to avoid creating references to unaligned data (E0793).
        let shader_stage = cmd.shader_stage;
        let reserved0 = cmd.reserved0;
        assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
        assert_eq!(reserved0, 0);
        assert_eq!(decode_stage_ex(shader_stage, reserved0), None);
    }

    // SET_TEXTURE (stage_ex = GEOMETRY).
    assert_eq!(packets[2].opcode, Some(AerogpuCmdOpcode::SetTexture));
    {
        let payload = packets[2].payload;
        let shader_stage = u32::from_le_bytes(payload[0..4].try_into().unwrap());
        let reserved0 = u32::from_le_bytes(payload[12..16].try_into().unwrap());
        assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
        assert_eq!(reserved0, AerogpuShaderStageEx::Geometry as u32);
        assert_eq!(
            decode_stage_ex(shader_stage, reserved0),
            Some(AerogpuShaderStageEx::Geometry)
        );
    }
}
