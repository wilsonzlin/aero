use core::mem::{offset_of, size_of};

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_create_shader_dxbc_payload_le, decode_cmd_hdr_le,
    decode_cmd_set_shader_resource_buffers_bindings_le,
    decode_cmd_set_unordered_access_buffers_bindings_le, decode_stage_ex, encode_stage_ex,
    resolve_shader_stage_with_ex, resolve_stage, AerogpuCmdOpcode, AerogpuCmdSetConstantBuffers,
    AerogpuCmdSetSamplers, AerogpuCmdSetShaderConstantsF, AerogpuCmdSetShaderResourceBuffers,
    AerogpuCmdSetTexture, AerogpuCmdSetUnorderedAccessBuffers, AerogpuCmdStreamHeader,
    AerogpuConstantBufferBinding, AerogpuD3dShaderStage, AerogpuShaderResourceBufferBinding,
    AerogpuShaderStage, AerogpuShaderStageEx, AerogpuShaderStageResolved, AerogpuStageResolveError,
    AerogpuUnorderedAccessBufferBinding,
};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn stage_ex_encode_decode_roundtrip() {
    let all = [
        AerogpuShaderStageEx::None,
        AerogpuShaderStageEx::Geometry,
        AerogpuShaderStageEx::Hull,
        AerogpuShaderStageEx::Domain,
    ];

    for stage_ex in all {
        let (shader_stage, reserved0) = encode_stage_ex(stage_ex);
        assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
        assert_eq!(reserved0, stage_ex as u32);
        assert_eq!(decode_stage_ex(shader_stage, reserved0), Some(stage_ex));
    }

    // reserved0==0 is the legacy "no override" encoding and must resolve to stage_ex=None.
    assert_eq!(
        decode_stage_ex(AerogpuShaderStage::Compute as u32, 0),
        Some(AerogpuShaderStageEx::None)
    );

    // Non-compute legacy stage never uses stage_ex.
    assert_eq!(
        decode_stage_ex(
            AerogpuShaderStage::Vertex as u32,
            AerogpuShaderStageEx::Geometry as u32
        ),
        None
    );

    // Compute program type (5) is accepted as an alias for Compute.
    assert_eq!(
        decode_stage_ex(
            AerogpuShaderStage::Compute as u32,
            AerogpuShaderStageEx::Compute as u32
        ),
        Some(AerogpuShaderStageEx::Compute)
    );
}

#[test]
fn resolve_shader_stage_with_ex_handles_legacy_and_stage_ex_encodings() {
    use AerogpuShaderStageResolved as Res;

    // Legacy VS/PS/CS must be representable.
    assert_eq!(
        resolve_shader_stage_with_ex(AerogpuShaderStage::Vertex as u32, 0),
        Res::Vertex
    );
    assert_eq!(
        resolve_shader_stage_with_ex(AerogpuShaderStage::Pixel as u32, 0),
        Res::Pixel
    );
    assert_eq!(
        resolve_shader_stage_with_ex(AerogpuShaderStage::Compute as u32, 0),
        Res::Compute
    );

    // Stage-ex encoding: shader_stage==COMPUTE plus non-zero reserved0 discriminator.
    assert_eq!(
        resolve_shader_stage_with_ex(
            AerogpuShaderStage::Compute as u32,
            AerogpuShaderStageEx::Geometry as u32
        ),
        Res::Geometry
    );
    assert_eq!(
        resolve_shader_stage_with_ex(
            AerogpuShaderStage::Compute as u32,
            AerogpuShaderStageEx::Hull as u32
        ),
        Res::Hull
    );
    assert_eq!(
        resolve_shader_stage_with_ex(
            AerogpuShaderStage::Compute as u32,
            AerogpuShaderStageEx::Domain as u32
        ),
        Res::Domain
    );

    // Non-compute stages ignore reserved0.
    assert_eq!(
        resolve_shader_stage_with_ex(AerogpuShaderStage::Vertex as u32, 123),
        Res::Vertex
    );

    // Unknown discriminators are preserved for forward-compat.
    assert_eq!(
        resolve_shader_stage_with_ex(AerogpuShaderStage::Compute as u32, 42),
        Res::Unknown {
            shader_stage: AerogpuShaderStage::Compute as u32,
            stage_ex: 42
        }
    );
}

