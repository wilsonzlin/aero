use core::mem::{offset_of, size_of};

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_bind_shaders_payload_le, decode_cmd_dispatch_le, decode_cmd_hdr_le,
    decode_cmd_stream_header_le, AerogpuBlendFactor, AerogpuBlendOp, AerogpuCmdBindShaders,
    AerogpuCmdCreateInputLayout, AerogpuCmdCreateShaderDxbc, AerogpuCmdDispatch,
    AerogpuCmdExportSharedSurface, AerogpuCmdHdr, AerogpuCmdImportSharedSurface, AerogpuCmdOpcode,
    AerogpuCmdPresentEx, AerogpuCmdReleaseSharedSurface, AerogpuCmdSetConstantBuffers,
    AerogpuCmdSetSamplers, AerogpuCmdSetShaderConstantsB, AerogpuCmdSetShaderConstantsF,
    AerogpuCmdSetShaderConstantsI, AerogpuCmdSetShaderResourceBuffers, AerogpuCmdSetTexture,
    AerogpuCmdStreamHeader, AerogpuCmdUploadResource, AerogpuCompareFunc,
    AerogpuConstantBufferBinding, AerogpuCullMode, AerogpuFillMode,
    AerogpuShaderResourceBufferBinding, AerogpuShaderStage, AerogpuShaderStageEx,
    AerogpuVertexBufferBinding, BindShadersEx, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

fn align_up(v: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two());
    (v + (a - 1)) & !(a - 1)
}

#[test]
fn cmd_writer_default_emits_valid_stream_header() {
    let buf = AerogpuCmdWriter::default().finish();
    assert!(buf.len() >= AerogpuCmdStreamHeader::SIZE_BYTES);

    let stream = decode_cmd_stream_header_le(&buf).expect("cmd stream header must decode");
    let magic = stream.magic;
    let abi_version = stream.abi_version;
    let size_bytes = stream.size_bytes;
    let flags = stream.flags;
    let reserved0 = stream.reserved0;
    let reserved1 = stream.reserved1;
    assert_eq!(magic, AEROGPU_CMD_STREAM_MAGIC);
    assert_eq!(abi_version, AEROGPU_ABI_VERSION_U32);
    assert!(size_bytes as usize >= AerogpuCmdStreamHeader::SIZE_BYTES);
    assert_eq!(size_bytes as usize, buf.len());
    assert_eq!(flags, 0);
    assert_eq!(reserved0, 0);
    assert_eq!(reserved1, 0);
}

