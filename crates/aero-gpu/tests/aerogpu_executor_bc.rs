mod common;

use aero_gpu::aerogpu_executor::AeroGpuExecutor;
use aero_gpu::{readback_rgba8, TextureRegion, VecGuestMemory};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32_bits(out: &mut Vec<u8>, v: f32) {
    push_u32(out, v.to_bits());
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);

    while !(out.len() - start).is_multiple_of(4) {
        out.push(0);
    }

    let size_bytes = (out.len() - start) as u32;
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut out, 0); // size_bytes placeholder
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

async fn create_device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
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

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aerogpu executor bc test device"),
                // Do not enable TEXTURE_COMPRESSION_BC so the executor must take the CPU
                // decompression fallback path.
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()?;

    Some((device, queue))
}

async fn run_bc_cpu_fallback_sample_test(
    test_name: &str,
    format: AerogpuFormat,
    bc_data: &[u8],
    expected_rgba: [u8; 4],
) {
    let (device, queue) = match create_device_queue().await {
        Some(v) => v,
        None => {
            common::skip_or_panic(test_name, "no wgpu adapter available");
            return;
        }
    };

    let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
    let mut guest = VecGuestMemory::new(0x1000);

    // Full-screen triangle (pos: vec2<f32>).
    let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
    let mut vb_bytes = Vec::new();
    for v in verts {
        vb_bytes.extend_from_slice(&v.to_le_bytes());
    }

    let stream = build_stream(|out| {
        // CREATE_BUFFER (handle=1) vertex buffer.
        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
            push_u32(out, 1); // buffer_handle
            push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER); // usage_flags
            push_u64(out, vb_bytes.len() as u64); // size_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // CREATE_TEXTURE2D (handle=2) 4x4 BC texture (sampled).
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 2); // texture_handle
            push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
            push_u32(out, format as u32); // format
            push_u32(out, 4); // width
            push_u32(out, 4); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // UPLOAD_RESOURCE: BC bytes for texture 2.
        emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
            push_u32(out, 2); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, bc_data.len() as u64); // size_bytes
            out.extend_from_slice(bc_data);
        });

        // CREATE_TEXTURE2D (handle=3) 1x1 render target.
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
            push_u32(out, 3); // texture_handle
            push_u32(out, AEROGPU_RESOURCE_USAGE_RENDER_TARGET); // usage_flags
            push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
            push_u32(out, 1); // width
            push_u32(out, 1); // height
            push_u32(out, 1); // mip_levels
            push_u32(out, 1); // array_layers
            push_u32(out, 0); // row_pitch_bytes
            push_u32(out, 0); // backing_alloc_id
            push_u32(out, 0); // backing_offset_bytes
            push_u64(out, 0); // reserved0
        });

        // SET_RENDER_TARGETS: color0 = texture 3.
        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
            push_u32(out, 1); // color_count
            push_u32(out, 0); // depth_stencil
            push_u32(out, 3); // colors[0]
            for _ in 1..8 {
                push_u32(out, 0);
            }
        });

        // CLEAR to black.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
            push_u32(out, 1); // flags: CLEAR_COLOR
            push_f32_bits(out, 0.0);
            push_f32_bits(out, 0.0);
            push_f32_bits(out, 0.0);
            push_f32_bits(out, 1.0);
            push_f32_bits(out, 1.0); // depth (unused)
            push_u32(out, 0); // stencil
        });

        // SET_TEXTURE (ps slot 0) = texture 2.
        emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
            push_u32(out, 1); // shader_stage: PIXEL
            push_u32(out, 0); // slot
            push_u32(out, 2); // texture handle
            push_u32(out, 0); // reserved0
        });

        // UPLOAD_RESOURCE vertex buffer bytes.
        emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
            push_u32(out, 1); // resource_handle
            push_u32(out, 0); // reserved0
            push_u64(out, 0); // offset_bytes
            push_u64(out, vb_bytes.len() as u64); // size_bytes
            out.extend_from_slice(&vb_bytes);
        });

        // SET_VERTEX_BUFFERS: slot 0 = buffer 1.
        emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
            push_u32(out, 0); // start_slot
            push_u32(out, 1); // buffer_count
            push_u32(out, 1); // binding[0].buffer
            push_u32(out, 8); // binding[0].stride_bytes
            push_u32(out, 0); // binding[0].offset_bytes
            push_u32(out, 0); // binding[0].reserved0
        });

        // DRAW: 3 vertices.
        emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
            push_u32(out, 3); // vertex_count
            push_u32(out, 1); // instance_count
            push_u32(out, 0); // first_vertex
            push_u32(out, 0); // first_instance
        });
    });

    let report = exec.process_cmd_stream(&stream, &mut guest, None);
    assert!(report.is_ok(), "report had errors: {:#?}", report.events);

    let rt_tex = exec.texture(3).expect("render target texture");
    let rgba = readback_rgba8(
        exec.device(),
        exec.queue(),
        rt_tex,
        TextureRegion {
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        },
    )
    .await;
    assert_eq!(&rgba[0..4], &expected_rgba);
}

