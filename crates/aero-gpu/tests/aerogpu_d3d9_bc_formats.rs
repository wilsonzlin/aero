mod common;

use std::sync::Arc;

use aero_gpu::stats::GpuStats;
use aero_gpu::{AerogpuD3d9Error, AerogpuD3d9Executor};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
        AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
};

const AEROGPU_FORMAT_BC1_RGBA_UNORM: u32 = AerogpuFormat::BC1RgbaUnorm as u32;
const AEROGPU_FORMAT_BC2_RGBA_UNORM: u32 = AerogpuFormat::BC2RgbaUnorm as u32;
const AEROGPU_FORMAT_BC3_RGBA_UNORM: u32 = AerogpuFormat::BC3RgbaUnorm as u32;
const AEROGPU_FORMAT_BC7_RGBA_UNORM: u32 = AerogpuFormat::BC7RgbaUnorm as u32;

fn env_var_truthy(name: &str) -> bool {
    let Ok(raw) = std::env::var(name) else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

fn texture_compression_disabled_by_env() -> bool {
    env_var_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION")
}

async fn create_executor_no_bc_features() -> Option<AerogpuD3d9Executor> {
    common::ensure_xdg_runtime_dir();

    // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::all()
        },
        ..Default::default()
    });

    let adapter = match instance
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
    }?;

    let downlevel_flags = adapter.get_downlevel_capabilities().flags;

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aerogpu d3d9 bc test device"),
                // Do not request TEXTURE_COMPRESSION_BC so the executor must take the CPU
                // decompression fallback path.
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()?;

    Some(AerogpuD3d9Executor::new(
        device,
        queue,
        downlevel_flags,
        Arc::new(GpuStats::new()),
    ))
}

async fn create_executor_with_bc_features() -> Option<AerogpuD3d9Executor> {
    common::ensure_xdg_runtime_dir();

    // Let CI opt out of any texture compression feature paths.
    if texture_compression_disabled_by_env() {
        return None;
    }

    async fn try_create(backends: wgpu::Backends) -> Option<AerogpuD3d9Executor> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });

        // Find any adapter that supports native BC sampling.
        //
        // Note: on Linux CI we prefer GL, but most CI environments only have software adapters
        // (e.g. llvmpipe) where BC texture compression is often unreliable. Avoid selecting CPU
        // adapters on Linux so the direct-BC tests skip instead of producing false failures.
        let allow_software_adapter = !cfg!(target_os = "linux");
        for adapter in instance.enumerate_adapters(backends) {
            if !adapter
                .features()
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
            {
                continue;
            }
            if !allow_software_adapter && adapter.get_info().device_type == wgpu::DeviceType::Cpu {
                continue;
            }

            let downlevel_flags = adapter.get_downlevel_capabilities().flags;

            let Ok((device, queue)) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("aerogpu d3d9 bc (direct) test device"),
                        required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                        required_limits: wgpu::Limits::downlevel_defaults(),
                    },
                    None,
                )
                .await
            else {
                continue;
            };

            return Some(AerogpuD3d9Executor::new(
                device,
                queue,
                downlevel_flags,
                Arc::new(GpuStats::new()),
            ));
        }

        None
    }

    // Prefer GL on Linux CI (avoid crashes in some Vulkan software adapters), but fall back to
    // other backends if GL doesn't expose BC support.
    if cfg!(target_os = "linux") {
        if let Some(exec) = try_create(wgpu::Backends::GL).await {
            return Some(exec);
        }
    }

    try_create(wgpu::Backends::all()).await
}

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

fn assemble_vs_passthrough_pos_and_t0_from_c0() -> Vec<u8> {
    // vs_2_0:
    //   mov oPos, v0
    //   mov oT0, c0
    //   end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_texld_s3() -> Vec<u8> {
    // ps_2_0:
    //   texld r0, t0, s3
    //   mov oC0, r0
    //   end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 3, 0xE4), // s3
        ],
    ));
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