#[test]
fn cmd_writer_bind_shaders_with_gs_reuses_reserved0_field() {
    let mut w = AerogpuCmdWriter::new();
    w.bind_shaders_with_gs(11, 22, 33, 44);
    w.flush();

    let buf = w.finish();

    let packet_offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    let hdr = decode_cmd_hdr_le(&buf[packet_offset..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::BindShaders as u32);
    assert_eq!(size_bytes as usize, size_of::<AerogpuCmdBindShaders>());

    let reserved0 = u32::from_le_bytes(
        buf[packet_offset + offset_of!(AerogpuCmdBindShaders, reserved0)
            ..packet_offset + offset_of!(AerogpuCmdBindShaders, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(reserved0, 22);
}

#[test]
fn cmd_writer_bind_shaders_ex_emits_append_only_extended_payload() {
    let mut w = AerogpuCmdWriter::new();
    w.bind_shaders_ex(
        /* vs */ 11, /* ps */ 22, /* cs */ 33, /* gs */ 44, /* hs */ 55,
        /* ds */ 66,
    );
    w.flush();

    let buf = w.finish();
    assert_eq!(
        buf.len() % 4,
        0,
        "command stream must be 4-byte aligned (len={})",
        buf.len()
    );

    let stream = decode_cmd_stream_header_le(&buf).expect("cmd stream header must decode");
    assert_eq!(stream.size_bytes as usize, buf.len());

    let packet_offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    let hdr = decode_cmd_hdr_le(&buf[packet_offset..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::BindShaders as u32);
    assert_eq!(size_bytes as usize, AerogpuCmdBindShaders::EX_SIZE_BYTES);

    // Extension is append-only: `reserved0` stays 0 and `{gs,hs,ds}` are appended after the base struct.
    let reserved0 = u32::from_le_bytes(
        buf[packet_offset + offset_of!(AerogpuCmdBindShaders, reserved0)
            ..packet_offset + offset_of!(AerogpuCmdBindShaders, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(reserved0, 0);

    // Trailing handles are at offsets 24/28/32 from the packet start.
    assert_eq!(
        u32::from_le_bytes(
            buf[packet_offset + 24..packet_offset + 28]
                .try_into()
                .unwrap()
        ),
        44
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[packet_offset + 28..packet_offset + 32]
                .try_into()
                .unwrap()
        ),
        55
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[packet_offset + 32..packet_offset + 36]
                .try_into()
                .unwrap()
        ),
        66
    );

    let (cmd, ex) = decode_cmd_bind_shaders_payload_le(&buf[packet_offset..]).unwrap();
    // `AerogpuCmdBindShaders` is `#[repr(packed)]`, so copy fields out before asserting to avoid
    // creating unaligned references.
    let vs = cmd.vs;
    let ps = cmd.ps;
    let cs = cmd.cs;
    let reserved0 = cmd.reserved0;
    assert_eq!(vs, 11);
    assert_eq!(ps, 22);
    assert_eq!(cs, 33);
    assert_eq!(reserved0, 0);
    assert_eq!(
        ex,
        Some(BindShadersEx {
            gs: 44,
            hs: 55,
            ds: 66,
        })
    );
}

#[test]
fn cmd_writer_bind_shaders_ex_can_mirror_gs_into_reserved0() {
    let mut w = AerogpuCmdWriter::new();
    w.bind_shaders_ex_with_gs_mirror(
        /* vs */ 11, /* ps */ 22, /* cs */ 33, /* gs */ 44, /* hs */ 55,
        /* ds */ 66,
    );
    w.flush();

    let buf = w.finish();

    let packet_offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    let hdr = decode_cmd_hdr_le(&buf[packet_offset..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::BindShaders as u32);
    assert_eq!(size_bytes as usize, AerogpuCmdBindShaders::EX_SIZE_BYTES);

    let reserved0 = u32::from_le_bytes(
        buf[packet_offset + offset_of!(AerogpuCmdBindShaders, reserved0)
            ..packet_offset + offset_of!(AerogpuCmdBindShaders, reserved0) + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(reserved0, 44);

    let (cmd, ex) = decode_cmd_bind_shaders_payload_le(&buf[packet_offset..]).unwrap();
    // Avoid creating unaligned references to packed fields.
    let reserved0 = cmd.reserved0;
    assert_eq!(reserved0, 44);
    assert_eq!(
        ex,
        Some(BindShadersEx {
            gs: 44,
            hs: 55,
            ds: 66,
        })
    );
}

#[test]
fn cmd_writer_bind_shaders_hs_ds_is_sugar_for_extended_packet() {
    let mut w = AerogpuCmdWriter::new();
    w.bind_shaders_hs_ds(/*hs=*/ 55, /*ds=*/ 66);
    w.flush();

    let buf = w.finish();

    let packet_offset = AerogpuCmdStreamHeader::SIZE_BYTES;
    let hdr = decode_cmd_hdr_le(&buf[packet_offset..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::BindShaders as u32);
    assert_eq!(size_bytes as usize, AerogpuCmdBindShaders::EX_SIZE_BYTES);

    let (cmd, ex) = decode_cmd_bind_shaders_payload_le(&buf[packet_offset..]).unwrap();
    let vs = cmd.vs;
    let ps = cmd.ps;
    let cs = cmd.cs;
    let reserved0 = cmd.reserved0;
    assert_eq!(vs, 0);
    assert_eq!(ps, 0);
    assert_eq!(cs, 0);
    assert_eq!(reserved0, 0);
    assert_eq!(
        ex,
        Some(BindShadersEx {
            gs: 0,
            hs: 55,
            ds: 66,
        })
    );
}

#[test]
fn cmd_writer_dispatch_stage_ex_is_encoded_in_reserved0() {
    let mut w = AerogpuCmdWriter::new();
    w.dispatch(1, 2, 3);
    w.dispatch_ex(AerogpuShaderStageEx::Hull, 4, 5, 6);
    // `stage_ex=Compute` should canonicalize to legacy encoding (`reserved0=0`).
    w.dispatch_ex(AerogpuShaderStageEx::Compute, 7, 8, 9);

    let buf = w.finish();
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // DISPATCH (legacy)
    {
        let cmd = decode_cmd_dispatch_le(&buf[cursor..]).unwrap();
        let size_bytes = cmd.hdr.size_bytes as usize;
        let group_count_x = cmd.group_count_x;
        let group_count_y = cmd.group_count_y;
        let group_count_z = cmd.group_count_z;
        let reserved0 = cmd.reserved0;
        assert_eq!(size_bytes, AerogpuCmdDispatch::SIZE_BYTES);
        assert_eq!(group_count_x, 1);
        assert_eq!(group_count_y, 2);
        assert_eq!(group_count_z, 3);
        assert_eq!(reserved0, 0);
        cursor += size_bytes;
    }

    // DISPATCH (stage_ex=Hull)
    {
        let cmd = decode_cmd_dispatch_le(&buf[cursor..]).unwrap();
        let size_bytes = cmd.hdr.size_bytes as usize;
        let group_count_x = cmd.group_count_x;
        let group_count_y = cmd.group_count_y;
        let group_count_z = cmd.group_count_z;
        let reserved0 = cmd.reserved0;
        assert_eq!(size_bytes, AerogpuCmdDispatch::SIZE_BYTES);
        assert_eq!(group_count_x, 4);
        assert_eq!(group_count_y, 5);
        assert_eq!(group_count_z, 6);
        assert_eq!(reserved0, AerogpuShaderStageEx::Hull as u32);
        cursor += size_bytes;
    }

    // DISPATCH (stage_ex=Compute canonicalized)
    {
        let cmd = decode_cmd_dispatch_le(&buf[cursor..]).unwrap();
        let size_bytes = cmd.hdr.size_bytes as usize;
        let group_count_x = cmd.group_count_x;
        let group_count_y = cmd.group_count_y;
        let group_count_z = cmd.group_count_z;
        let reserved0 = cmd.reserved0;
        assert_eq!(size_bytes, AerogpuCmdDispatch::SIZE_BYTES);
        assert_eq!(group_count_x, 7);
        assert_eq!(group_count_y, 8);
        assert_eq!(group_count_z, 9);
        assert_eq!(reserved0, 0);
        cursor += size_bytes;
    }

    assert_eq!(cursor, buf.len());
}

#[test]
fn cmd_writer_emits_geometry_stage_binding_packets() {
    let mut w = AerogpuCmdWriter::new();

    w.set_texture(AerogpuShaderStage::Geometry, 7, 123);
    w.set_samplers(AerogpuShaderStage::Geometry, 2, &[42, 43]);
    w.set_constant_buffers(
        AerogpuShaderStage::Geometry,
        1,
        &[AerogpuConstantBufferBinding {
            buffer: 11,
            offset_bytes: 16,
            size_bytes: 32,
            reserved0: 0,
        }],
    );
    w.set_shader_resource_buffers(
        AerogpuShaderStage::Geometry,
        3,
        &[AerogpuShaderResourceBufferBinding {
            buffer: 55,
            offset_bytes: 0,
            size_bytes: 0,
            reserved0: 0,
        }],
    );
    w.flush();

    let buf = w.finish();

    let read_u32 =
        |off: usize| -> u32 { u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) };

    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // SET_TEXTURE (GS)
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        // `AerogpuCmdHdr` is packed; copy fields out to avoid unaligned references.
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes;
        assert_eq!(opcode, AerogpuCmdOpcode::SetTexture as u32);
        assert_eq!(size_bytes as usize, size_of::<AerogpuCmdSetTexture>());
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetTexture, shader_stage)),
            AerogpuShaderStage::Geometry as u32
        );
        assert_eq!(read_u32(cursor + offset_of!(AerogpuCmdSetTexture, slot)), 7);
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetTexture, texture)),
            123
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetTexture, reserved0)),
            0
        );
        cursor += size_bytes as usize;
    }

    // SET_SAMPLERS (GS)
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes;
        assert_eq!(opcode, AerogpuCmdOpcode::SetSamplers as u32);
        assert_eq!(
            size_bytes as usize,
            size_of::<AerogpuCmdSetSamplers>() + 2 * size_of::<u32>()
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetSamplers, shader_stage)),
            AerogpuShaderStage::Geometry as u32
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetSamplers, start_slot)),
            2
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetSamplers, sampler_count)),
            2
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetSamplers, reserved0)),
            0
        );
        let payload = cursor + size_of::<AerogpuCmdSetSamplers>();
        assert_eq!(read_u32(payload), 42);
        assert_eq!(read_u32(payload + 4), 43);
        cursor += size_bytes as usize;
    }

    // SET_CONSTANT_BUFFERS (GS)
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes;
        assert_eq!(opcode, AerogpuCmdOpcode::SetConstantBuffers as u32);
        assert_eq!(
            size_bytes as usize,
            size_of::<AerogpuCmdSetConstantBuffers>() + size_of::<AerogpuConstantBufferBinding>()
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetConstantBuffers, shader_stage)),
            AerogpuShaderStage::Geometry as u32
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetConstantBuffers, start_slot)),
            1
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count)),
            1
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0)),
            0
        );
        let b = cursor + size_of::<AerogpuCmdSetConstantBuffers>();
        assert_eq!(
            read_u32(b + offset_of!(AerogpuConstantBufferBinding, buffer)),
            11
        );
        assert_eq!(
            read_u32(b + offset_of!(AerogpuConstantBufferBinding, offset_bytes)),
            16
        );
        assert_eq!(
            read_u32(b + offset_of!(AerogpuConstantBufferBinding, size_bytes)),
            32
        );
        assert_eq!(
            read_u32(b + offset_of!(AerogpuConstantBufferBinding, reserved0)),
            0
        );
        cursor += size_bytes as usize;
    }

    // SET_SHADER_RESOURCE_BUFFERS (GS)
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes;
        assert_eq!(opcode, AerogpuCmdOpcode::SetShaderResourceBuffers as u32);
        assert_eq!(
            size_bytes as usize,
            size_of::<AerogpuCmdSetShaderResourceBuffers>()
                + size_of::<AerogpuShaderResourceBufferBinding>()
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, shader_stage)),
            AerogpuShaderStage::Geometry as u32
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, start_slot)),
            3
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, buffer_count)),
            1
        );
        assert_eq!(
            read_u32(cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0)),
            0
        );
        let b = cursor + size_of::<AerogpuCmdSetShaderResourceBuffers>();
        assert_eq!(
            read_u32(b + offset_of!(AerogpuShaderResourceBufferBinding, buffer)),
            55
        );
        assert_eq!(
            read_u32(b + offset_of!(AerogpuShaderResourceBufferBinding, offset_bytes)),
            0
        );
        assert_eq!(
            read_u32(b + offset_of!(AerogpuShaderResourceBufferBinding, size_bytes)),
            0
        );
        assert_eq!(
            read_u32(b + offset_of!(AerogpuShaderResourceBufferBinding, reserved0)),
            0
        );
        cursor += size_bytes as usize;
    }

    // FLUSH
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        assert_eq!(opcode, AerogpuCmdOpcode::Flush as u32);
    }
}