#[test]
fn executor_upload_bc1_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc1_cpu_fallback_and_sample");
    pollster::block_on(async {
        // Single BC1 block for a 4x4 texture. Make it solid red by setting
        // color0=color1=RGB565(255,0,0)=0xF800 and all indices to 0.
        let bc1_solid_red = [0x00, 0xF8, 0x00, 0xF8, 0x00, 0x00, 0x00, 0x00];
        run_bc_cpu_fallback_sample_test(
            TEST_NAME,
            AerogpuFormat::BC1RgbaUnorm,
            &bc1_solid_red,
            [255, 0, 0, 255],
        )
        .await;
    });
}

#[test]
fn executor_upload_bc2_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc2_cpu_fallback_and_sample");
    pollster::block_on(async {
        // BC2 / DXT3 block for a 4x4 texture.
        // Alpha block: explicit 4-bit alpha with all texels set to 0xF (255).
        // Color: solid red with color0=color1=RGB565(255,0,0)=0xF800 and all indices 0.
        let bc2_solid_red = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // alpha (64 bits)
            0x00, 0xf8, // color0
            0x00, 0xf8, // color1
            0x00, 0x00, 0x00, 0x00, // indices
        ];
        run_bc_cpu_fallback_sample_test(
            TEST_NAME,
            AerogpuFormat::BC2RgbaUnorm,
            &bc2_solid_red,
            [255, 0, 0, 255],
        )
        .await;
    });
}

#[test]
fn executor_upload_bc3_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc3_cpu_fallback_and_sample");
    pollster::block_on(async {
        // BC3 / DXT5 block for a 4x4 texture.
        // Alpha: alpha0=255, alpha1=0, indices all 0 -> alpha 255.
        // Color: solid red with color0=color1=RGB565(255,0,0)=0xF800 and all indices 0.
        let bc3_solid_red = [
            0xff, 0x00, // alpha0, alpha1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // alpha indices (48-bit LE)
            0x00, 0xf8, // color0
            0x00, 0xf8, // color1
            0x00, 0x00, 0x00, 0x00, // color indices
        ];
        run_bc_cpu_fallback_sample_test(
            TEST_NAME,
            AerogpuFormat::BC3RgbaUnorm,
            &bc3_solid_red,
            [255, 0, 0, 255],
        )
        .await;
    });
}

#[test]
fn executor_rejects_misaligned_bc_copy_region() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x1000);

        let stream = build_stream(|out| {
            // CREATE_TEXTURE2D src (handle=1) BC1.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32);
                push_u32(out, 8);
                push_u32(out, 8);
                push_u32(out, 1);
                push_u32(out, 1);
                push_u32(out, 0);
                push_u32(out, 0);
                push_u32(out, 0);
                push_u64(out, 0);
            });

            // CREATE_TEXTURE2D dst (handle=2) BC1.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32);
                push_u32(out, 8);
                push_u32(out, 8);
                push_u32(out, 1);
                push_u32(out, 1);
                push_u32(out, 0);
                push_u32(out, 0);
                push_u32(out, 0);
                push_u64(out, 0);
            });

            // COPY_TEXTURE2D: misaligned src_x=2 (must be multiple of 4 for BC).
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 2); // dst_texture
                push_u32(out, 1); // src_texture
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

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(
            !report.is_ok(),
            "expected error for misaligned BC copy, got ok report"
        );
        let msg = match report.events.first() {
            Some(aero_gpu::aerogpu_executor::ExecutorEvent::Error { message, .. }) => message,
            other => panic!("expected error event, got {other:?}"),
        };
        assert!(
            msg.contains("BC origin") || msg.contains("BC width") || msg.contains("BC height"),
            "unexpected error message: {msg}"
        );
    });
}
