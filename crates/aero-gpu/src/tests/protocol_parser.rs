use crate::{
    parse_cmd_stream, AeroGpuCmd, AeroGpuCmdStreamParseError, AeroGpuOpcode,
    AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

use crate::protocol::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn pad4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);
    pad4(out);

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert_eq!(size_bytes % 4, 0);
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

#[test]
fn protocol_parses_all_opcodes() {
    let debug_marker = b"mark";
    let upload_data = [1u8, 2, 3, 4, 5];
    let dxbc_bytes = [9u8, 8, 7, 6, 5];

    let constants_f32 = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
    let mut constants_bytes = Vec::new();
    for v in constants_f32 {
        constants_bytes.extend_from_slice(&v.to_le_bytes());
    }

    let mut ilay_blob = Vec::new();
    // aerogpu_input_layout_blob_header
    push_u32(&mut ilay_blob, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay_blob, 1); // version
    push_u32(&mut ilay_blob, 1); // element_count
    push_u32(&mut ilay_blob, 0); // reserved0
                                 // aerogpu_input_layout_element_dxgi (1 element)
    push_u32(&mut ilay_blob, 0x1234_5678); // semantic_name_hash
    push_u32(&mut ilay_blob, 0); // semantic_index
    push_u32(&mut ilay_blob, 28); // dxgi_format (opaque numeric)
    push_u32(&mut ilay_blob, 0); // input_slot
    push_u32(&mut ilay_blob, 0); // aligned_byte_offset
    push_u32(&mut ilay_blob, 0); // input_slot_class
    push_u32(&mut ilay_blob, 0); // instance_data_step_rate

    let mut expected_vb_bindings = Vec::new();
    // binding[0]
    push_u32(&mut expected_vb_bindings, 0xA0); // buffer
    push_u32(&mut expected_vb_bindings, 16); // stride_bytes
    push_u32(&mut expected_vb_bindings, 0); // offset_bytes
    push_u32(&mut expected_vb_bindings, 0); // reserved0
                                            // binding[1]
    push_u32(&mut expected_vb_bindings, 0xA1);
    push_u32(&mut expected_vb_bindings, 32);
    push_u32(&mut expected_vb_bindings, 64);
    push_u32(&mut expected_vb_bindings, 0);

    let stream = build_stream(|out| {
        emit_packet(out, AeroGpuOpcode::Nop as u32, |_| {});

        emit_packet(out, AeroGpuOpcode::DebugMarker as u32, |out| {
            out.extend_from_slice(debug_marker);
        });

        emit_packet(out, AeroGpuOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 0x10); // buffer_handle
            push_u32(out, 0x3); // usage_flags
            push_u64(out, 0x1122); // size_bytes
            push_u32(out, 0x5); // backing_alloc_id
            push_u32(out, 0x20); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 0x11); // texture_handle
            push_u32(out, 0x4); // usage_flags
            push_u32(out, 0x16); // format
            push_u32(out, 640); // width
            push_u32(out, 480); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 2560); // row_pitch_bytes
            push_u32(out, 2); // backing_alloc_id
            push_u32(out, 128); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::DestroyResource as u32, |out| {
            push_u32(out, 0x11); // resource_handle
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::ResourceDirtyRange as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1000); // offset_bytes
            push_u64(out, 0x200); // size_bytes
        });

        emit_packet(out, AeroGpuOpcode::UploadResource as u32, |out| {
            push_u32(out, 0x10); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x20); // offset_bytes
            push_u64(out, upload_data.len() as u64); // size_bytes
            out.extend_from_slice(&upload_data);
        });

        emit_packet(out, AeroGpuOpcode::CopyBuffer as u32, |out| {
            push_u32(out, 0x20); // dst_buffer
            push_u32(out, 0x21); // src_buffer
            push_u64(out, 0x1000); // dst_offset_bytes
            push_u64(out, 0x2000); // src_offset_bytes
            push_u64(out, 0x80); // size_bytes
            push_u32(out, 0x1); // flags
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::CopyTexture2d as u32, |out| {
            push_u32(out, 0x30); // dst_texture
            push_u32(out, 0x31); // src_texture
            push_u32(out, 1); // dst_mip_level
            push_u32(out, 2); // dst_array_layer
            push_u32(out, 3); // src_mip_level
            push_u32(out, 4); // src_array_layer
            push_u32(out, 5); // dst_x
            push_u32(out, 6); // dst_y
            push_u32(out, 7); // src_x
            push_u32(out, 8); // src_y
            push_u32(out, 9); // width
            push_u32(out, 10); // height
            push_u32(out, 0x2); // flags
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, 0x33); // shader_handle
            push_u32(out, 1); // stage
            push_u32(out, dxbc_bytes.len() as u32); // dxbc_size_bytes
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&dxbc_bytes);
        });

        emit_packet(out, AeroGpuOpcode::DestroyShader as u32, |out| {
            push_u32(out, 0x33); // shader_handle
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::BindShaders as u32, |out| {
            push_u32(out, 1); // vs
            push_u32(out, 2); // ps
            push_u32(out, 3); // cs
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::SetShaderConstantsF as u32, |out| {
            push_u32(out, 0); // stage
            push_u32(out, 4); // start_register
            push_u32(out, 2); // vec4_count
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&constants_bytes);
        });

        emit_packet(out, AeroGpuOpcode::CreateInputLayout as u32, |out| {
            push_u32(out, 0x44); // input_layout_handle
            push_u32(out, ilay_blob.len() as u32); // blob_size_bytes
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&ilay_blob);
        });

        emit_packet(out, AeroGpuOpcode::DestroyInputLayout as u32, |out| {
            push_u32(out, 0x44); // input_layout_handle
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::SetInputLayout as u32, |out| {
            push_u32(out, 0x44); // input_layout_handle
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::SetBlendState as u32, |out| {
            push_u32(out, 1); // enable
            push_u32(out, 2); // src_factor
            push_u32(out, 3); // dst_factor
            push_u32(out, 4); // blend_op
            out.push(0xF); // color_write_mask
            out.extend_from_slice(&[0u8; 3]); // reserved0[3]
        });

        emit_packet(out, AeroGpuOpcode::SetDepthStencilState as u32, |out| {
            push_u32(out, 1); // depth_enable
            push_u32(out, 1); // depth_write_enable
            push_u32(out, 2); // depth_func
            push_u32(out, 0); // stencil_enable
            out.push(0xAA); // stencil_read_mask
            out.push(0xBB); // stencil_write_mask
            out.extend_from_slice(&[0u8; 2]); // reserved0[2]
        });

        emit_packet(out, AeroGpuOpcode::SetRasterizerState as u32, |out| {
            push_u32(out, 0); // fill_mode
            push_u32(out, 1); // cull_mode
            push_u32(out, 1); // front_ccw
            push_u32(out, 0); // scissor_enable
            push_i32(out, -1); // depth_bias
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 2); // color_count
            push_u32(out, 0x99); // depth_stencil
                                 // colors[8]
            push_u32(out, 1);
            push_u32(out, 2);
            for _ in 2..8 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, AeroGpuOpcode::SetViewport as u32, |out| {
            push_u32(out, 0.0f32.to_bits()); // x_f32
            push_u32(out, 1.0f32.to_bits()); // y_f32
            push_u32(out, 640.0f32.to_bits()); // width_f32
            push_u32(out, 480.0f32.to_bits()); // height_f32
            push_u32(out, 0.0f32.to_bits()); // min_depth_f32
            push_u32(out, 1.0f32.to_bits()); // max_depth_f32
        });

        emit_packet(out, AeroGpuOpcode::SetScissor as u32, |out| {
            push_i32(out, 1); // x
            push_i32(out, 2); // y
            push_i32(out, 3); // width
            push_i32(out, 4); // height
        });

        emit_packet(out, AeroGpuOpcode::SetVertexBuffers as u32, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 2); // buffer_count
            out.extend_from_slice(&expected_vb_bindings);
        });

        emit_packet(out, AeroGpuOpcode::SetIndexBuffer as u32, |out| {
            push_u32(out, 0xB0); // buffer
            push_u32(out, 1); // format
            push_u32(out, 128); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::SetPrimitiveTopology as u32, |out| {
            push_u32(out, 4); // topology
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::SetTexture as u32, |out| {
            push_u32(out, 1); // shader_stage
            push_u32(out, 3); // slot
            push_u32(out, 0xC0); // texture
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::SetSamplerState as u32, |out| {
            push_u32(out, 1); // shader_stage
            push_u32(out, 2); // slot
            push_u32(out, 7); // state
            push_u32(out, 9); // value
        });

        emit_packet(out, AeroGpuOpcode::SetRenderState as u32, |out| {
            push_u32(out, 0x10); // state
            push_u32(out, 0x20); // value
        });

        emit_packet(out, AeroGpuOpcode::Clear as u32, |out| {
            push_u32(out, 1); // flags
            push_u32(out, 1.0f32.to_bits());
            push_u32(out, 0.5f32.to_bits());
            push_u32(out, 0.25f32.to_bits());
            push_u32(out, 0.0f32.to_bits());
            push_u32(out, 1.0f32.to_bits()); // depth_f32
            push_u32(out, 3); // stencil
        });

        emit_packet(out, AeroGpuOpcode::Draw as u32, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        emit_packet(out, AeroGpuOpcode::DrawIndexed as u32, |out| {
            push_u32(out, 6); // index_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_index
            push_i32(out, -1); // base_vertex
            push_u32(out, 0); // first_instance
        });

        emit_packet(out, AeroGpuOpcode::Present as u32, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 1); // flags
        });

        emit_packet(out, AeroGpuOpcode::PresentEx as u32, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 1); // flags
            push_u32(out, 2); // d3d9_present_flags
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, AeroGpuOpcode::ExportSharedSurface as u32, |out| {
            push_u32(out, 0xD0); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });

        emit_packet(out, AeroGpuOpcode::ImportSharedSurface as u32, |out| {
            push_u32(out, 0xD1); // out_resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0x1122_3344_5566_7788);
        });

        emit_packet(out, AeroGpuOpcode::Flush as u32, |out| {
            push_u32(out, 0); // reserved0
            push_u32(out, 0); // reserved1
        });
    });

    let parsed = parse_cmd_stream(&stream).expect("parse should succeed");
    assert_eq!(parsed.cmds.len(), 36);

    let mut cmds = parsed.cmds.into_iter();

    assert!(matches!(cmds.next(), Some(AeroGpuCmd::Nop)));
    match cmds.next().unwrap() {
        AeroGpuCmd::DebugMarker { bytes } => assert_eq!(bytes, debug_marker),
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::CreateBuffer {
            buffer_handle,
            usage_flags,
            size_bytes,
            backing_alloc_id,
            backing_offset_bytes,
        } => {
            assert_eq!(buffer_handle, 0x10);
            assert_eq!(usage_flags, 0x3);
            assert_eq!(size_bytes, 0x1122);
            assert_eq!(backing_alloc_id, 0x5);
            assert_eq!(backing_offset_bytes, 0x20);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::CreateTexture2d {
            texture_handle,
            usage_flags,
            format,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
            backing_alloc_id,
            backing_offset_bytes,
        } => {
            assert_eq!(texture_handle, 0x11);
            assert_eq!(usage_flags, 0x4);
            assert_eq!(format, 0x16);
            assert_eq!(width, 640);
            assert_eq!(height, 480);
            assert_eq!(mip_levels, 1);
            assert_eq!(array_layers, 1);
            assert_eq!(row_pitch_bytes, 2560);
            assert_eq!(backing_alloc_id, 2);
            assert_eq!(backing_offset_bytes, 128);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::DestroyResource { resource_handle } => assert_eq!(resource_handle, 0x11),
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::ResourceDirtyRange {
            resource_handle,
            offset_bytes,
            size_bytes,
        } => {
            assert_eq!(resource_handle, 0x10);
            assert_eq!(offset_bytes, 0x1000);
            assert_eq!(size_bytes, 0x200);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::UploadResource {
            resource_handle,
            offset_bytes,
            size_bytes,
            data,
        } => {
            assert_eq!(resource_handle, 0x10);
            assert_eq!(offset_bytes, 0x20);
            assert_eq!(size_bytes, upload_data.len() as u64);
            assert_eq!(data, upload_data);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::CopyBuffer {
            dst_buffer,
            src_buffer,
            dst_offset_bytes,
            src_offset_bytes,
            size_bytes,
            flags,
        } => {
            assert_eq!(dst_buffer, 0x20);
            assert_eq!(src_buffer, 0x21);
            assert_eq!(dst_offset_bytes, 0x1000);
            assert_eq!(src_offset_bytes, 0x2000);
            assert_eq!(size_bytes, 0x80);
            assert_eq!(flags, 0x1);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::CopyTexture2d {
            dst_texture,
            src_texture,
            dst_mip_level,
            dst_array_layer,
            src_mip_level,
            src_array_layer,
            dst_x,
            dst_y,
            src_x,
            src_y,
            width,
            height,
            flags,
        } => {
            assert_eq!(dst_texture, 0x30);
            assert_eq!(src_texture, 0x31);
            assert_eq!(dst_mip_level, 1);
            assert_eq!(dst_array_layer, 2);
            assert_eq!(src_mip_level, 3);
            assert_eq!(src_array_layer, 4);
            assert_eq!(dst_x, 5);
            assert_eq!(dst_y, 6);
            assert_eq!(src_x, 7);
            assert_eq!(src_y, 8);
            assert_eq!(width, 9);
            assert_eq!(height, 10);
            assert_eq!(flags, 0x2);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::CreateShaderDxbc {
            shader_handle,
            stage,
            dxbc_size_bytes,
            dxbc_bytes: parsed_dxbc,
        } => {
            assert_eq!(shader_handle, 0x33);
            assert_eq!(stage, 1);
            assert_eq!(dxbc_size_bytes, dxbc_bytes.len() as u32);
            assert_eq!(parsed_dxbc, dxbc_bytes);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::DestroyShader { shader_handle } => assert_eq!(shader_handle, 0x33),
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::BindShaders { vs, ps, cs } => {
            assert_eq!(vs, 1);
            assert_eq!(ps, 2);
            assert_eq!(cs, 3);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetShaderConstantsF {
            stage,
            start_register,
            vec4_count,
            data,
        } => {
            assert_eq!(stage, 0);
            assert_eq!(start_register, 4);
            assert_eq!(vec4_count, 2);
            assert_eq!(data, constants_bytes);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::CreateInputLayout {
            input_layout_handle,
            blob_size_bytes,
            blob_bytes,
        } => {
            assert_eq!(input_layout_handle, 0x44);
            assert_eq!(blob_size_bytes as usize, ilay_blob.len());
            assert_eq!(blob_bytes, ilay_blob);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::DestroyInputLayout {
            input_layout_handle,
        } => assert_eq!(input_layout_handle, 0x44),
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetInputLayout {
            input_layout_handle,
        } => assert_eq!(input_layout_handle, 0x44),
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetBlendState { state } => {
            assert_eq!(state.enable, 1);
            assert_eq!(state.src_factor, 2);
            assert_eq!(state.dst_factor, 3);
            assert_eq!(state.blend_op, 4);
            assert_eq!(state.color_write_mask, 0xF);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetDepthStencilState { state } => {
            assert_eq!(state.depth_enable, 1);
            assert_eq!(state.depth_write_enable, 1);
            assert_eq!(state.depth_func, 2);
            assert_eq!(state.stencil_enable, 0);
            assert_eq!(state.stencil_read_mask, 0xAA);
            assert_eq!(state.stencil_write_mask, 0xBB);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetRasterizerState { state } => {
            assert_eq!(state.fill_mode, 0);
            assert_eq!(state.cull_mode, 1);
            assert_eq!(state.front_ccw, 1);
            assert_eq!(state.scissor_enable, 0);
            assert_eq!(state.depth_bias, -1);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetRenderTargets {
            color_count,
            depth_stencil,
            colors,
        } => {
            assert_eq!(color_count, 2);
            assert_eq!(depth_stencil, 0x99);
            assert_eq!(colors[0], 1);
            assert_eq!(colors[1], 2);
            assert!(colors[2..].iter().all(|&v| v == 0));
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetViewport {
            x_f32,
            y_f32,
            width_f32,
            height_f32,
            min_depth_f32,
            max_depth_f32,
        } => {
            assert_eq!(x_f32, 0.0f32.to_bits());
            assert_eq!(y_f32, 1.0f32.to_bits());
            assert_eq!(width_f32, 640.0f32.to_bits());
            assert_eq!(height_f32, 480.0f32.to_bits());
            assert_eq!(min_depth_f32, 0.0f32.to_bits());
            assert_eq!(max_depth_f32, 1.0f32.to_bits());
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetScissor {
            x,
            y,
            width,
            height,
        } => {
            assert_eq!(x, 1);
            assert_eq!(y, 2);
            assert_eq!(width, 3);
            assert_eq!(height, 4);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetVertexBuffers {
            start_slot,
            buffer_count,
            bindings_bytes,
        } => {
            assert_eq!(start_slot, 0);
            assert_eq!(buffer_count, 2);
            assert_eq!(bindings_bytes, expected_vb_bindings);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetIndexBuffer {
            buffer,
            format,
            offset_bytes,
        } => {
            assert_eq!(buffer, 0xB0);
            assert_eq!(format, 1);
            assert_eq!(offset_bytes, 128);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetPrimitiveTopology { topology } => assert_eq!(topology, 4),
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetTexture {
            shader_stage,
            slot,
            texture,
        } => {
            assert_eq!(shader_stage, 1);
            assert_eq!(slot, 3);
            assert_eq!(texture, 0xC0);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetSamplerState {
            shader_stage,
            slot,
            state,
            value,
        } => {
            assert_eq!(shader_stage, 1);
            assert_eq!(slot, 2);
            assert_eq!(state, 7);
            assert_eq!(value, 9);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::SetRenderState { state, value } => {
            assert_eq!(state, 0x10);
            assert_eq!(value, 0x20);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::Clear {
            flags,
            color_rgba_f32,
            depth_f32,
            stencil,
        } => {
            assert_eq!(flags, 1);
            assert_eq!(
                color_rgba_f32,
                [
                    1.0f32.to_bits(),
                    0.5f32.to_bits(),
                    0.25f32.to_bits(),
                    0.0f32.to_bits(),
                ]
            );
            assert_eq!(depth_f32, 1.0f32.to_bits());
            assert_eq!(stencil, 3);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::Draw {
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        } => {
            assert_eq!(vertex_count, 3);
            assert_eq!(instance_count, 1);
            assert_eq!(first_vertex, 0);
            assert_eq!(first_instance, 0);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::DrawIndexed {
            index_count,
            instance_count,
            first_index,
            base_vertex,
            first_instance,
        } => {
            assert_eq!(index_count, 6);
            assert_eq!(instance_count, 1);
            assert_eq!(first_index, 0);
            assert_eq!(base_vertex, -1);
            assert_eq!(first_instance, 0);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::Present { scanout_id, flags } => {
            assert_eq!(scanout_id, 0);
            assert_eq!(flags, 1);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::PresentEx {
            scanout_id,
            flags,
            d3d9_present_flags,
        } => {
            assert_eq!(scanout_id, 0);
            assert_eq!(flags, 1);
            assert_eq!(d3d9_present_flags, 2);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::ExportSharedSurface {
            resource_handle,
            share_token,
        } => {
            assert_eq!(resource_handle, 0xD0);
            assert_eq!(share_token, 0x1122_3344_5566_7788);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    match cmds.next().unwrap() {
        AeroGpuCmd::ImportSharedSurface {
            out_resource_handle,
            share_token,
        } => {
            assert_eq!(out_resource_handle, 0xD1);
            assert_eq!(share_token, 0x1122_3344_5566_7788);
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    assert!(matches!(cmds.next(), Some(AeroGpuCmd::Flush)));
    assert!(cmds.next().is_none());
}

#[test]
fn protocol_skips_unknown_opcodes() {
    let stream = build_stream(|out| {
        emit_packet(out, 0xDEAD_BEEF, |out| {
            push_u32(out, 0xAABB_CCDD);
        });

        emit_packet(out, AeroGpuOpcode::Present as u32, |out| {
            push_u32(out, 0);
            push_u32(out, 1);
        });
    });

    let parsed = parse_cmd_stream(&stream).expect("parse should succeed");
    assert_eq!(parsed.cmds.len(), 2);

    match &parsed.cmds[0] {
        AeroGpuCmd::Unknown { opcode, payload } => {
            assert_eq!(*opcode, 0xDEAD_BEEF);
            assert_eq!(*payload, 0xAABB_CCDD_u32.to_le_bytes());
        }
        other => panic!("unexpected cmd: {other:?}"),
    }

    assert!(matches!(parsed.cmds[1], AeroGpuCmd::Present { .. }));
}

#[test]
fn protocol_rejects_misaligned_cmd_size_bytes() {
    let mut stream = Vec::new();
    push_u32(&mut stream, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut stream, 0); // size_bytes patched later
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // cmd header: size_bytes = 10 (not 4-byte aligned)
    push_u32(&mut stream, AeroGpuOpcode::Nop as u32);
    push_u32(&mut stream, 10);
    stream.extend_from_slice(&[0u8; 2]);

    let size_bytes = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());

    let err = parse_cmd_stream(&stream).unwrap_err();
    assert!(matches!(
        err,
        AeroGpuCmdStreamParseError::MisalignedCmdSizeBytes(10)
    ));
}

#[test]
fn protocol_rejects_truncated_variable_payload() {
    let stream = build_stream(|out| {
        // CREATE_SHADER_DXBC with dxbc_size_bytes=8, but only 4 bytes follow.
        emit_packet(out, AeroGpuOpcode::CreateShaderDxbc as u32, |out| {
            push_u32(out, 0x33); // shader_handle
            push_u32(out, 1); // stage
            push_u32(out, 8); // dxbc_size_bytes (claims 8)
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&[1, 2, 3, 4]); // truncated dxbc bytes
        });
    });

    let err = parse_cmd_stream(&stream).unwrap_err();
    assert!(matches!(err, AeroGpuCmdStreamParseError::BufferTooSmall));
}

#[test]
fn protocol_rejects_stream_size_bytes_smaller_than_header() {
    let mut stream = Vec::new();
    push_u32(&mut stream, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut stream, 16); // size_bytes < header size (24)
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    let err = parse_cmd_stream(&stream).unwrap_err();
    assert!(matches!(
        err,
        AeroGpuCmdStreamParseError::InvalidSizeBytes { size_bytes: 16, .. }
    ));
}