#[test]
fn cmd_writer_emits_aligned_packets_and_updates_stream_size() {
    let mut w = AerogpuCmdWriter::new();

    w.create_buffer(1, 0xDEAD_BEEF, 1024, 0, 0);
    w.create_shader_dxbc(
        2,
        aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage::Vertex,
        &[0xAA, 0xBB, 0xCC],
    );
    w.create_input_layout(3, &[0x11]);
    w.upload_resource(1, 16, &[1, 2, 3, 4, 5]);

    let vbs = [
        AerogpuVertexBufferBinding {
            buffer: 10,
            stride_bytes: 16,
            offset_bytes: 0,
            reserved0: 0,
        },
        AerogpuVertexBufferBinding {
            buffer: 11,
            stride_bytes: 32,
            offset_bytes: 64,
            reserved0: 0,
        },
    ];
    w.set_vertex_buffers(0, &vbs);

    w.draw(3, 1, 0, 0);
    w.flush();

    let buf = w.finish();
    assert!(buf.len() >= AerogpuCmdStreamHeader::SIZE_BYTES);

    let stream = decode_cmd_stream_header_le(&buf).expect("cmd stream header must decode");
    let stream_magic = stream.magic;
    let stream_abi_version = stream.abi_version;
    let stream_size_bytes = stream.size_bytes;
    assert_eq!(stream_magic, AEROGPU_CMD_STREAM_MAGIC);
    assert_eq!(stream_abi_version, AEROGPU_ABI_VERSION_U32);
    assert_eq!(stream_size_bytes as usize, buf.len());

    // Walk packets using the public decode helper, ensuring packet size/alignment
    // does not overrun the stream.
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    let mut seen_opcodes = Vec::new();
    while cursor < buf.len() {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).expect("packet header must decode");
        assert!(hdr.size_bytes >= AerogpuCmdHdr::SIZE_BYTES as u32);
        assert_eq!(hdr.size_bytes % 4, 0);

        let pkt_size = hdr.size_bytes as usize;
        assert!(cursor + pkt_size <= buf.len());

        seen_opcodes.push(hdr.opcode);
        cursor += pkt_size;
    }
    assert_eq!(
        cursor,
        buf.len(),
        "packet walk must land exactly on end of stream"
    );

    let expected_sizes: &[(u32, usize)] = &[
        (
            AerogpuCmdOpcode::CreateBuffer as u32,
            size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdCreateBuffer>(),
        ),
        (
            AerogpuCmdOpcode::CreateShaderDxbc as u32,
            align_up(size_of::<AerogpuCmdCreateShaderDxbc>() + 3, 4),
        ),
        (
            AerogpuCmdOpcode::CreateInputLayout as u32,
            align_up(size_of::<AerogpuCmdCreateInputLayout>() + 1, 4),
        ),
        (
            AerogpuCmdOpcode::UploadResource as u32,
            align_up(size_of::<AerogpuCmdUploadResource>() + 5, 4),
        ),
        (
            AerogpuCmdOpcode::SetVertexBuffers as u32,
            size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdSetVertexBuffers>()
                + size_of::<AerogpuVertexBufferBinding>() * 2,
        ),
        (
            AerogpuCmdOpcode::Draw as u32,
            size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdDraw>(),
        ),
        (
            AerogpuCmdOpcode::Flush as u32,
            size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdFlush>(),
        ),
    ];

    // Validate `size_bytes` for each packet matches our expected padded size.
    cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
    for &(expected_opcode, expected_size) in expected_sizes {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes;
        assert_eq!(opcode, expected_opcode);
        assert_eq!(size_bytes as usize, expected_size);
        cursor += expected_size;
    }
    assert_eq!(cursor, buf.len());

    // Validate per-command self-described sizes for variable-length payloads.
    let shader_pkt_base = AerogpuCmdStreamHeader::SIZE_BYTES + expected_sizes[0].1;
    assert_eq!(
        u32::from_le_bytes(
            buf[shader_pkt_base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes)
                ..shader_pkt_base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes) + 4]
                .try_into()
                .unwrap()
        ),
        3
    );

    let input_layout_pkt_base = shader_pkt_base + expected_sizes[1].1;
    assert_eq!(
        u32::from_le_bytes(
            buf[input_layout_pkt_base + offset_of!(AerogpuCmdCreateInputLayout, blob_size_bytes)
                ..input_layout_pkt_base
                    + offset_of!(AerogpuCmdCreateInputLayout, blob_size_bytes)
                    + 4]
                .try_into()
                .unwrap()
        ),
        1
    );

    let upload_pkt_base = input_layout_pkt_base + expected_sizes[2].1;
    assert_eq!(
        u64::from_le_bytes(
            buf[upload_pkt_base + offset_of!(AerogpuCmdUploadResource, size_bytes)
                ..upload_pkt_base + offset_of!(AerogpuCmdUploadResource, size_bytes) + 8]
                .try_into()
                .unwrap()
        ),
        5
    );
    assert_eq!(
        &buf[upload_pkt_base + size_of::<AerogpuCmdUploadResource>()
            ..upload_pkt_base + size_of::<AerogpuCmdUploadResource>() + 5],
        &[1, 2, 3, 4, 5]
    );

    // Sanity check that our packet walk saw the opcodes we appended, in order.
    assert_eq!(
        seen_opcodes,
        expected_sizes.iter().map(|(op, _)| *op).collect::<Vec<_>>()
    );
}

#[test]
fn cmd_writer_create_shader_dxbc_ex_sets_stage_and_reserved0_and_padding() {
    let mut w = AerogpuCmdWriter::new();
    let stage_ex = AerogpuShaderStageEx::Domain;
    let dxbc = [0xAAu8, 0xBB, 0xCC];

    w.create_shader_dxbc_ex(7, stage_ex, &dxbc);
    w.flush();

    let buf = w.finish();
    let pkt0_base = AerogpuCmdStreamHeader::SIZE_BYTES;

    let hdr0 = decode_cmd_hdr_le(&buf[pkt0_base..]).unwrap();
    let opcode0 = hdr0.opcode;
    let size0 = hdr0.size_bytes;
    assert_eq!(opcode0, AerogpuCmdOpcode::CreateShaderDxbc as u32);
    let expected_size = align_up(size_of::<AerogpuCmdCreateShaderDxbc>() + dxbc.len(), 4);
    assert_eq!(size0 as usize, expected_size);

    assert_eq!(
        u32::from_le_bytes(
            buf[pkt0_base + offset_of!(AerogpuCmdCreateShaderDxbc, stage)
                ..pkt0_base + offset_of!(AerogpuCmdCreateShaderDxbc, stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Compute as u32,
        "legacy stage field forced to Compute for forward-compat",
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt0_base + offset_of!(AerogpuCmdCreateShaderDxbc, reserved0)
                ..pkt0_base + offset_of!(AerogpuCmdCreateShaderDxbc, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        stage_ex as u32,
        "extended stage encoding stored in reserved0",
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt0_base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes)
                ..pkt0_base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes) + 4]
                .try_into()
                .unwrap()
        ),
        dxbc.len() as u32,
    );

    let dxbc_base = pkt0_base + size_of::<AerogpuCmdCreateShaderDxbc>();
    assert_eq!(&buf[dxbc_base..dxbc_base + dxbc.len()], &dxbc);

    // Packet is 4-byte aligned; ensure trailing padding bytes are zero.
    let pkt0_end = pkt0_base + expected_size;
    assert!(pkt0_end >= dxbc_base + dxbc.len());
    for &b in &buf[dxbc_base + dxbc.len()..pkt0_end] {
        assert_eq!(b, 0, "CREATE_SHADER_DXBC_EX padding must be zero");
    }
}