#[test]
fn d3d9_cmd_stream_bc1_texture_cpu_fallback_upload_and_sample() {
    // BC1 4x4 block encoding a solid red color (0xF800 in RGB565).
    let bc1_block = [
        0x00, 0xF8, // color0
        0x00, 0xF8, // color1
        0x00, 0x00, 0x00, 0x00, // indices
    ];

    run_bc_texture_cpu_fallback_upload_and_sample(
        AEROGPU_FORMAT_BC1_RGBA_UNORM,
        8, // row_pitch_bytes (1 BC1 block row)
        &bc1_block,
        [255, 0, 0, 255],
    );
}

#[test]
fn d3d9_cmd_stream_bc2_texture_cpu_fallback_upload_and_sample() {
    // BC2/DXT3 4x4 block encoding solid red with alpha=255.
    //
    // Layout:
    // - 64-bit explicit 4-bit alpha values
    // - BC1 color block
    let bc2_block = [
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // alpha nibbles (all 0xF)
        0x00, 0xF8, // color0 (red)
        0x00, 0xF8, // color1 (red)
        0x00, 0x00, 0x00, 0x00, // color indices (all 0)
    ];

    run_bc_texture_cpu_fallback_upload_and_sample(
        AEROGPU_FORMAT_BC2_RGBA_UNORM,
        16, // row_pitch_bytes (1 BC2 block row)
        &bc2_block,
        [255, 0, 0, 255],
    );
}

#[test]
fn d3d9_cmd_stream_bc1_texture_direct_upload_and_sample() {
    // BC1 4x4 block encoding a solid red color (0xF800 in RGB565).
    let bc1_block = [
        0x00, 0xF8, // color0
        0x00, 0xF8, // color1
        0x00, 0x00, 0x00, 0x00, // indices
    ];

    run_bc_texture_direct_upload_and_sample(
        AEROGPU_FORMAT_BC1_RGBA_UNORM,
        8, // row_pitch_bytes (1 BC1 block row)
        &bc1_block,
        [255, 0, 0, 255],
    );
}

#[test]
fn d3d9_cmd_stream_bc3_texture_cpu_fallback_upload_and_sample() {
    // BC3/DXT5 4x4 block encoding solid red with alpha=255.
    //
    // Layout:
    // - alpha0, alpha1
    // - 48-bit alpha indices
    // - BC1 color block
    let bc3_block = [
        0xFF, 0xFF, // alpha0, alpha1
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // alpha indices (all 0 -> alpha0)
        0x00, 0xF8, // color0 (red)
        0x00, 0xF8, // color1 (red)
        0x00, 0x00, 0x00, 0x00, // color indices (all 0)
    ];

    run_bc_texture_cpu_fallback_upload_and_sample(
        AEROGPU_FORMAT_BC3_RGBA_UNORM,
        16, // row_pitch_bytes (1 BC3 block row)
        &bc3_block,
        [255, 0, 0, 255],
    );
}

#[test]
fn d3d9_cmd_stream_bc7_texture_cpu_fallback_upload_and_sample() {
    // Pick a deterministic BC7 block whose decode output is a solid color. We don't care what the
    // color is, as long as it isn't the clear color (black) so sampling is observable.
    let bc7_block = [0xFFu8; 16];
    let decoded = aero_gpu::decompress_bc7_rgba8(4, 4, &bc7_block);
    let expected_rgba: [u8; 4] = decoded[0..4].try_into().unwrap();

    for px in decoded.chunks_exact(4) {
        assert_eq!(px, &expected_rgba);
    }
    assert_ne!(expected_rgba, [0, 0, 0, 255]);

    run_bc_texture_cpu_fallback_upload_and_sample(
        AEROGPU_FORMAT_BC7_RGBA_UNORM,
        16, // row_pitch_bytes (1 BC7 block row)
        &bc7_block,
        expected_rgba,
    );
}

