mod common;

use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor, AerogpuD3d9ExecutorConfig};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader,
        AerogpuPrimitiveTopology, AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC,
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn push_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn align4(v: usize) -> usize {
    (v + 3) & !3
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
    let end_aligned = align4(out.len());
    out.resize(end_aligned, 0);
    let size_bytes = (end_aligned - start) as u32;
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

fn enc_reg_type(ty: u8) -> u32 {
    let low = (ty & 0x7) as u32;
    let high = (ty & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn enc_src(reg_type: u8, reg_num: u16, swizzle: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16)
}

fn enc_dst(reg_type: u8, reg_num: u16, mask: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((mask as u32) << 16)
}

fn enc_inst(opcode: u16, params: &[u32]) -> Vec<u32> {
    // D3D9 SM2/SM3 encodes the *total* instruction length in tokens (including the opcode token)
    // in bits 24..27.
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn assemble_vs_passthrough_pos() -> Vec<u8> {
    // vs_2_0: mov oPos, v0; end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_solid_color_c0() -> Vec<u8> {
    // ps_2_0: mov oC0, c0; end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn px(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * width + x) * 4) as usize;
    rgba[idx..idx + 4].try_into().unwrap()
}

fn run(exec: &mut AerogpuD3d9Executor, viewport: Option<(f32, f32)>) -> Vec<u8> {
    // Reuse the same wgpu device across multiple runs to avoid repeatedly creating/dropping
    // adapters/devices, which can be fragile on some backends.
    exec.reset();

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    let width = 4u32;
    let height = 4u32;

    // Build a rectangle whose window-space bounds are [0.9, 1.9] in both X and Y.
    //
    // Without the half-pixel adjustment, this should cover the pixel centered at (1.5, 1.5)
    // (i.e. pixel (1,1) in a 4x4 target). With the adjustment, the vertex shader shifts the
    // geometry by (-0.5, -0.5) pixels, so it should instead cover pixel (0,0).
    let vp_w = width as f32;
    let vp_h = height as f32;
    let x0 = 0.9f32;
    let x1 = 1.9f32;
    let y0 = 0.9f32;
    let y1 = 1.9f32;
    let ndc_x0 = (x0 / vp_w) * 2.0 - 1.0;
    let ndc_x1 = (x1 / vp_w) * 2.0 - 1.0;
    let ndc_y0 = 1.0 - (y0 / vp_h) * 2.0;
    let ndc_y1 = 1.0 - (y1 / vp_h) * 2.0;

    let verts = [
        // Triangle 1 (clockwise): tl, tr, bl.
        (ndc_x0, ndc_y0),
        (ndc_x1, ndc_y0),
        (ndc_x0, ndc_y1),
        // Triangle 2 (clockwise): bl, tr, br.
        (ndc_x0, ndc_y1),
        (ndc_x1, ndc_y0),
        (ndc_x1, ndc_y1),
    ];

    let mut vb_data = Vec::with_capacity(verts.len() * 16);
    for (x, y) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, 0.0);
        push_f32(&mut vb_data, 1.0);
    }

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut vertex_decl = Vec::new();
    push_u16(&mut vertex_decl, 0); // stream
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 3); // type = FLOAT4
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage = POSITION
    push_u8(&mut vertex_decl, 0); // usage_index
    push_u16(&mut vertex_decl, 0x00FF); // stream = 0xFF
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 17); // type = UNUSED
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage
    push_u8(&mut vertex_decl, 0); // usage_index

    let stream = build_stream(|out| {
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(
                out,
                AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
            );
            push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(out, width);
            push_u32(out, height);
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, width * 4); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_data.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_data.len() as u64);
            out.extend_from_slice(&vb_data);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, 0); // AEROGPU_SHADER_STAGE_VERTEX
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, ps_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&ps_bytes);
        });

        emit_packet(out, OPC_BIND_SHADERS, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, PS_HANDLE);
            push_u32(out, 0); // cs
            push_u32(out, 0); // reserved0 (gs when non-zero)
        });

        emit_packet(out, OPC_CREATE_INPUT_LAYOUT, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, vertex_decl.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vertex_decl);
        });

        emit_packet(out, OPC_SET_INPUT_LAYOUT, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_HANDLE);
            // Remaining 7 color slots.
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        // Deliberately omit SetViewport in some tests to validate the D3D9 default viewport
        // behavior (full render target). Some tests also pass an oversized viewport to validate
        // that we use the *effective clamped viewport* for half-pixel calculations.
        if let Some((vp_w, vp_h)) = viewport {
            emit_packet(out, OPC_SET_VIEWPORT, |out| {
                push_u32(out, 0.0f32.to_bits()); // x
                push_u32(out, 0.0f32.to_bits()); // y
                push_u32(out, vp_w.to_bits()); // width
                push_u32(out, vp_h.to_bits()); // height
                push_u32(out, 0.0f32.to_bits()); // min_depth
                push_u32(out, 1.0f32.to_bits()); // max_depth
            });
        }

        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_u32(out, 0); // x
            push_u32(out, 0); // y
            push_u32(out, width); // width
            push_u32(out, height); // height
        });

        emit_packet(out, OPC_SET_VERTEX_BUFFERS, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            // One binding entry.
            push_u32(out, VB_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_PRIMITIVE_TOPOLOGY, |out| {
            push_u32(out, AEROGPU_TOPOLOGY_TRIANGLELIST);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            // color = opaque black
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_u32(out, 0.0f32.to_bits()); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
                              // c0 = green
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, verts.len() as u32); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));
    rgba
}