#[test]
fn cmd_writer_emits_pipeline_and_binding_packets() {
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuBlendState, AerogpuCmdSetBlendState, AerogpuCmdSetDepthStencilState,
        AerogpuCmdSetRasterizerState, AerogpuCmdSetRenderState, AerogpuCmdSetSamplerState,
        AerogpuDepthStencilState, AerogpuRasterizerState,
        AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE,
    };

    let mut w = AerogpuCmdWriter::new();
    w.set_shader_constants_f(AerogpuShaderStage::Pixel, 4, &[1.0, 2.0, 3.0, 4.0]);
    w.set_shader_constants_i(AerogpuShaderStage::Pixel, 1, &[-1, 2, 3, 4]);
    w.set_shader_constants_b(AerogpuShaderStage::Pixel, 2, &[0, 1]);
    w.set_texture(AerogpuShaderStage::Pixel, 0, 99);
    w.set_sampler_state(AerogpuShaderStage::Pixel, 0, 7, 42);
    w.set_render_state(10, 20);
    w.set_blend_state_ext(
        true,
        AerogpuBlendFactor::One,
        AerogpuBlendFactor::Zero,
        AerogpuBlendOp::Add,
        AerogpuBlendFactor::SrcAlpha,
        AerogpuBlendFactor::InvSrcAlpha,
        AerogpuBlendOp::Subtract,
        [0.25, 0.5, 0.75, 1.0],
        0x00FF_FF00,
        0xF,
    );
    w.set_depth_stencil_state(true, true, AerogpuCompareFunc::LessEqual, false, 0xAA, 0xBB);
    w.set_rasterizer_state_ext(
        AerogpuFillMode::Solid,
        AerogpuCullMode::Back,
        false,
        true,
        -1,
        true,
    );
    w.present_ex(0, 0, 0x1234_5678);
    w.export_shared_surface(55, 0x0102_0304_0506_0708);
    w.import_shared_surface(56, 0x0102_0304_0506_0708);
    w.release_shared_surface(0x0102_0304_0506_0708);
    w.flush();

    let buf = w.finish();

    let expected_sizes: &[(u32, usize)] = &[
        (
            AerogpuCmdOpcode::SetShaderConstantsF as u32,
            size_of::<AerogpuCmdSetShaderConstantsF>() + 16,
        ),
        (
            AerogpuCmdOpcode::SetShaderConstantsI as u32,
            size_of::<AerogpuCmdSetShaderConstantsI>() + 16,
        ),
        (
            AerogpuCmdOpcode::SetShaderConstantsB as u32,
            // Bool constants are encoded as vec4<u32> per register.
            size_of::<AerogpuCmdSetShaderConstantsB>() + 32,
        ),
        (
            AerogpuCmdOpcode::SetTexture as u32,
            size_of::<AerogpuCmdSetTexture>(),
        ),
        (
            AerogpuCmdOpcode::SetSamplerState as u32,
            size_of::<AerogpuCmdSetSamplerState>(),
        ),
        (
            AerogpuCmdOpcode::SetRenderState as u32,
            size_of::<AerogpuCmdSetRenderState>(),
        ),
        (
            AerogpuCmdOpcode::SetBlendState as u32,
            size_of::<AerogpuCmdSetBlendState>(),
        ),
        (
            AerogpuCmdOpcode::SetDepthStencilState as u32,
            size_of::<AerogpuCmdSetDepthStencilState>(),
        ),
        (
            AerogpuCmdOpcode::SetRasterizerState as u32,
            size_of::<AerogpuCmdSetRasterizerState>(),
        ),
        (
            AerogpuCmdOpcode::PresentEx as u32,
            size_of::<AerogpuCmdPresentEx>(),
        ),
        (
            AerogpuCmdOpcode::ExportSharedSurface as u32,
            size_of::<AerogpuCmdExportSharedSurface>(),
        ),
        (
            AerogpuCmdOpcode::ImportSharedSurface as u32,
            size_of::<AerogpuCmdImportSharedSurface>(),
        ),
        (
            AerogpuCmdOpcode::ReleaseSharedSurface as u32,
            size_of::<AerogpuCmdReleaseSharedSurface>(),
        ),
        (
            AerogpuCmdOpcode::Flush as u32,
            size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdFlush>(),
        ),
    ];

    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
    for &(expected_opcode, expected_size) in expected_sizes {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes;
        assert_eq!(opcode, expected_opcode);
        assert_eq!(size_bytes as usize, expected_size);
        cursor += expected_size;
    }
    assert_eq!(cursor, buf.len());

    // Validate the variable-length shader constants packets.
    let pkt0_base = AerogpuCmdStreamHeader::SIZE_BYTES;
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt0_base + offset_of!(AerogpuCmdSetShaderConstantsF, vec4_count)
                ..pkt0_base + offset_of!(AerogpuCmdSetShaderConstantsF, vec4_count) + 4]
                .try_into()
                .unwrap()
        ),
        1
    );
    assert_eq!(
        f32::from_bits(u32::from_le_bytes(
            buf[pkt0_base + size_of::<AerogpuCmdSetShaderConstantsF>()
                ..pkt0_base + size_of::<AerogpuCmdSetShaderConstantsF>() + 4]
                .try_into()
                .unwrap()
        )),
        1.0
    );

    let pkt1_base = pkt0_base + expected_sizes[0].1;
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt1_base + offset_of!(AerogpuCmdSetShaderConstantsI, vec4_count)
                ..pkt1_base + offset_of!(AerogpuCmdSetShaderConstantsI, vec4_count) + 4]
                .try_into()
                .unwrap()
        ),
        1
    );
    assert_eq!(
        i32::from_le_bytes(
            buf[pkt1_base + size_of::<AerogpuCmdSetShaderConstantsI>()
                ..pkt1_base + size_of::<AerogpuCmdSetShaderConstantsI>() + 4]
                .try_into()
                .unwrap()
        ),
        -1
    );

    let pkt2_base = pkt1_base + expected_sizes[1].1;
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt2_base + offset_of!(AerogpuCmdSetShaderConstantsB, bool_count)
                ..pkt2_base + offset_of!(AerogpuCmdSetShaderConstantsB, bool_count) + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    // Payload must be expanded to vec4<u32> per bool register.
    let payload_base = pkt2_base + size_of::<AerogpuCmdSetShaderConstantsB>();
    let expected_payload: [u32; 8] = [0, 0, 0, 0, 1, 1, 1, 1];
    for (i, expected) in expected_payload.into_iter().enumerate() {
        let off = payload_base + i * 4;
        assert_eq!(
            u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()),
            expected
        );
    }

    // Validate nested state structs have their byte-sized fields populated.
    let pre_blend_size: usize = expected_sizes[..6].iter().map(|(_, sz)| *sz).sum();
    let blend_base = pkt0_base + pre_blend_size;
    let color_write_mask_off = offset_of!(AerogpuCmdSetBlendState, state)
        + offset_of!(AerogpuBlendState, color_write_mask);
    assert_eq!(buf[blend_base + color_write_mask_off], 0xF);
    let src_factor_alpha_off = offset_of!(AerogpuCmdSetBlendState, state)
        + offset_of!(AerogpuBlendState, src_factor_alpha);
    assert_eq!(
        u32::from_le_bytes(
            buf[blend_base + src_factor_alpha_off..blend_base + src_factor_alpha_off + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuBlendFactor::SrcAlpha as u32
    );
    let dst_factor_alpha_off = offset_of!(AerogpuCmdSetBlendState, state)
        + offset_of!(AerogpuBlendState, dst_factor_alpha);
    assert_eq!(
        u32::from_le_bytes(
            buf[blend_base + dst_factor_alpha_off..blend_base + dst_factor_alpha_off + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuBlendFactor::InvSrcAlpha as u32
    );
    let blend_op_alpha_off =
        offset_of!(AerogpuCmdSetBlendState, state) + offset_of!(AerogpuBlendState, blend_op_alpha);
    assert_eq!(
        u32::from_le_bytes(
            buf[blend_base + blend_op_alpha_off..blend_base + blend_op_alpha_off + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuBlendOp::Subtract as u32
    );
    let constant_base = offset_of!(AerogpuCmdSetBlendState, state)
        + offset_of!(AerogpuBlendState, blend_constant_rgba_f32);
    let expected_constant = [0.25f32, 0.5, 0.75, 1.0];
    for (i, &c) in expected_constant.iter().enumerate() {
        assert_eq!(
            u32::from_le_bytes(
                buf[blend_base + constant_base + i * 4..blend_base + constant_base + i * 4 + 4]
                    .try_into()
                    .unwrap()
            ),
            c.to_bits()
        );
    }
    let sample_mask_off =
        offset_of!(AerogpuCmdSetBlendState, state) + offset_of!(AerogpuBlendState, sample_mask);
    assert_eq!(
        u32::from_le_bytes(
            buf[blend_base + sample_mask_off..blend_base + sample_mask_off + 4]
                .try_into()
                .unwrap()
        ),
        0x00FF_FF00
    );

    let depth_base = blend_base + expected_sizes[6].1;
    let stencil_read_mask_off = offset_of!(AerogpuCmdSetDepthStencilState, state)
        + offset_of!(AerogpuDepthStencilState, stencil_read_mask);
    let stencil_write_mask_off = offset_of!(AerogpuCmdSetDepthStencilState, state)
        + offset_of!(AerogpuDepthStencilState, stencil_write_mask);
    assert_eq!(buf[depth_base + stencil_read_mask_off], 0xAA);
    assert_eq!(buf[depth_base + stencil_write_mask_off], 0xBB);

    let rast_base = depth_base + expected_sizes[7].1;
    assert_eq!(
        i32::from_le_bytes(
            buf[rast_base
                + offset_of!(AerogpuCmdSetRasterizerState, state)
                + offset_of!(AerogpuRasterizerState, depth_bias)
                ..rast_base
                    + offset_of!(AerogpuCmdSetRasterizerState, state)
                    + offset_of!(AerogpuRasterizerState, depth_bias)
                    + 4]
                .try_into()
                .unwrap()
        ),
        -1
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[rast_base
                + offset_of!(AerogpuCmdSetRasterizerState, state)
                + offset_of!(AerogpuRasterizerState, flags)
                ..rast_base
                    + offset_of!(AerogpuCmdSetRasterizerState, state)
                    + offset_of!(AerogpuRasterizerState, flags)
                    + 4]
                .try_into()
                .unwrap()
        ),
        AEROGPU_RASTERIZER_FLAG_DEPTH_CLIP_DISABLE,
        "depth_clip_disable flag"
    );
}

#[test]
fn cmd_writer_emits_copy_packets() {
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuCmdCopyBuffer, AerogpuCmdCopyTexture2d, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    };

    let mut w = AerogpuCmdWriter::new();
    w.copy_buffer_writeback_dst(1, 2, 4, 8, 12);
    w.copy_texture2d_writeback_dst(10, 11, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8);
    w.flush();

    let buf = w.finish();
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    let hdr0 = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode0 = hdr0.opcode;
    let size0 = hdr0.size_bytes as usize;
    assert_eq!(opcode0, AerogpuCmdOpcode::CopyBuffer as u32);
    assert_eq!(size0, size_of::<AerogpuCmdCopyBuffer>());

    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdCopyBuffer, dst_buffer)
                ..cursor + offset_of!(AerogpuCmdCopyBuffer, dst_buffer) + 4]
                .try_into()
                .unwrap()
        ),
        1
    );
    assert_eq!(
        u64::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdCopyBuffer, size_bytes)
                ..cursor + offset_of!(AerogpuCmdCopyBuffer, size_bytes) + 8]
                .try_into()
                .unwrap()
        ),
        12
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdCopyBuffer, flags)
                ..cursor + offset_of!(AerogpuCmdCopyBuffer, flags) + 4]
                .try_into()
                .unwrap()
        ),
        AEROGPU_COPY_FLAG_WRITEBACK_DST
    );

    cursor += size0;

    let hdr1 = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode1 = hdr1.opcode;
    let size1 = hdr1.size_bytes as usize;
    assert_eq!(opcode1, AerogpuCmdOpcode::CopyTexture2d as u32);
    assert_eq!(size1, size_of::<AerogpuCmdCopyTexture2d>());

    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdCopyTexture2d, width)
                ..cursor + offset_of!(AerogpuCmdCopyTexture2d, width) + 4]
                .try_into()
                .unwrap()
        ),
        7
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdCopyTexture2d, height)
                ..cursor + offset_of!(AerogpuCmdCopyTexture2d, height) + 4]
                .try_into()
                .unwrap()
        ),
        8
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdCopyTexture2d, flags)
                ..cursor + offset_of!(AerogpuCmdCopyTexture2d, flags) + 4]
                .try_into()
                .unwrap()
        ),
        AEROGPU_COPY_FLAG_WRITEBACK_DST
    );

    cursor += size1;

    let hdr2 = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode2 = hdr2.opcode;
    let size2 = hdr2.size_bytes as usize;
    assert_eq!(opcode2, AerogpuCmdOpcode::Flush as u32);
    assert_eq!(
        size2,
        size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdFlush>()
    );

    cursor += size2;
    assert_eq!(cursor, buf.len());
}