#[test]
fn d3d9_cmd_stream_bc7_texture_direct_upload_and_sample() {
    // Pick a deterministic BC7 block whose decode output is a solid color. We don't care what the
    // color is, as long as it isn't the clear color (black) so sampling is observable.
    let bc7_block = [0xFFu8; 16];
    let decoded = aero_gpu::decompress_bc7_rgba8(4, 4, &bc7_block);
    let expected_rgba: [u8; 4] = decoded[0..4].try_into().unwrap();

    for px in decoded.chunks_exact(4) {
        assert_eq!(px, &expected_rgba);
    }
    assert_ne!(expected_rgba, [0, 0, 0, 255]);

    run_bc_texture_direct_upload_and_sample(
        AEROGPU_FORMAT_BC7_RGBA_UNORM,
        16, // row_pitch_bytes (1 BC7 block row)
        &bc7_block,
        expected_rgba,
    );
}

#[test]
fn d3d9_cmd_stream_bc3_texture_direct_upload_and_sample() {
    // BC3/DXT5 4x4 block encoding solid red with alpha=255.
    //
    // Layout:
    // - alpha0, alpha1
    // - 48-bit alpha indices
    // - BC1 color block
    let bc3_block = [
        0xFF, 0xFF, // alpha0, alpha1
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // alpha indices (all 0 -> alpha0)
        0x00, 0xF8, // color0 (red)
        0x00, 0xF8, // color1 (red)
        0x00, 0x00, 0x00, 0x00, // color indices (all 0)
    ];

    run_bc_texture_direct_upload_and_sample(
        AEROGPU_FORMAT_BC3_RGBA_UNORM,
        16, // row_pitch_bytes (1 BC3 block row)
        &bc3_block,
        [255, 0, 0, 255],
    );
}

#[test]
fn d3d9_cmd_stream_bc2_texture_direct_upload_and_sample() {
    // BC2/DXT3 4x4 block encoding solid red with alpha=255.
    //
    // Layout:
    // - 64-bit explicit alpha (4 bits per texel)
    // - BC1 color block
    let bc2_block = [
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // alpha (all 0xF)
        0x00, 0xF8, // color0 (red)
        0x00, 0xF8, // color1 (red)
        0x00, 0x00, 0x00, 0x00, // color indices (all 0)
    ];

    run_bc_texture_direct_upload_and_sample(
        AEROGPU_FORMAT_BC2_RGBA_UNORM,
        16, // row_pitch_bytes (1 BC2 block row)
        &bc2_block,
        [255, 0, 0, 255],
    );
}

fn run_bc_texture_cpu_fallback_upload_and_sample(
    sample_format: u32,
    sample_row_pitch_bytes: u32,
    sample_block_bytes: &[u8],
    expected_rgba: [u8; 4],
) {
    let mut exec = match pollster::block_on(create_executor_no_bc_features()) {
        Some(exec) => exec,
        None => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
    };

    run_bc_texture_upload_and_sample(
        &mut exec,
        sample_format,
        sample_row_pitch_bytes,
        sample_block_bytes,
        expected_rgba,
        true,
    );
}

fn run_bc_texture_direct_upload_and_sample(
    sample_format: u32,
    sample_row_pitch_bytes: u32,
    sample_block_bytes: &[u8],
    expected_rgba: [u8; 4],
) {
    let mut exec = match pollster::block_on(create_executor_with_bc_features()) {
        Some(exec) => exec,
        None => {
            if texture_compression_disabled_by_env() {
                common::skip_or_panic(
                    module_path!(),
                    "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set",
                );
            } else {
                common::skip_or_panic(
                    module_path!(),
                    "wgpu adapter/device with TEXTURE_COMPRESSION_BC not found",
                );
            }
            return;
        }
    };

    run_bc_texture_upload_and_sample(
        &mut exec,
        sample_format,
        sample_row_pitch_bytes,
        sample_block_bytes,
        expected_rgba,
        false,
    );
}

