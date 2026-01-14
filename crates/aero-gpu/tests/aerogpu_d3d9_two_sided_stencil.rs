mod common;

use std::sync::Arc;

use aero_gpu::stats::GpuStats;
use aero_gpu::AerogpuD3d9Executor;
use aero_protocol::aerogpu::aerogpu_cmd as cmd;
use aero_protocol::aerogpu::aerogpu_pci as pci;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(cmd::AerogpuCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(cmd::AerogpuCmdHdr, size_bytes);

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
    push_u32(&mut out, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, pci::AEROGPU_ABI_VERSION_U32);
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
    // D3D9 SM2/SM3 encodes the *total* instruction length in DWORD tokens (including the opcode
    // token) in bits 24..27.
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

fn vertex_decl_pos() -> Vec<u8> {
    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // End marker: stream 0xFF, type UNUSED.
    let mut decl = Vec::new();
    // POSITION0
    push_u16(&mut decl, 0); // stream
    push_u16(&mut decl, 0); // offset
    push_u8(&mut decl, 3); // type = FLOAT4
    push_u8(&mut decl, 0); // method
    push_u8(&mut decl, 0); // usage = POSITION
    push_u8(&mut decl, 0); // usage_index
                           // End marker
    push_u16(&mut decl, 0x00FF); // stream = 0xFF
    push_u16(&mut decl, 0); // offset
    push_u8(&mut decl, 17); // type = UNUSED
    push_u8(&mut decl, 0); // method
    push_u8(&mut decl, 0); // usage
    push_u8(&mut decl, 0); // usage_index
    decl
}

fn two_sided_stencil_test_vertices() -> Vec<u8> {
    // Layout: float4 position.
    //
    // First two triangles overlap and have opposite winding.
    // The last triangle is a fullscreen triangle.
    let mut vb = Vec::new();

    let verts: [[f32; 4]; 9] = [
        // Triangle 0: CW winding.
        [-1.0, -1.0, 0.0, 1.0],
        [-1.0, 1.0, 0.0, 1.0],
        [0.2, 0.0, 0.0, 1.0],
        // Triangle 1: CCW winding.
        [1.0, -1.0, 0.0, 1.0],
        [1.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 0.0, 1.0],
        // Triangle 2: fullscreen (clockwise in D3D9's default winding convention).
        [-1.0, -1.0, 0.0, 1.0],
        [-1.0, 3.0, 0.0, 1.0],
        [3.0, -1.0, 0.0, 1.0],
    ];

    for pos in verts {
        for f in pos {
            push_f32(&mut vb, f);
        }
    }

    vb
}

fn pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * width + x) * 4) as usize;
    pixels[idx..idx + 4].try_into().unwrap()
}

fn create_executor_with_d24s8_support() -> Option<AerogpuD3d9Executor> {
    common::ensure_xdg_runtime_dir();

    let backends = if cfg!(target_os = "linux") {
        // Prefer wgpu's GL backend on Linux CI for stability. Vulkan software adapters have been a
        // recurring source of flakes/crashes in headless sandboxes.
        wgpu::Backends::GL
    } else {
        wgpu::Backends::all()
    };

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });

    let adapter = pollster::block_on(async {
        match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
        {
            Some(adapter) => Some(adapter),
            None => {
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        }
    });

    let Some(adapter) = adapter else {
        common::skip_or_panic(module_path!(), "wgpu adapter not found");
        return None;
    };

    let format_features =
        adapter.get_texture_format_features(wgpu::TextureFormat::Depth24PlusStencil8);
    if !format_features
        .allowed_usages
        .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
    {
        common::skip_or_panic(
            module_path!(),
            "Depth24PlusStencil8 not supported as a render attachment",
        );
        return None;
    }

    let downlevel_flags = adapter.get_downlevel_capabilities().flags;

    // The D3D9 executor's constants uniform buffer exceeds wgpu's downlevel default 16 KiB binding
    // size.
    let mut required_limits = wgpu::Limits::downlevel_defaults();
    required_limits.max_uniform_buffer_binding_size =
        required_limits.max_uniform_buffer_binding_size.max(18432);

    let (device, queue) = match pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-gpu AerogpuD3d9Executor two-sided stencil test"),
            required_features: wgpu::Features::empty(),
            required_limits,
        },
        None,
    )) {
        Ok(v) => v,
        Err(err) => panic!("request_device failed: {err}"),
    };

    Some(AerogpuD3d9Executor::new(
        device,
        queue,
        downlevel_flags,
        Arc::new(GpuStats::new()),
    ))
}