#[test]
fn cmd_writer_emits_stage_ex_and_bind_shaders_ex_packets() {
    use aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStageEx;

    let mut w = AerogpuCmdWriter::new();

    w.bind_shaders_ex(1, 2, 3, 4, 5, 6);
    w.create_shader_dxbc_ex(7, AerogpuShaderStageEx::Geometry, &[0xAA, 0xBB, 0xCC]);
    w.set_texture_ex(AerogpuShaderStageEx::Hull, 9, 10);

    let buf = w.finish();
    assert!(buf.len() >= AerogpuCmdStreamHeader::SIZE_BYTES);
    assert_eq!(buf.len() % 4, 0, "stream must remain 4-byte aligned");

    let stream = decode_cmd_stream_header_le(&buf).unwrap();
    assert_eq!(stream.size_bytes as usize, buf.len());

    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // BIND_SHADERS_EX
    let hdr0 = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode0 = hdr0.opcode;
    let size0 = hdr0.size_bytes as usize;
    assert_eq!(opcode0, AerogpuCmdOpcode::BindShaders as u32);
    assert_eq!(size0, 24 + 12);
    assert_eq!(size0 % 4, 0);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdBindShaders, vs)
                ..cursor + offset_of!(AerogpuCmdBindShaders, vs) + 4]
                .try_into()
                .unwrap()
        ),
        1
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdBindShaders, ps)
                ..cursor + offset_of!(AerogpuCmdBindShaders, ps) + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdBindShaders, cs)
                ..cursor + offset_of!(AerogpuCmdBindShaders, cs) + 4]
                .try_into()
                .unwrap()
        ),
        3
    );

    // Trailing GS/HS/DS u32s.
    let trailing_base = cursor + size_of::<AerogpuCmdBindShaders>();
    assert_eq!(
        u32::from_le_bytes(buf[trailing_base..trailing_base + 4].try_into().unwrap()),
        4
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[trailing_base + 4..trailing_base + 8]
                .try_into()
                .unwrap()
        ),
        5
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[trailing_base + 8..trailing_base + 12]
                .try_into()
                .unwrap()
        ),
        6
    );

    cursor += size0;

    // CREATE_SHADER_DXBC_EX (stage_ex stored in reserved0; stage set to Compute for fwd-compat).
    let hdr1 = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode1 = hdr1.opcode;
    let size1 = hdr1.size_bytes as usize;
    assert_eq!(opcode1, AerogpuCmdOpcode::CreateShaderDxbc as u32);
    assert_eq!(size1 % 4, 0);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdCreateShaderDxbc, stage)
                ..cursor + offset_of!(AerogpuCmdCreateShaderDxbc, stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Compute as u32
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdCreateShaderDxbc, reserved0)
                ..cursor + offset_of!(AerogpuCmdCreateShaderDxbc, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStageEx::Geometry as u32
    );

    cursor += size1;

    // SET_TEXTURE_EX (shader_stage_ex stored in reserved0; shader_stage set to Compute for fwd-compat).
    let hdr2 = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
    let opcode2 = hdr2.opcode;
    let size2 = hdr2.size_bytes as usize;
    assert_eq!(opcode2, AerogpuCmdOpcode::SetTexture as u32);
    assert_eq!(size2, size_of::<AerogpuCmdSetTexture>());
    assert_eq!(size2 % 4, 0);
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetTexture, shader_stage)
                ..cursor + offset_of!(AerogpuCmdSetTexture, shader_stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Compute as u32
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[cursor + offset_of!(AerogpuCmdSetTexture, reserved0)
                ..cursor + offset_of!(AerogpuCmdSetTexture, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStageEx::Hull as u32
    );

    cursor += size2;
    assert_eq!(cursor, buf.len());
}

#[test]
fn cmd_writer_emits_d3d11_binding_table_packets() {
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuCmdCreateSampler, AerogpuCmdDestroySampler, AerogpuCmdSetConstantBuffers,
        AerogpuCmdSetSamplers, AerogpuConstantBufferBinding, AerogpuHandle,
        AerogpuSamplerAddressMode, AerogpuSamplerFilter,
    };

    let mut w = AerogpuCmdWriter::new();
    w.create_sampler(
        40,
        AerogpuSamplerFilter::Linear,
        AerogpuSamplerAddressMode::Repeat,
        AerogpuSamplerAddressMode::ClampToEdge,
        AerogpuSamplerAddressMode::MirrorRepeat,
    );
    w.set_samplers(AerogpuShaderStage::Pixel, 2, &[40, 41]);
    w.set_sampler(AerogpuShaderStage::Vertex, 0, 40);

    let cb_bindings = [
        AerogpuConstantBufferBinding {
            buffer: 99,
            offset_bytes: 16,
            size_bytes: 64,
            reserved0: 0,
        },
        AerogpuConstantBufferBinding {
            buffer: 0,
            offset_bytes: 0,
            size_bytes: 0,
            reserved0: 0,
        },
    ];
    w.set_constant_buffers(AerogpuShaderStage::Pixel, 4, &cb_bindings);
    w.set_constant_buffer(AerogpuShaderStage::Vertex, 1, 77, 0, 128);
    w.destroy_sampler(40);
    w.flush();

    let buf = w.finish();

    let expected_sizes: &[(u32, usize)] = &[
        (
            AerogpuCmdOpcode::CreateSampler as u32,
            size_of::<AerogpuCmdCreateSampler>(),
        ),
        (
            AerogpuCmdOpcode::SetSamplers as u32,
            size_of::<AerogpuCmdSetSamplers>() + 2 * size_of::<AerogpuHandle>(),
        ),
        (
            AerogpuCmdOpcode::SetSamplers as u32,
            size_of::<AerogpuCmdSetSamplers>() + size_of::<AerogpuHandle>(),
        ),
        (
            AerogpuCmdOpcode::SetConstantBuffers as u32,
            size_of::<AerogpuCmdSetConstantBuffers>()
                + 2 * size_of::<AerogpuConstantBufferBinding>(),
        ),
        (
            AerogpuCmdOpcode::SetConstantBuffers as u32,
            size_of::<AerogpuCmdSetConstantBuffers>() + size_of::<AerogpuConstantBufferBinding>(),
        ),
        (
            AerogpuCmdOpcode::DestroySampler as u32,
            size_of::<AerogpuCmdDestroySampler>(),
        ),
        (
            AerogpuCmdOpcode::Flush as u32,
            size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdFlush>(),
        ),
    ];

    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;
    for &(expected_opcode, expected_size) in expected_sizes {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes;
        assert_eq!(opcode, expected_opcode);
        assert_eq!(size_bytes as usize, expected_size);
        cursor += expected_size;
    }
    assert_eq!(cursor, buf.len());

    // Validate CREATE_SAMPLER fields.
    let create_sampler_base = AerogpuCmdStreamHeader::SIZE_BYTES;
    assert_eq!(
        u32::from_le_bytes(
            buf[create_sampler_base + offset_of!(AerogpuCmdCreateSampler, sampler_handle)
                ..create_sampler_base + offset_of!(AerogpuCmdCreateSampler, sampler_handle) + 4]
                .try_into()
                .unwrap()
        ),
        40
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[create_sampler_base + offset_of!(AerogpuCmdCreateSampler, filter)
                ..create_sampler_base + offset_of!(AerogpuCmdCreateSampler, filter) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuSamplerFilter::Linear as u32
    );

    // Validate sampler count and payload for the SET_SAMPLERS packet.
    let set_samplers_base = AerogpuCmdStreamHeader::SIZE_BYTES + expected_sizes[0].1;
    assert_eq!(
        u32::from_le_bytes(
            buf[set_samplers_base + offset_of!(AerogpuCmdSetSamplers, start_slot)
                ..set_samplers_base + offset_of!(AerogpuCmdSetSamplers, start_slot) + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_samplers_base + offset_of!(AerogpuCmdSetSamplers, shader_stage)
                ..set_samplers_base + offset_of!(AerogpuCmdSetSamplers, shader_stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Pixel as u32
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_samplers_base + offset_of!(AerogpuCmdSetSamplers, sampler_count)
                ..set_samplers_base + offset_of!(AerogpuCmdSetSamplers, sampler_count) + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_samplers_base + size_of::<AerogpuCmdSetSamplers>()
                ..set_samplers_base + size_of::<AerogpuCmdSetSamplers>() + 4]
                .try_into()
                .unwrap()
        ),
        40
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_samplers_base + size_of::<AerogpuCmdSetSamplers>() + size_of::<AerogpuHandle>()
                ..set_samplers_base
                    + size_of::<AerogpuCmdSetSamplers>()
                    + 2 * size_of::<AerogpuHandle>()]
                .try_into()
                .unwrap()
        ),
        41
    );

    // Validate the single-slot SET_SAMPLER packet emitted by `set_sampler`.
    let set_sampler_base = set_samplers_base + expected_sizes[1].1;
    assert_eq!(
        u32::from_le_bytes(
            buf[set_sampler_base + offset_of!(AerogpuCmdSetSamplers, shader_stage)
                ..set_sampler_base + offset_of!(AerogpuCmdSetSamplers, shader_stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Vertex as u32
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_sampler_base + offset_of!(AerogpuCmdSetSamplers, start_slot)
                ..set_sampler_base + offset_of!(AerogpuCmdSetSamplers, start_slot) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_sampler_base + offset_of!(AerogpuCmdSetSamplers, sampler_count)
                ..set_sampler_base + offset_of!(AerogpuCmdSetSamplers, sampler_count) + 4]
                .try_into()
                .unwrap()
        ),
        1
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_sampler_base + size_of::<AerogpuCmdSetSamplers>()
                ..set_sampler_base + size_of::<AerogpuCmdSetSamplers>() + 4]
                .try_into()
                .unwrap()
        ),
        40
    );

    // Validate constant-buffer count and payload for the SET_CONSTANT_BUFFERS packet.
    let set_cbs_base = set_samplers_base + expected_sizes[1].1 + expected_sizes[2].1;
    assert_eq!(
        u32::from_le_bytes(
            buf[set_cbs_base + offset_of!(AerogpuCmdSetConstantBuffers, start_slot)
                ..set_cbs_base + offset_of!(AerogpuCmdSetConstantBuffers, start_slot) + 4]
                .try_into()
                .unwrap()
        ),
        4
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_cbs_base + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count)
                ..set_cbs_base + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count) + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_cbs_base + size_of::<AerogpuCmdSetConstantBuffers>()
                ..set_cbs_base + size_of::<AerogpuCmdSetConstantBuffers>() + 4]
                .try_into()
                .unwrap()
        ),
        99
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_cbs_base
                + size_of::<AerogpuCmdSetConstantBuffers>()
                + offset_of!(AerogpuConstantBufferBinding, offset_bytes)
                ..set_cbs_base
                    + size_of::<AerogpuCmdSetConstantBuffers>()
                    + offset_of!(AerogpuConstantBufferBinding, offset_bytes)
                    + 4]
                .try_into()
                .unwrap()
        ),
        16
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_cbs_base
                + size_of::<AerogpuCmdSetConstantBuffers>()
                + offset_of!(AerogpuConstantBufferBinding, size_bytes)
                ..set_cbs_base
                    + size_of::<AerogpuCmdSetConstantBuffers>()
                    + offset_of!(AerogpuConstantBufferBinding, size_bytes)
                    + 4]
                .try_into()
                .unwrap()
        ),
        64
    );

    // Second binding is all zeros (unbind).
    let cb1_base = set_cbs_base
        + size_of::<AerogpuCmdSetConstantBuffers>()
        + size_of::<AerogpuConstantBufferBinding>();
    assert_eq!(
        u32::from_le_bytes(buf[cb1_base..cb1_base + 4].try_into().unwrap()),
        0
    );

    // Validate the single-slot SET_CONSTANT_BUFFERS packet emitted by `set_constant_buffer`.
    let set_cb_single_base = set_cbs_base + expected_sizes[3].1;
    assert_eq!(
        u32::from_le_bytes(
            buf[set_cb_single_base + offset_of!(AerogpuCmdSetConstantBuffers, start_slot)
                ..set_cb_single_base + offset_of!(AerogpuCmdSetConstantBuffers, start_slot) + 4]
                .try_into()
                .unwrap()
        ),
        1
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[set_cb_single_base + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count)
                ..set_cb_single_base + offset_of!(AerogpuCmdSetConstantBuffers, buffer_count) + 4]
                .try_into()
                .unwrap()
        ),
        1
    );
    let binding0_base = set_cb_single_base + size_of::<AerogpuCmdSetConstantBuffers>();
    assert_eq!(
        u32::from_le_bytes(buf[binding0_base..binding0_base + 4].try_into().unwrap()),
        77
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[binding0_base + offset_of!(AerogpuConstantBufferBinding, size_bytes)
                ..binding0_base + offset_of!(AerogpuConstantBufferBinding, size_bytes) + 4]
                .try_into()
                .unwrap()
        ),
        128
    );
}