fn run_bc_texture_upload_and_sample(
    exec: &mut AerogpuD3d9Executor,
    sample_format: u32,
    sample_row_pitch_bytes: u32,
    sample_block_bytes: &[u8],
    expected_rgba: [u8; 4],
    expect_sample_texture_readback_ok: bool,
) {
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
    const OPC_SET_TEXTURE: u32 = AerogpuCmdOpcode::SetTexture as u32;
    const OPC_CLEAR: u32 = AerogpuCmdOpcode::Clear as u32;
    const OPC_DRAW: u32 = AerogpuCmdOpcode::Draw as u32;
    const OPC_PRESENT: u32 = AerogpuCmdOpcode::Present as u32;

    const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
    const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const SAMPLE_TEX_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let width = 64u32;
    let height = 64u32;

    let mut vb_data = Vec::new();
    // D3D9 defaults to back-face culling with clockwise front faces.
    let verts = [
        (-0.8f32, -0.2f32, 0.0f32, 1.0f32),
        (0.0f32, 0.8f32, 0.0f32, 1.0f32),
        (0.8f32, -0.2f32, 0.0f32, 1.0f32),
    ];
    for (x, y, z, w) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, z);
        push_f32(&mut vb_data, w);
    }
    assert_eq!(vb_data.len(), 3 * 16);

    let vs_bytes = assemble_vs_passthrough_pos_and_t0_from_c0();
    let ps_bytes = assemble_ps_texld_s3();

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
            push_u32(out, SAMPLE_TEX_HANDLE);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, sample_format);
            push_u32(out, 4); // width
            push_u32(out, 4); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, sample_row_pitch_bytes);
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SAMPLE_TEX_HANDLE);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, sample_block_bytes.len() as u64);
            out.extend_from_slice(sample_block_bytes);
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
            push_u64(out, vb_data.len() as u64); // size_bytes
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

        // VS c0 = vec4(0.5, 0.5, 0.0, 1.0) -> constant texcoord.
        emit_packet(out, OPC_SET_SHADER_CONSTANTS_F, |out| {
            push_u32(out, 0); // AEROGPU_SHADER_STAGE_VERTEX
            push_u32(out, 0); // start_register
            push_u32(out, 1); // vec4_count
            push_u32(out, 0); // reserved0
            push_f32(out, 0.5);
            push_f32(out, 0.5);
            push_f32(out, 0.0);
            push_f32(out, 1.0);
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

        emit_packet(out, OPC_SET_TEXTURE, |out| {
            push_u32(out, 1); // AEROGPU_SHADER_STAGE_PIXEL
            push_u32(out, 3); // slot -> s3
            push_u32(out, SAMPLE_TEX_HANDLE);
            push_u32(out, 0); // reserved0
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

    let (out_w, out_h, rgba) = pollster::block_on(exec.readback_texture_rgba8(RT_HANDLE))
        .expect("readback should succeed");
    assert_eq!((out_w, out_h), (width, height));

    let px = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    assert_eq!(px(32, 2), [0, 0, 0, 255], "top row should be background");
    assert_eq!(
        px(32, 16),
        expected_rgba,
        "inside probe should be the sampled BC texture color"
    );

    // Ensure we actually exercised the intended BC path:
    // - CPU fallback path: the BC texture is mapped to RGBA8, and readback should succeed.
    // - Native path: the texture remains BC-compressed, and RGBA8 readback should be rejected.
    let sample_readback = pollster::block_on(exec.readback_texture_rgba8(SAMPLE_TEX_HANDLE));
    if expect_sample_texture_readback_ok {
        let (w, h, bytes) = sample_readback.expect("sample texture readback should succeed");
        assert_eq!((w, h), (4, 4));
        for px in bytes.chunks_exact(4) {
            assert_eq!(px, expected_rgba);
        }
    } else {
        match sample_readback {
            Err(AerogpuD3d9Error::ReadbackUnsupported(handle)) => {
                assert_eq!(handle, SAMPLE_TEX_HANDLE);
            }
            Err(other) => panic!("expected ReadbackUnsupported, got {other:?}"),
            Ok((w, h, _)) => {
                panic!("expected ReadbackUnsupported for BC texture, got Ok({w}x{h}) instead")
            }
        }
    }
}

#[test]
fn d3d9_bc1_misaligned_copy_region_is_rejected() {
    let mut exec = match pollster::block_on(create_executor_no_bc_features()) {
        Some(exec) => exec,
        None => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    let stream = build_stream(|out| {
        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_BC1_RGBA_UNORM);
            push_u32(out, 8); // width
            push_u32(out, 8); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 16); // row_pitch_bytes (2 BC1 blocks)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
            push_u32(out, AEROGPU_FORMAT_BC1_RGBA_UNORM);
            push_u32(out, 8); // width
            push_u32(out, 8); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 16); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // src_x=2 is not 4-aligned for BC formats. The executor should reject this before wgpu
        // validation (especially in the CPU-decompression fallback path where the actual wgpu
        // textures are RGBA8 and wgpu would otherwise allow the copy).
        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 2); // src_x (misaligned)
            push_u32(out, 0); // src_y
            push_u32(out, 4); // width
            push_u32(out, 4); // height
            push_u32(out, 0); // flags
            push_u32(out, 0); // reserved0
        });
    });

    let err = exec
        .execute_cmd_stream(&stream)
        .expect_err("misaligned BC copy should fail");
    match err {
        AerogpuD3d9Error::Validation(msg) => {
            assert!(
                msg.contains("BC copy origin") || msg.contains("BC copy"),
                "unexpected validation message: {msg}"
            );
        }
        other => panic!("expected Validation error, got {other:?}"),
    }
}