fn assert_half_pixel_center_shift(off: &[u8], on: &[u8]) {
    let width = 4u32;
    let bg = [0, 0, 0, 255];
    let green = [0, 255, 0, 255];

    assert_eq!(
        px(off, width, 0, 0),
        bg,
        "half_pixel_center=false: (0,0) is background"
    );
    assert_eq!(
        px(off, width, 1, 1),
        green,
        "half_pixel_center=false: (1,1) is inside the rect"
    );

    assert_eq!(
        px(on, width, 0, 0),
        green,
        "half_pixel_center=true: (0,0) is inside the rect"
    );
    assert_eq!(
        px(on, width, 1, 1),
        bg,
        "half_pixel_center=true: (1,1) is background"
    );
}

#[test]
fn d3d9_half_pixel_center_cases() {
    let mut exec_off = match pollster::block_on(AerogpuD3d9Executor::new_headless_with_config(
        AerogpuD3d9ExecutorConfig {
            half_pixel_center: false,
        },
    )) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };
    // Create a second executor with half_pixel_center enabled.
    //
    // Note: we intentionally create a separate executor instead of trying to share the underlying
    // wgpu device/queue. `wgpu::Device`/`wgpu::Queue` are not `Clone`, and the executor owns them.

    // Explicit viewport (matches RT).
    let off_explicit = run(&mut exec_off, Some((4.0, 4.0)));

    // Default viewport (guest never calls SetViewport).
    let off_default = run(&mut exec_off, None);

    // Oversized viewport clamped to RT bounds.
    let off_oversized = run(&mut exec_off, Some((8.0, 8.0)));

    drop(exec_off);
    let mut exec_on = match pollster::block_on(AerogpuD3d9Executor::new_headless_with_config(
        AerogpuD3d9ExecutorConfig {
            half_pixel_center: true,
        },
    )) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    let on_explicit = run(&mut exec_on, Some((4.0, 4.0)));
    assert_half_pixel_center_shift(&off_explicit, &on_explicit);

    let on_default = run(&mut exec_on, None);
    assert_half_pixel_center_shift(&off_default, &on_default);

    let on_oversized = run(&mut exec_on, Some((8.0, 8.0)));
    assert_half_pixel_center_shift(&off_oversized, &on_oversized);

    // Multi-context: ensure per-context uniform initialization even when viewports match.
    let exec = &mut exec_on;
    exec.reset();

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_SCISSOR: u32 = AerogpuCmdOpcode::SetScissor as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

    const RT0_HANDLE: u32 = 1;
    const RT1_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 4u32;
    let height = 4u32;

    // Same rectangle as other tests.
    let vp_w = width as f32;
    let vp_h = height as f32;
    let x0 = 0.9f32;
    let x1 = 1.9f32;
    let y0 = 0.9f32;
    let y1 = 1.9f32;
    let ndc_x0 = (x0 / vp_w) * 2.0 - 1.0;
    let ndc_x1 = (x1 / vp_w) * 2.0 - 1.0;
    let ndc_y0 = 1.0 - (y0 / vp_h) * 2.0;
    let ndc_y1 = 1.0 - (y1 / vp_h) * 2.0;

    let verts = [
        // Triangle 1 (clockwise): tl, tr, bl.
        (ndc_x0, ndc_y0),
        (ndc_x1, ndc_y0),
        (ndc_x0, ndc_y1),
        // Triangle 2 (clockwise): bl, tr, br.
        (ndc_x0, ndc_y1),
        (ndc_x1, ndc_y0),
        (ndc_x1, ndc_y1),
    ];

    let mut vb_data = Vec::with_capacity(verts.len() * 16);
    for (x, y) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, 0.0);
        push_f32(&mut vb_data, 1.0);
    }

    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut vertex_decl = Vec::new();
    push_u16(&mut vertex_decl, 0); // stream
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 3); // type = FLOAT4
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage = POSITION
    push_u8(&mut vertex_decl, 0); // usage_index
    push_u16(&mut vertex_decl, 0x00FF); // stream = 0xFF
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 17); // type = UNUSED
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage
    push_u8(&mut vertex_decl, 0); // usage_index

    // Context 0: create all resources, then draw green into RT0.
    let stream0 = build_stream(|out| {
        for &rt in &[RT0_HANDLE, RT1_HANDLE] {
            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, rt);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AEROGPU_FORMAT_R8G8B8A8_UNORM);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        }

        emit_packet(out, OPC_CREATE_BUFFER, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(out, vb_data.len() as u64);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, VB_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_data.len() as u64);
            out.extend_from_slice(&vb_data);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, 0); // AEROGPU_SHADER_STAGE_VERTEX
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, ps_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&ps_bytes);
        });

        emit_packet(out, OPC_BIND_SHADERS, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, PS_HANDLE);
            push_u32(out, 0); // cs
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_INPUT_LAYOUT, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, vertex_decl.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vertex_decl);
        });

        emit_packet(out, OPC_SET_INPUT_LAYOUT, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT0_HANDLE);
            // Remaining 7 color slots.
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, OPC_SET_VIEWPORT, |out| {
            push_u32(out, 0.0f32.to_bits()); // x
            push_u32(out, 0.0f32.to_bits()); // y
            push_u32(out, (width as f32).to_bits()); // width
            push_u32(out, (height as f32).to_bits()); // height
            push_u32(out, 0.0f32.to_bits()); // min_depth
            push_u32(out, 1.0f32.to_bits()); // max_depth
        });

        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_u32(out, 0); // x
            push_u32(out, 0); // y
            push_u32(out, width); // width
            push_u32(out, height); // height
        });

        emit_packet(out, OPC_SET_VERTEX_BUFFERS, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
                              // One binding entry.
            push_u32(out, VB_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved1
        });

        emit_packet(out, OPC_SET_PRIMITIVE_TOPOLOGY, |out| {
            push_u32(out, AEROGPU_TOPOLOGY_TRIANGLELIST);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            // color = opaque black
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_u32(out, 0.0f32.to_bits()); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
                              // c0 = green
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, verts.len() as u32); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    // Context 1: reuse resources, draw red into RT1. The viewport dimensions are identical to the
    // draw in context 0; without per-context tracking of the last uploaded viewport dims, the
    // executor could incorrectly skip initializing the half-pixel uniform buffer for the new
    // context.
    let stream1 = build_stream(|out| {
        emit_packet(out, OPC_BIND_SHADERS, |out| {
            push_u32(out, VS_HANDLE);
            push_u32(out, PS_HANDLE);
            push_u32(out, 0); // cs
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_INPUT_LAYOUT, |out| {
            push_u32(out, IL_HANDLE);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT1_HANDLE);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, OPC_SET_VIEWPORT, |out| {
            push_u32(out, 0.0f32.to_bits()); // x
            push_u32(out, 0.0f32.to_bits()); // y
            push_u32(out, (width as f32).to_bits()); // width
            push_u32(out, (height as f32).to_bits()); // height
            push_u32(out, 0.0f32.to_bits()); // min_depth
            push_u32(out, 1.0f32.to_bits()); // max_depth
        });

        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_u32(out, 0); // x
            push_u32(out, 0); // y
            push_u32(out, width); // width
            push_u32(out, height); // height
        });

        emit_packet(out, OPC_SET_VERTEX_BUFFERS, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, VB_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved1
        });

        emit_packet(out, OPC_SET_PRIMITIVE_TOPOLOGY, |out| {
            push_u32(out, AEROGPU_TOPOLOGY_TRIANGLELIST);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            // color = opaque black
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_u32(out, 0.0f32.to_bits()); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
                              // c0 = red
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, verts.len() as u32); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    exec.execute_cmd_stream_for_context(0, &stream0)
        .expect("context 0 execute should succeed");
    exec.execute_cmd_stream_for_context(1, &stream1)
        .expect("context 1 execute should succeed");

    let (_, _, rgba0) = pollster::block_on(exec.readback_texture_rgba8(RT0_HANDLE))
        .expect("readback RT0 should succeed");
    let (_, _, rgba1) = pollster::block_on(exec.readback_texture_rgba8(RT1_HANDLE))
        .expect("readback RT1 should succeed");

    let bg = [0, 0, 0, 255];
    let green = [0, 255, 0, 255];
    let red = [255, 0, 0, 255];

    assert_eq!(px(&rgba0, width, 0, 0), green, "context 0: (0,0) is inside");
    assert_eq!(
        px(&rgba0, width, 1, 1),
        bg,
        "context 0: (1,1) is background"
    );

    assert_eq!(px(&rgba1, width, 0, 0), red, "context 1: (0,0) is inside");
    assert_eq!(
        px(&rgba1, width, 1, 1),
        bg,
        "context 1: (1,1) is background"
    );
}