#[test]
fn cmd_writer_emits_stage_ex_binding_packets() {
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuCmdSetConstantBuffers, AerogpuCmdSetSamplers, AerogpuCmdSetShaderResourceBuffers,
        AerogpuCmdSetUnorderedAccessBuffers, AerogpuConstantBufferBinding, AerogpuHandle,
        AerogpuShaderResourceBufferBinding, AerogpuShaderStageEx,
        AerogpuUnorderedAccessBufferBinding,
    };

    let mut w = AerogpuCmdWriter::new();

    // Legacy stage bindings (reserved0 must remain 0).
    w.set_texture(AerogpuShaderStage::Pixel, 0, 99);
    w.set_samplers(AerogpuShaderStage::Pixel, 0, &[1]);

    let cb_bindings = [AerogpuConstantBufferBinding {
        buffer: 77,
        offset_bytes: 16,
        size_bytes: 64,
        reserved0: 0,
    }];
    w.set_constant_buffers(AerogpuShaderStage::Vertex, 1, &cb_bindings);
    let srv_bindings = [AerogpuShaderResourceBufferBinding {
        buffer: 88,
        offset_bytes: 0,
        size_bytes: 32,
        reserved0: 0,
    }];
    w.set_shader_resource_buffers(AerogpuShaderStage::Pixel, 0, &srv_bindings);
    let uav_bindings = [AerogpuUnorderedAccessBufferBinding {
        buffer: 99,
        offset_bytes: 4,
        size_bytes: 16,
        initial_count: 0,
    }];
    w.set_unordered_access_buffers(AerogpuShaderStage::Compute, 1, &uav_bindings);
    w.set_shader_constants_f(AerogpuShaderStage::Vertex, 0, &[0.0, 0.0, 0.0, 0.0]);

    // Extended stage bindings (shader_stage must be Compute, reserved0 = stage_ex).
    w.set_texture_stage_ex(
        AerogpuShaderStage::Pixel,
        Some(AerogpuShaderStageEx::Geometry),
        1,
        100,
    );
    w.set_samplers_stage_ex(
        AerogpuShaderStage::Pixel,
        Some(AerogpuShaderStageEx::Hull),
        2,
        &[10, 11],
    );
    w.set_constant_buffers_stage_ex(
        AerogpuShaderStage::Vertex,
        Some(AerogpuShaderStageEx::Domain),
        3,
        &cb_bindings,
    );
    w.set_shader_resource_buffers_stage_ex(
        AerogpuShaderStage::Pixel,
        Some(AerogpuShaderStageEx::Hull),
        4,
        &srv_bindings,
    );
    w.set_unordered_access_buffers_stage_ex(
        AerogpuShaderStage::Compute,
        Some(AerogpuShaderStageEx::Domain),
        5,
        &uav_bindings,
    );
    w.set_shader_constants_f_stage_ex(
        AerogpuShaderStage::Vertex,
        Some(AerogpuShaderStageEx::Geometry),
        6,
        &[1.0, 2.0, 3.0, 4.0],
    );

    w.flush();

    let buf = w.finish();
    let mut cursor = AerogpuCmdStreamHeader::SIZE_BYTES;

    // SET_TEXTURE (legacy).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetTexture as u32);
        assert_eq!(size_bytes, size_of::<AerogpuCmdSetTexture>());
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetTexture, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetTexture, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Pixel as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetTexture, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetTexture, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        cursor += size_bytes;
    }

    // SET_SAMPLERS (legacy).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetSamplers as u32);
        assert_eq!(
            size_bytes,
            size_of::<AerogpuCmdSetSamplers>() + size_of::<AerogpuHandle>()
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetSamplers, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetSamplers, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Pixel as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetSamplers, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetSamplers, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        cursor += size_bytes;
    }

    // SET_CONSTANT_BUFFERS (legacy).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetConstantBuffers as u32);
        assert_eq!(
            size_bytes,
            size_of::<AerogpuCmdSetConstantBuffers>() + size_of::<AerogpuConstantBufferBinding>()
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetConstantBuffers, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetConstantBuffers, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Vertex as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        cursor += size_bytes;
    }

    // SET_SHADER_RESOURCE_BUFFERS (legacy).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetShaderResourceBuffers as u32);
        assert_eq!(
            size_bytes,
            size_of::<AerogpuCmdSetShaderResourceBuffers>()
                + size_of::<AerogpuShaderResourceBufferBinding>()
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Pixel as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        cursor += size_bytes;
    }

    // SET_UNORDERED_ACCESS_BUFFERS (legacy).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32);
        assert_eq!(
            size_bytes,
            size_of::<AerogpuCmdSetUnorderedAccessBuffers>()
                + size_of::<AerogpuUnorderedAccessBufferBinding>()
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Compute as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        cursor += size_bytes;
    }

    // SET_SHADER_CONSTANTS_F (legacy).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetShaderConstantsF as u32);
        assert_eq!(size_bytes, size_of::<AerogpuCmdSetShaderConstantsF>() + 16);
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetShaderConstantsF, stage)
                    ..cursor + offset_of!(AerogpuCmdSetShaderConstantsF, stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Vertex as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        cursor += size_bytes;
    }

    // SET_TEXTURE (stage_ex).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetTexture as u32);
        assert_eq!(size_bytes, size_of::<AerogpuCmdSetTexture>());
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetTexture, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetTexture, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Compute as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetTexture, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetTexture, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStageEx::Geometry as u32
        );
        cursor += size_bytes;
    }

    // SET_SAMPLERS (stage_ex).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetSamplers as u32);
        assert_eq!(
            size_bytes,
            size_of::<AerogpuCmdSetSamplers>() + 2 * size_of::<AerogpuHandle>()
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetSamplers, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetSamplers, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Compute as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetSamplers, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetSamplers, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStageEx::Hull as u32
        );
        cursor += size_bytes;
    }

    // SET_CONSTANT_BUFFERS (stage_ex).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetConstantBuffers as u32);
        assert_eq!(
            size_bytes,
            size_of::<AerogpuCmdSetConstantBuffers>() + size_of::<AerogpuConstantBufferBinding>()
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetConstantBuffers, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetConstantBuffers, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Compute as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetConstantBuffers, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStageEx::Domain as u32
        );
        cursor += size_bytes;
    }

    // SET_SHADER_RESOURCE_BUFFERS (stage_ex).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetShaderResourceBuffers as u32);
        assert_eq!(
            size_bytes,
            size_of::<AerogpuCmdSetShaderResourceBuffers>()
                + size_of::<AerogpuShaderResourceBufferBinding>()
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Compute as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetShaderResourceBuffers, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStageEx::Hull as u32
        );
        cursor += size_bytes;
    }

    // SET_UNORDERED_ACCESS_BUFFERS (stage_ex).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetUnorderedAccessBuffers as u32);
        assert_eq!(
            size_bytes,
            size_of::<AerogpuCmdSetUnorderedAccessBuffers>()
                + size_of::<AerogpuUnorderedAccessBufferBinding>()
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, shader_stage)
                    ..cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, shader_stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Compute as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetUnorderedAccessBuffers, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStageEx::Domain as u32
        );
        cursor += size_bytes;
    }

    // SET_SHADER_CONSTANTS_F (stage_ex).
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::SetShaderConstantsF as u32);
        assert_eq!(size_bytes, size_of::<AerogpuCmdSetShaderConstantsF>() + 16);
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetShaderConstantsF, stage)
                    ..cursor + offset_of!(AerogpuCmdSetShaderConstantsF, stage) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStage::Compute as u32
        );
        assert_eq!(
            u32::from_le_bytes(
                buf[cursor + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0)
                    ..cursor + offset_of!(AerogpuCmdSetShaderConstantsF, reserved0) + 4]
                    .try_into()
                    .unwrap()
            ),
            AerogpuShaderStageEx::Geometry as u32
        );
        cursor += size_bytes;
    }

    // FLUSH.
    {
        let hdr = decode_cmd_hdr_le(&buf[cursor..]).unwrap();
        let opcode = hdr.opcode;
        let size_bytes = hdr.size_bytes as usize;
        assert_eq!(opcode, AerogpuCmdOpcode::Flush as u32);
        cursor += size_bytes;
    }

    assert_eq!(cursor, buf.len());
}