#[test]
fn d3d9_bc_copy_region_reaching_mip_edge_is_allowed() {
    let mut exec = match pollster::block_on(create_executor_no_bc_features()) {
        Some(exec) => exec,
        None => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_UPLOAD_RESOURCE: u32 = AerogpuCmdOpcode::UploadResource as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const WIDTH: u32 = 5;
    const HEIGHT: u32 = 5;

    fn bc1_solid_block(rgb565: u16) -> [u8; 8] {
        let [lo, hi] = rgb565.to_le_bytes();
        [lo, hi, lo, hi, 0x00, 0x00, 0x00, 0x00]
    }

    // Build a 5x5 BC1 texture as a 2x2 grid of 4x4 blocks.
    // We make each block a different solid color so we can validate edge behavior deterministically.
    let block_red = bc1_solid_block(0xF800);
    let block_green = bc1_solid_block(0x07E0);
    let block_blue = bc1_solid_block(0x001F);
    let block_white = bc1_solid_block(0xFFFF);

    let mut src_blocks = Vec::new();
    // Block row 0 (y=0..3): [red][green]
    src_blocks.extend_from_slice(&block_red);
    src_blocks.extend_from_slice(&block_green);
    // Block row 1 (y=4..7): [blue][white]
    src_blocks.extend_from_slice(&block_blue);
    src_blocks.extend_from_slice(&block_white);
    assert_eq!(src_blocks.len(), 32);

    // Destination starts as solid red everywhere.
    let mut dst_blocks = Vec::new();
    for _ in 0..4 {
        dst_blocks.extend_from_slice(&block_red);
    }
    assert_eq!(dst_blocks.len(), 32);

    let stream = build_stream(|out| {
        for handle in [SRC_TEX, DST_TEX] {
            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, handle);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AEROGPU_FORMAT_BC1_RGBA_UNORM);
                push_u32(out, WIDTH);
                push_u32(out, HEIGHT);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 16); // row_pitch_bytes (2 BC1 blocks per row)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        }

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, src_blocks.len() as u64);
            out.extend_from_slice(&src_blocks);
        });

        emit_packet(out, OPC_UPLOAD_RESOURCE, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, dst_blocks.len() as u64);
            out.extend_from_slice(&dst_blocks);
        });

        // Copy a 1x1 region starting at (4,4) (block-aligned origin) to the same dst coords. The
        // extent is not block-aligned, but it reaches the mip edge (width=5,height=5), so it is
        // valid per WebGPU BC copy rules.
        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 4); // dst_x
            push_u32(out, 4); // dst_y
            push_u32(out, 4); // src_x
            push_u32(out, 4); // src_y
            push_u32(out, 1); // width (not block-aligned, but ends at mip edge)
            push_u32(out, 1); // height (not block-aligned, but ends at mip edge)
            push_u32(out, 0); // flags
            push_u32(out, 0); // reserved0
        });
    });

    exec.execute_cmd_stream(&stream)
        .expect("edge-aligned BC copy should succeed");

    let (src_w, src_h, src_rgba) = pollster::block_on(exec.readback_texture_rgba8(SRC_TEX))
        .expect("source readback should succeed");
    let (dst_w, dst_h, dst_rgba) = pollster::block_on(exec.readback_texture_rgba8(DST_TEX))
        .expect("dest readback should succeed");
    assert_eq!((src_w, src_h), (WIDTH, HEIGHT));
    assert_eq!((dst_w, dst_h), (WIDTH, HEIGHT));

    let px = |buf: &[u8], x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * WIDTH + x) * 4) as usize;
        buf[idx..idx + 4].try_into().unwrap()
    };

    // Sanity: source blocks decode as expected.
    assert_eq!(px(&src_rgba, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(&src_rgba, 4, 0), [0, 255, 0, 255]);
    assert_eq!(px(&src_rgba, 0, 4), [0, 0, 255, 255]);
    assert_eq!(px(&src_rgba, 4, 4), [255, 255, 255, 255]);

    // Destination starts red, and should have copied the bottom-right pixel (4,4) from the source.
    assert_eq!(px(&dst_rgba, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(&dst_rgba, 4, 4), [255, 255, 255, 255]);
}

#[test]
fn d3d9_bc_copy_region_not_reaching_mip_edge_is_rejected() {
    let mut exec = match pollster::block_on(create_executor_no_bc_features()) {
        Some(exec) => exec,
        None => {
            common::skip_or_panic(module_path!(), "wgpu adapter not found");
            return;
        }
    };

    const OPC_CREATE_TEXTURE2D: u32 = AerogpuCmdOpcode::CreateTexture2d as u32;
    const OPC_COPY_TEXTURE2D: u32 = AerogpuCmdOpcode::CopyTexture2d as u32;

    const SRC_TEX: u32 = 1;
    const DST_TEX: u32 = 2;

    const WIDTH: u32 = 5;
    const HEIGHT: u32 = 5;

    let stream = build_stream(|out| {
        for handle in [SRC_TEX, DST_TEX] {
            emit_packet(out, OPC_CREATE_TEXTURE2D, |out| {
                push_u32(out, handle);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AEROGPU_FORMAT_BC1_RGBA_UNORM);
                push_u32(out, WIDTH);
                push_u32(out, HEIGHT);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 16); // row_pitch_bytes (2 BC1 blocks per row)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        }

        // Copy a 1x1 region from the top-left corner. For BC formats the extent is not
        // block-aligned *and* does not reach the mip edge, so it must be rejected.
        emit_packet(out, OPC_COPY_TEXTURE2D, |out| {
            push_u32(out, DST_TEX);
            push_u32(out, SRC_TEX);
            push_u32(out, 0); // dst_mip_level
            push_u32(out, 0); // dst_array_layer
            push_u32(out, 0); // src_mip_level
            push_u32(out, 0); // src_array_layer
            push_u32(out, 0); // dst_x
            push_u32(out, 0); // dst_y
            push_u32(out, 0); // src_x
            push_u32(out, 0); // src_y
            push_u32(out, 1); // width (invalid for BC when not reaching edge)
            push_u32(out, 1); // height
            push_u32(out, 0); // flags
            push_u32(out, 0); // reserved0
        });
    });

    let err = exec
        .execute_cmd_stream(&stream)
        .expect_err("misaligned BC copy extent should fail");
    match err {
        AerogpuD3d9Error::Validation(msg) => assert!(
            msg.contains("BC copy width") || msg.contains("BC copy height"),
            "unexpected validation message: {msg}"
        ),
        other => panic!("expected Validation error, got {other:?}"),
    }
}