#[test]
fn resolve_stage_rejects_vertex_program_type_in_stage_ex() {
    // DXBC program type 1 is Vertex, but stage_ex values 0/1 (Pixel/Vertex) must be encoded via
    // the legacy `shader_stage` field, not via reserved0/stage_ex.
    assert_eq!(
        resolve_stage(AerogpuShaderStage::Compute as u32, 1),
        Err(AerogpuStageResolveError::InvalidStageEx { stage_ex: 1 })
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
    // Compute stage is canonicalized to stage_ex=None (reserved0=0).
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
    assert_eq!(
        size_bytes as usize,
        size_of::<AerogpuCmdSetShaderConstantsF>() + 16
    );
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
    // Compute is canonicalized to legacy encoding (`reserved0==0`).
    assert_eq!(reserved0, 0);
    assert_eq!(
        decode_stage_ex(stage, reserved0),
        Some(AerogpuShaderStageEx::None)
    );
    cursor += size_bytes as usize;

    assert_eq!(cursor, buf.len());
}

#[test]
fn legacy_compute_stage_ex_zero_resolves_to_compute() {
    let mut w = AerogpuCmdWriter::new();
    w.create_shader_dxbc(1, AerogpuShaderStage::Compute, &[]);
    let buf = w.finish();

    let (cmd, _dxbc) =
        decode_cmd_create_shader_dxbc_payload_le(&buf[AerogpuCmdStreamHeader::SIZE_BYTES..])
            .expect("CREATE_SHADER_DXBC must decode");

    let stage = cmd.stage;
    let stage_ex = cmd.reserved0;
    assert_eq!(stage, AerogpuShaderStage::Compute as u32);
    assert_eq!(stage_ex, 0);
    assert_eq!(
        resolve_stage(stage, stage_ex).expect("stage must resolve"),
        AerogpuD3dShaderStage::Compute
    );
}

#[test]
fn extended_stage_ex_resolves_geometry_hull_domain() {
    let cases: &[(AerogpuShaderStageEx, AerogpuD3dShaderStage)] = &[
        (
            AerogpuShaderStageEx::Geometry,
            AerogpuD3dShaderStage::Geometry,
        ),
        (AerogpuShaderStageEx::Hull, AerogpuD3dShaderStage::Hull),
        (AerogpuShaderStageEx::Domain, AerogpuD3dShaderStage::Domain),
    ];

    for &(stage_ex, expected) in cases {
        let mut w = AerogpuCmdWriter::new();
        w.create_shader_dxbc_ex(1, stage_ex, &[]);
        let buf = w.finish();

        let (cmd, _dxbc) =
            decode_cmd_create_shader_dxbc_payload_le(&buf[AerogpuCmdStreamHeader::SIZE_BYTES..])
                .expect("CREATE_SHADER_DXBC must decode");

        let stage = cmd.stage;
        let reserved0 = cmd.reserved0;
        assert_eq!(stage, AerogpuShaderStage::Compute as u32);
        assert_eq!(reserved0, stage_ex as u32);
        assert_eq!(
            resolve_stage(stage, reserved0).expect("stage must resolve"),
            expected
        );
    }
}

#[test]
fn cmd_writer_stage_ex_encodes_srv_uav_buffers() {
    let mut w = AerogpuCmdWriter::new();
    let srv_bindings: [AerogpuShaderResourceBufferBinding; 2] = [
        AerogpuShaderResourceBufferBinding {
            buffer: 10,
            offset_bytes: 16,
            size_bytes: 64,
            reserved0: 0,
        },
        AerogpuShaderResourceBufferBinding {
            buffer: 11,
            offset_bytes: 0,
            size_bytes: 128,
            reserved0: 0,
        },
    ];
    let uav_bindings: [AerogpuUnorderedAccessBufferBinding; 2] = [
        AerogpuUnorderedAccessBufferBinding {
            buffer: 20,
            offset_bytes: 0,
            size_bytes: 256,
            initial_count: 0,
        },
        AerogpuUnorderedAccessBufferBinding {
            buffer: 21,
            offset_bytes: 32,
            size_bytes: 96,
            initial_count: 123,
        },
    ];

    w.set_shader_resource_buffers_ex(AerogpuShaderStageEx::Hull, 5, &srv_bindings);
    w.set_unordered_access_buffers_ex(AerogpuShaderStageEx::Domain, 7, &uav_bindings);

    let buf = w.finish();
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // SET_SHADER_RESOURCE_BUFFERS (stage_ex)
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let pkt = &buf[cursor..cursor + hdr.size_bytes as usize];
    let (cmd, bindings) = decode_cmd_set_shader_resource_buffers_bindings_le(pkt).unwrap();
    let shader_stage = cmd.shader_stage;
    let reserved0 = cmd.reserved0;
    let start_slot = cmd.start_slot;
    let buffer_count = cmd.buffer_count;
    assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
    assert_eq!(reserved0, AerogpuShaderStageEx::Hull as u32);
    assert_eq!(start_slot, 5);
    assert_eq!(buffer_count, srv_bindings.len() as u32);
    assert_eq!(bindings.len(), srv_bindings.len());
    for (got, exp) in bindings.iter().zip(srv_bindings.iter()) {
        let got_buffer = got.buffer;
        let got_offset_bytes = got.offset_bytes;
        let got_size_bytes = got.size_bytes;
        let exp_buffer = exp.buffer;
        let exp_offset_bytes = exp.offset_bytes;
        let exp_size_bytes = exp.size_bytes;
        assert_eq!(got_buffer, exp_buffer);
        assert_eq!(got_offset_bytes, exp_offset_bytes);
        assert_eq!(got_size_bytes, exp_size_bytes);
    }
    cursor += hdr.size_bytes as usize;

    // SET_UNORDERED_ACCESS_BUFFERS (stage_ex)
    let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let pkt = &buf[cursor..cursor + hdr.size_bytes as usize];
    let (cmd, bindings) = decode_cmd_set_unordered_access_buffers_bindings_le(pkt).unwrap();
    let shader_stage = cmd.shader_stage;
    let reserved0 = cmd.reserved0;
    let start_slot = cmd.start_slot;
    let uav_count = cmd.uav_count;
    assert_eq!(shader_stage, AerogpuShaderStage::Compute as u32);
    assert_eq!(reserved0, AerogpuShaderStageEx::Domain as u32);
    assert_eq!(start_slot, 7);
    assert_eq!(uav_count, uav_bindings.len() as u32);
    assert_eq!(bindings.len(), uav_bindings.len());
    for (got, exp) in bindings.iter().zip(uav_bindings.iter()) {
        let got_buffer = got.buffer;
        let got_offset_bytes = got.offset_bytes;
        let got_size_bytes = got.size_bytes;
        let got_initial_count = got.initial_count;
        let exp_buffer = exp.buffer;
        let exp_offset_bytes = exp.offset_bytes;
        let exp_size_bytes = exp.size_bytes;
        let exp_initial_count = exp.initial_count;
        assert_eq!(got_buffer, exp_buffer);
        assert_eq!(got_offset_bytes, exp_offset_bytes);
        assert_eq!(got_size_bytes, exp_size_bytes);
        assert_eq!(got_initial_count, exp_initial_count);
    }
    cursor += hdr.size_bytes as usize;

    assert_eq!(cursor, buf.len());
}