#[test]
fn cmd_writer_emits_create_shader_dxbc_ex_with_stage_ex_and_padding() {
    use aero_protocol::aerogpu::aerogpu_cmd::{AerogpuCmdCreateShaderDxbc, AerogpuShaderStageEx};

    // 5 bytes -> requires 3 bytes of 4-byte padding after the payload.
    let dxbc = [0xAAu8, 0xBB, 0xCC, 0xDD, 0xEE];

    let mut w = AerogpuCmdWriter::new();
    w.create_shader_dxbc_ex(7, AerogpuShaderStageEx::Geometry, &dxbc);
    let buf = w.finish();

    let pkt_base = AerogpuCmdStreamHeader::SIZE_BYTES;
    let hdr = decode_cmd_hdr_le(&buf[pkt_base..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::CreateShaderDxbc as u32);

    let expected_size = align_up(size_of::<AerogpuCmdCreateShaderDxbc>() + dxbc.len(), 4);
    assert_eq!(size_bytes as usize, expected_size);

    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdCreateShaderDxbc, stage)
                ..pkt_base + offset_of!(AerogpuCmdCreateShaderDxbc, stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Compute as u32,
        "stage is encoded as COMPUTE when using stage_ex"
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdCreateShaderDxbc, reserved0)
                ..pkt_base + offset_of!(AerogpuCmdCreateShaderDxbc, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStageEx::Geometry as u32,
        "reserved0 carries stage_ex"
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes)
                ..pkt_base + offset_of!(AerogpuCmdCreateShaderDxbc, dxbc_size_bytes) + 4]
                .try_into()
                .unwrap()
        ),
        dxbc.len() as u32
    );

    let payload_base = pkt_base + size_of::<AerogpuCmdCreateShaderDxbc>();
    assert_eq!(&buf[payload_base..payload_base + dxbc.len()], &dxbc);

    // Validate 4-byte padding is present and zeroed.
    let unpadded_end = payload_base + dxbc.len();
    let padded_end = pkt_base + expected_size;
    assert_eq!(padded_end - unpadded_end, 3);
    assert!(buf[unpadded_end..padded_end].iter().all(|&b| b == 0));
}