#[test]
fn d3d9_cmd_stream_two_sided_stencil_mode() {
    let Some(mut exec) = create_executor_with_d24s8_support() else {
        return;
    };

    // Protocol constants from `aero-protocol`.
    const OPC_CREATE_BUFFER: u32 = cmd::AerogpuCmdOpcode::CreateBuffer as u32;
    const OPC_CREATE_TEXTURE2D: u32 = cmd::AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = cmd::AerogpuCmdOpcode::UploadResource as u32;
    const OPC_CREATE_SHADER_DXBC: u32 = cmd::AerogpuCmdOpcode::CreateShaderDxbc as u32;
    const OPC_BIND_SHADERS: u32 = cmd::AerogpuCmdOpcode::BindShaders as u32;
    const OPC_SET_SHADER_CONSTANTS_F: u32 = cmd::AerogpuCmdOpcode::SetShaderConstantsF as u32;
    const OPC_CREATE_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::CreateInputLayout as u32;
    const OPC_SET_INPUT_LAYOUT: u32 = cmd::AerogpuCmdOpcode::SetInputLayout as u32;
    const OPC_SET_RENDER_TARGETS: u32 = cmd::AerogpuCmdOpcode::SetRenderTargets as u32;
    const OPC_SET_VIEWPORT: u32 = cmd::AerogpuCmdOpcode::SetViewport as u32;
    const OPC_SET_VERTEX_BUFFERS: u32 = cmd::AerogpuCmdOpcode::SetVertexBuffers as u32;
    const OPC_SET_PRIMITIVE_TOPOLOGY: u32 = cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32;
    const OPC_SET_RENDER_STATE: u32 = cmd::AerogpuCmdOpcode::SetRenderState as u32;
    const OPC_CLEAR: u32 = cmd::AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = cmd::AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = cmd::AerogpuCmdOpcode::Present as u32;

    // D3D9 render state IDs (subset).
    const D3DRS_CULLMODE: u32 = 22;
    const D3DRS_FRONTCOUNTERCLOCKWISE: u32 = 18;
    const D3DRS_COLORWRITEENABLE: u32 = 168;

    const D3DRS_STENCILENABLE: u32 = 52;
    const D3DRS_STENCILFAIL: u32 = 53;
    const D3DRS_STENCILZFAIL: u32 = 54;
    const D3DRS_STENCILPASS: u32 = 55;
    const D3DRS_STENCILFUNC: u32 = 56;
    const D3DRS_STENCILREF: u32 = 57;
    const D3DRS_STENCILMASK: u32 = 58;
    const D3DRS_STENCILWRITEMASK: u32 = 59;

    const D3DRS_TWOSIDEDSTENCILMODE: u32 = 185;
    const D3DRS_CCW_STENCILFAIL: u32 = 186;
    const D3DRS_CCW_STENCILZFAIL: u32 = 187;
    const D3DRS_CCW_STENCILPASS: u32 = 188;
    const D3DRS_CCW_STENCILFUNC: u32 = 189;

    // D3D9 enums.
    const D3DCULL_NONE: u32 = 1;
    const D3DCMP_EQUAL: u32 = 3;
    const D3DCMP_ALWAYS: u32 = 8;
    const D3DSTENCILOP_KEEP: u32 = 1;
    const D3DSTENCILOP_REPLACE: u32 = 3;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = pci::AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_FORMAT_D24_UNORM_S8_UINT: u32 = pci::AerogpuFormat::D24UnormS8Uint as u32;
    const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = cmd::AEROGPU_RESOURCE_USAGE_TEXTURE;
    const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
    const AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL: u32 = cmd::AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
    const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = cmd::AerogpuPrimitiveTopology::TriangleList as u32;
    const AEROGPU_CLEAR_COLOR: u32 = cmd::AEROGPU_CLEAR_COLOR;
    const AEROGPU_CLEAR_DEPTH: u32 = cmd::AEROGPU_CLEAR_DEPTH;
    const AEROGPU_CLEAR_STENCIL: u32 = cmd::AEROGPU_CLEAR_STENCIL;

    const RT_HANDLE: u32 = 1;
    const DS_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let vertex_decl = vertex_decl_pos();
    let vb_data = two_sided_stencil_test_vertices();
    let vs_bytes = assemble_vs_passthrough_pos();
    let ps_bytes = assemble_ps_solid_color_c0();

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

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DS_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL);
            push_u32(out, AEROGPU_FORMAT_D24_UNORM_S8_UINT);
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
            push_u32(out, cmd::AerogpuShaderStage::Vertex as u32);
            push_u32(out, vs_bytes.len() as u32);
            push_u32(out, 0); // reserved0
            out.extend_from_slice(&vs_bytes);
        });

        emit_packet(out, OPC_CREATE_SHADER_DXBC, |out| {
            push_u32(out, PS_HANDLE);
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
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
            push_u32(out, DS_HANDLE); // depth_stencil
            push_u32(out, RT_HANDLE);
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

        // Red background; clear stencil to 0.
        emit_packet(out, OPC_CLEAR, |out| {
            push_u32(
                out,
                AEROGPU_CLEAR_COLOR | AEROGPU_CLEAR_DEPTH | AEROGPU_CLEAR_STENCIL,
            );
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 1.0); // depth
            push_u32(out, 0); // stencil
        });

        // Disable culling so both windings render; set non-default front-face winding to validate
        // that CCW stencil state is keyed to winding (not "back face").
        for (state, value) in [
            (D3DRS_FRONTCOUNTERCLOCKWISE, 1),
            (D3DRS_CULLMODE, D3DCULL_NONE),
        ] {
            emit_packet(out, OPC_SET_RENDER_STATE, |out| {
                push_u32(out, state);
                push_u32(out, value);
            });
        }

        // Enable two-sided stencil, but write CW and CCW stencil values in two passes.
        //
        // Pass 1: CW writes 1, CCW keeps 0.
        for (state, value) in [
            (D3DRS_COLORWRITEENABLE, 0),
            (D3DRS_STENCILENABLE, 1),
            (D3DRS_STENCILFUNC, D3DCMP_ALWAYS),
            (D3DRS_STENCILREF, 1),
            (D3DRS_STENCILFAIL, D3DSTENCILOP_KEEP),
            (D3DRS_STENCILZFAIL, D3DSTENCILOP_KEEP),
            (D3DRS_STENCILPASS, D3DSTENCILOP_REPLACE),
            (D3DRS_STENCILMASK, 0xFF),
            (D3DRS_STENCILWRITEMASK, 0xFF),
            (D3DRS_TWOSIDEDSTENCILMODE, 1),
            (D3DRS_CCW_STENCILFUNC, D3DCMP_ALWAYS),
            (D3DRS_CCW_STENCILFAIL, D3DSTENCILOP_KEEP),
            (D3DRS_CCW_STENCILZFAIL, D3DSTENCILOP_KEEP),
            (D3DRS_CCW_STENCILPASS, D3DSTENCILOP_KEEP),
        ] {
            emit_packet(out, OPC_SET_RENDER_STATE, |out| {
                push_u32(out, state);
                push_u32(out, value);
            });
        }

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 6); // vertex_count (two triangles)
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        // Pass 2: CW keeps existing stencil, CCW writes 2.
        for (state, value) in [
            (D3DRS_STENCILREF, 2),
            (D3DRS_STENCILPASS, D3DSTENCILOP_KEEP),
            (D3DRS_CCW_STENCILPASS, D3DSTENCILOP_REPLACE),
        ] {
            emit_packet(out, OPC_SET_RENDER_STATE, |out| {
                push_u32(out, state);
                push_u32(out, value);
            });
        }

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 6); // vertex_count (two triangles)
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });

        // Fullscreen draw: stencil test == 2, output green.
        for (state, value) in [
            (D3DRS_COLORWRITEENABLE, 0xF),
            (D3DRS_STENCILFUNC, D3DCMP_EQUAL),
            (D3DRS_STENCILREF, 2),
            (D3DRS_STENCILPASS, D3DSTENCILOP_KEEP),
            (D3DRS_CCW_STENCILFUNC, D3DCMP_EQUAL),
            (D3DRS_CCW_STENCILPASS, D3DSTENCILOP_KEEP),
        ] {
            emit_packet(out, OPC_SET_RENDER_STATE, |out| {
                push_u32(out, state);
                push_u32(out, value);
            });
        }

        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, cmd::AerogpuShaderStage::Pixel as u32);
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.0);
            push_f32(out, 1.0);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
        });

        emit_packet(out, OPC_DRAW, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 6); // first_vertex (fullscreen triangle)
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

    // Right-hand side: inside the CCW triangle, should be green.
    assert_eq!(pixel_at(&rgba, width, 48, 32), [0, 255, 0, 255]);
    // Left-hand side: inside the CW triangle, should remain red.
    assert_eq!(pixel_at(&rgba, width, 16, 32), [255, 0, 0, 255]);
    // Overlap: CCW triangle wins, should be green.
    assert_eq!(pixel_at(&rgba, width, 34, 32), [0, 255, 0, 255]);
}
