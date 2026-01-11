use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
    AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
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

fn push_i32(out: &mut Vec<u8>, v: i32) {
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
    let token = (opcode as u32) | ((params.len() as u32) << 24);
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
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_solid_color_c0() -> Vec<u8> {
    // ps_2_0: mov oC0, c0; end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

#[test]
fn d3d9_shared_surface_import_export_renders_via_alias_handle() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            eprintln!("skipping d3d9 shared surface test: wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    // Protocol constants from `drivers/aerogpu/protocol/aerogpu_cmd.h`.
    const OPC_CREATE_BUFFER: u32 = 0x100;
    const OPC_CREATE_TEXTURE2D: u32 = 0x101;
    const OPC_DESTROY_RESOURCE: u32 = 0x102;
    const OPC_UPLOAD_RESOURCE: u32 = 0x104;
    const OPC_CREATE_SHADER_DXBC: u32 = 0x200;
    const OPC_BIND_SHADERS: u32 = 0x202;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = 0x203;
    const OPC_CREATE_INPUT_LAYOUT: u32 = 0x204;
    const OPC_SET_INPUT_LAYOUT: u32 = 0x206;
    const OPC_SET_RENDER_TARGETS: u32 = 0x400;
    const OPC_SET_VIEWPORT: u32 = 0x401;
    const OPC_SET_SCISSOR: u32 = 0x402;
    const OPC_SET_VERTEX_BUFFERS: u32 = 0x500;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = 0x502;
    const OPC_CLEAR: u32 = 0x600;
    const OPC_DRAW: u32 = 0x601;
    const OPC_PRESENT: u32 = 0x700;
    const OPC_EXPORT_SHARED_SURFACE: u32 = 0x710;
    const OPC_IMPORT_SHARED_SURFACE: u32 = 0x711;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = 3;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = 1 << 3;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = 1 << 4;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = 1 << 0;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = 4;
    const AEROGPU_CLEAR_COLOR: u32 = 1 << 0;

    const SHARE_TOKEN: u64 = 0x1122_3344_5566_7788;

    const RT_HANDLE: u32 = 1;
    const RT_ALIAS: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let mut vb_data = Vec::new();
    let verts = [
        (-0.8f32, -0.2f32, 0.0f32, 1.0f32),
        (0.8f32, -0.2f32, 0.0f32, 1.0f32),
        (0.0f32, 0.8f32, 0.0f32, 1.0f32),
    ];
    for (x, y, z, w) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, z);
        push_f32(&mut vb_data, w);
    }
    assert_eq!(vb_data.len(), 3 * 16);

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
    assert_eq!(vertex_decl.len(), 16);

    // Create a render target, export it, import it to an alias handle, and drop the original
    // handle. Rendering should still work through the alias.
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

        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, SHARE_TOKEN);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, RT_ALIAS);
            push_u32(out, 0); // reserved0
            push_u64(out, SHARE_TOKEN);
        });

        // Re-export the alias under the same token to ensure EXPORT_SHARED_SURFACE resolves alias
        // handles to the underlying resource.
        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, RT_ALIAS);
            push_u32(out, 0); // reserved0
            push_u64(out, SHARE_TOKEN);
        });

        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(out, 0); // reserved0
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

        emit_packet(out, OPC_SET_VERTEX_BUFFERS, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, VB_HANDLE);
            push_u32(out, 16); // stride_bytes
            push_u32(out, 0); // offset_bytes
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_PRIMITIVE_TOPOLOGY, |out| {
            push_u32(out, AEROGPU_TOPOLOGY_TRIANGLELIST);
            push_u32(out, 0); // reserved0
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_ALIAS);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, OPC_SET_VIEWPORT, |out| {
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, width as f32);
            push_f32(out, height as f32);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_SET_SCISSOR, |out| {
            push_i32(out, 0);
            push_i32(out, 0);
            push_i32(out, width as i32);
            push_i32(out, height as i32);
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
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
            push_u32(out, 3); // vertex_count
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

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_ALIAS))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));
    assert_eq!(rgba.len(), (width * height * 4) as usize);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    assert_eq!(px(32, 2), [0, 0, 0, 255], "top row should be background");
    assert_eq!(
        px(32, 48),
        [0, 0, 0, 255],
        "bottom probe should be background"
    );
    assert_eq!(
        px(32, 16),
        [0, 255, 0, 255],
        "center-top probe should be inside the triangle"
    );

    // After readback succeeds, dropping the alias should drop the final reference to the texture.
    let teardown = build_stream(|out| {
        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, RT_ALIAS);
            push_u32(out, 0); // reserved0
        });
    });
    exec.execute_cmd_stream(&teardown)
        .expect("destroying alias should succeed");
}

#[test]
fn d3d9_presented_scanout_survives_destroying_present_alias_handle() {
    let mut exec = match pollster::block_on(AerogpuD3d9Executor::new_headless()) {
        Ok(exec) => exec,
        Err(AerogpuD3d9Error::AdapterNotFound) => {
            eprintln!("skipping d3d9 present alias test: wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create executor: {err}"),
    };

    // Protocol constants from `drivers/aerogpu/protocol/aerogpu_cmd.h`.
    const OPC_CREATE_TEXTURE2D: u32 = 0x101;
    const OPC_DESTROY_RESOURCE: u32 = 0x102;
    const OPC_SET_RENDER_TARGETS: u32 = 0x400;
    const OPC_CLEAR: u32 = 0x600;
    const OPC_PRESENT: u32 = 0x700;
    const OPC_EXPORT_SHARED_SURFACE: u32 = 0x710;
    const OPC_IMPORT_SHARED_SURFACE: u32 = 0x711;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = 3;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = 1 << 3;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = 1 << 4;
    const AEROGPU_CLEAR_COLOR: u32 = 1 << 0;

    const SHARE_TOKEN: u64 = 0x7788_6655_4433_2211;

    const RT_HANDLE: u32 = 1;
    const RT_ALIAS: u32 = 2;

    let width = 8u32;
    let height = 8u32;

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

        emit_packet(out, OPC_EXPORT_SHARED_SURFACE, |out| {
            push_u32(out, RT_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, SHARE_TOKEN);
        });

        emit_packet(out, OPC_IMPORT_SHARED_SURFACE, |out| {
            push_u32(out, RT_ALIAS);
            push_u32(out, 0); // reserved0
            push_u64(out, SHARE_TOKEN);
        });

        emit_packet(out, OPC_SET_RENDER_TARGETS, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, RT_ALIAS);
            for _ in 0..7 {
                push_u32(out, 0);
            }
        });

        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(out, AEROGPU_CLEAR_COLOR);
            // RGBA = red
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        emit_packet(out, OPC_PRESENT, |out| {
            push_u32(out, 0); // scanout_id
            push_u32(out, 0); // flags
        });

        // Destroy the alias that was used for Present; scanout readback should still work because
        // the executor records the underlying handle.
        emit_packet(out, OPC_DESTROY_RESOURCE, |out| {
            push_u32(out, RT_ALIAS);
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("execute should succeed");

    let (_w, _h, rgba) = pollster::block_on(exec.read_presented_scanout_rgba8(0))
        .expect("read_presented_scanout should succeed")
        .expect("scanout should be present");
    assert_eq!(rgba.len(), (width * height * 4) as usize);
    let center = (((height / 2) * width) + (width / 2)) * 4;
    assert_eq!(&rgba[center as usize..center as usize + 4], &[255, 0, 0, 255]);
}