#[test]
fn cmd_writer_emits_bind_shaders_ex_with_trailing_gs_hs_ds_handles() {
    use aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdBindShaders;

    let mut w = AerogpuCmdWriter::new();
    w.bind_shaders_ex(1, 2, 3, 4, 5, 6);
    let buf = w.finish();

    let pkt_base = AerogpuCmdStreamHeader::SIZE_BYTES;
    let hdr = decode_cmd_hdr_le(&buf[pkt_base..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes;
    assert_eq!(opcode, AerogpuCmdOpcode::BindShaders as u32);

    assert_eq!(size_bytes as usize, AerogpuCmdBindShaders::EX_SIZE_BYTES);

    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdBindShaders, vs)
                ..pkt_base + offset_of!(AerogpuCmdBindShaders, vs) + 4]
                .try_into()
                .unwrap()
        ),
        1
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdBindShaders, ps)
                ..pkt_base + offset_of!(AerogpuCmdBindShaders, ps) + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdBindShaders, cs)
                ..pkt_base + offset_of!(AerogpuCmdBindShaders, cs) + 4]
                .try_into()
                .unwrap()
        ),
        3
    );
    // Append-only extension keeps reserved0=0; trailing `{gs,hs,ds}` are authoritative.
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdBindShaders, reserved0)
                ..pkt_base + offset_of!(AerogpuCmdBindShaders, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );

    let ext_base = pkt_base + AerogpuCmdBindShaders::SIZE_BYTES;
    assert_eq!(
        u32::from_le_bytes(buf[ext_base..ext_base + 4].try_into().unwrap()),
        4
    );
    assert_eq!(
        u32::from_le_bytes(buf[ext_base + 4..ext_base + 8].try_into().unwrap()),
        5
    );
    assert_eq!(
        u32::from_le_bytes(buf[ext_base + 8..ext_base + 12].try_into().unwrap()),
        6
    );
}

#[test]
fn cmd_writer_emits_set_shader_constants_i_with_vec4_aligned_i32_payload() {
    let data: [i32; 8] = [1, -2, 3, -4, 5, 6, -7, 8];

    let mut w = AerogpuCmdWriter::new();
    w.set_shader_constants_i(AerogpuShaderStage::Pixel, 5, &data);
    let buf = w.finish();

    let pkt_base = AerogpuCmdStreamHeader::SIZE_BYTES;
    let hdr = decode_cmd_hdr_le(&buf[pkt_base..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes as usize;
    assert_eq!(opcode, AerogpuCmdOpcode::SetShaderConstantsI as u32);
    assert_eq!(
        size_bytes,
        size_of::<AerogpuCmdSetShaderConstantsI>() + data.len() * 4
    );

    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdSetShaderConstantsI, stage)
                ..pkt_base + offset_of!(AerogpuCmdSetShaderConstantsI, stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Pixel as u32
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdSetShaderConstantsI, start_register)
                ..pkt_base + offset_of!(AerogpuCmdSetShaderConstantsI, start_register) + 4]
                .try_into()
                .unwrap()
        ),
        5
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdSetShaderConstantsI, vec4_count)
                ..pkt_base + offset_of!(AerogpuCmdSetShaderConstantsI, vec4_count) + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdSetShaderConstantsI, reserved0)
                ..pkt_base + offset_of!(AerogpuCmdSetShaderConstantsI, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );

    let payload_base = pkt_base + size_of::<AerogpuCmdSetShaderConstantsI>();
    for (i, &expected) in data.iter().enumerate() {
        let off = payload_base + i * 4;
        let found = i32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        assert_eq!(found, expected);
    }
}

#[test]
fn cmd_writer_emits_set_shader_constants_b_as_vec4_u32_per_register() {
    let data: [u32; 2] = [0, 2];

    let mut w = AerogpuCmdWriter::new();
    w.set_shader_constants_b(AerogpuShaderStage::Vertex, 7, &data);
    let buf = w.finish();

    let pkt_base = AerogpuCmdStreamHeader::SIZE_BYTES;
    let hdr = decode_cmd_hdr_le(&buf[pkt_base..]).unwrap();
    let opcode = hdr.opcode;
    let size_bytes = hdr.size_bytes as usize;
    assert_eq!(opcode, AerogpuCmdOpcode::SetShaderConstantsB as u32);
    assert_eq!(
        size_bytes,
        size_of::<AerogpuCmdSetShaderConstantsB>() + data.len() * 16
    );

    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdSetShaderConstantsB, stage)
                ..pkt_base + offset_of!(AerogpuCmdSetShaderConstantsB, stage) + 4]
                .try_into()
                .unwrap()
        ),
        AerogpuShaderStage::Vertex as u32
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdSetShaderConstantsB, start_register)
                ..pkt_base + offset_of!(AerogpuCmdSetShaderConstantsB, start_register) + 4]
                .try_into()
                .unwrap()
        ),
        7
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdSetShaderConstantsB, bool_count)
                ..pkt_base + offset_of!(AerogpuCmdSetShaderConstantsB, bool_count) + 4]
                .try_into()
                .unwrap()
        ),
        2
    );
    assert_eq!(
        u32::from_le_bytes(
            buf[pkt_base + offset_of!(AerogpuCmdSetShaderConstantsB, reserved0)
                ..pkt_base + offset_of!(AerogpuCmdSetShaderConstantsB, reserved0) + 4]
                .try_into()
                .unwrap()
        ),
        0
    );

    // Payload is `bool_count` registers, each encoded as a vec4<u32> (16 bytes) with the scalar
    // bool replicated across lanes.
    let payload_base = pkt_base + size_of::<AerogpuCmdSetShaderConstantsB>();
    for (i, expected) in [0u32, 1u32].into_iter().enumerate() {
        let off = payload_base + i * 16;
        assert_eq!(
            u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()),
            expected
        );
        assert_eq!(
            u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()),
            expected
        );
        assert_eq!(
            u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap()),
            expected
        );
        assert_eq!(
            u32::from_le_bytes(buf[off + 12..off + 16].try_into().unwrap()),
            expected
        );
    }
}
