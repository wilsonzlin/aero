use core::mem::{offset_of, size_of};

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_hdr_le, decode_cmd_stream_header_le, AerogpuBlendFactor, AerogpuBlendOp, AerogpuCmdCreateInputLayout,
    AerogpuCmdCreateShaderDxbc, AerogpuCmdExportSharedSurface, AerogpuCmdHdr, AerogpuCmdImportSharedSurface,
    AerogpuCmdOpcode, AerogpuCmdPresentEx, AerogpuCmdSetShaderConstantsF, AerogpuCmdSetTexture, AerogpuCmdStreamHeader,
    AerogpuCmdUploadResource, AerogpuCompareFunc, AerogpuCullMode, AerogpuFillMode, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

fn align_up(v: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two());
    (v + (a - 1)) & !(a - 1)
}

#[test]
fn cmd_writer_emits_aligned_packets_and_updates_stream_size() {
    let mut w = AerogpuCmdWriter::new();

    w.create_buffer(1, 0xDEAD_BEEF, 1024, 0, 0);
    w.create_shader_dxbc(2, aero_protocol::aerogpu::aerogpu_cmd::AerogpuShaderStage::Vertex, &[0xAA, 0xBB, 0xCC]);
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
    assert_eq!(cursor, buf.len(), "packet walk must land exactly on end of stream");

    let expected_sizes: &[(u32, usize)] = &[
        (AerogpuCmdOpcode::CreateBuffer as u32, size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdCreateBuffer>()),
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
        (AerogpuCmdOpcode::Draw as u32, size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdDraw>()),
        (AerogpuCmdOpcode::Flush as u32, size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdFlush>()),
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
                ..input_layout_pkt_base + offset_of!(AerogpuCmdCreateInputLayout, blob_size_bytes) + 4]
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
fn cmd_writer_emits_pipeline_and_binding_packets() {
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuBlendState, AerogpuCmdSetBlendState, AerogpuCmdSetDepthStencilState, AerogpuCmdSetRasterizerState,
        AerogpuCmdSetRenderState, AerogpuCmdSetSamplerState, AerogpuDepthStencilState, AerogpuRasterizerState,
    };

    let mut w = AerogpuCmdWriter::new();
    w.set_shader_constants_f(AerogpuShaderStage::Pixel, 4, &[1.0, 2.0, 3.0, 4.0]);
    w.set_texture(AerogpuShaderStage::Pixel, 0, 99);
    w.set_sampler_state(AerogpuShaderStage::Pixel, 0, 7, 42);
    w.set_render_state(10, 20);
    w.set_blend_state(
        true,
        AerogpuBlendFactor::One,
        AerogpuBlendFactor::Zero,
        AerogpuBlendOp::Add,
        0xF,
    );
    w.set_depth_stencil_state(true, true, AerogpuCompareFunc::LessEqual, false, 0xAA, 0xBB);
    w.set_rasterizer_state(
        AerogpuFillMode::Solid,
        AerogpuCullMode::Back,
        false,
        true,
        -1,
    );
    w.present_ex(0, 0, 0x1234_5678);
    w.export_shared_surface(55, 0x0102_0304_0506_0708);
    w.import_shared_surface(56, 0x0102_0304_0506_0708);
    w.flush();

    let buf = w.finish();

    let expected_sizes: &[(u32, usize)] = &[
        (
            AerogpuCmdOpcode::SetShaderConstantsF as u32,
            size_of::<AerogpuCmdSetShaderConstantsF>() + 16,
        ),
        (AerogpuCmdOpcode::SetTexture as u32, size_of::<AerogpuCmdSetTexture>()),
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
        (AerogpuCmdOpcode::Flush as u32, size_of::<aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdFlush>()),
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

    // Validate the variable-length shader constants packet.
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

    // Validate nested state structs have their byte-sized fields populated.
    let blend_base = pkt0_base + expected_sizes[0].1 + expected_sizes[1].1 + expected_sizes[2].1 + expected_sizes[3].1;
    let color_write_mask_off =
        offset_of!(AerogpuCmdSetBlendState, state) + offset_of!(AerogpuBlendState, color_write_mask);
    assert_eq!(buf[blend_base + color_write_mask_off], 0xF);

    let depth_base = blend_base + expected_sizes[4].1;
    let stencil_read_mask_off =
        offset_of!(AerogpuCmdSetDepthStencilState, state) + offset_of!(AerogpuDepthStencilState, stencil_read_mask);
    let stencil_write_mask_off =
        offset_of!(AerogpuCmdSetDepthStencilState, state) + offset_of!(AerogpuDepthStencilState, stencil_write_mask);
    assert_eq!(buf[depth_base + stencil_read_mask_off], 0xAA);
    assert_eq!(buf[depth_base + stencil_write_mask_off], 0xBB);

    let rast_base = depth_base + expected_sizes[5].1;
    assert_eq!(
        i32::from_le_bytes(
            buf[rast_base + offset_of!(AerogpuCmdSetRasterizerState, state) + offset_of!(AerogpuRasterizerState, depth_bias)
                ..rast_base
                    + offset_of!(AerogpuCmdSetRasterizerState, state)
                    + offset_of!(AerogpuRasterizerState, depth_bias)
                    + 4]
                .try_into()
                .unwrap()
        ),
        -1
    );
}
