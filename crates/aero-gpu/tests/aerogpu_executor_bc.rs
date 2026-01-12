mod common;

use aero_gpu::aerogpu_executor::AeroGpuExecutor;
use aero_gpu::{readback_rgba8, TextureRegion, VecGuestMemory};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuShaderStage,
        AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
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

fn env_truthy(name: &str) -> bool {
    let Ok(raw) = std::env::var(name) else {
        return false;
    };
    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
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

async fn create_device_queue_bc() -> Option<(wgpu::Device, wgpu::Queue)> {
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

    // Try a couple different adapter options; the default request may land on an adapter that
    // doesn't support BC compression even when another does (e.g. integrated vs discrete).
    let adapter_opts = [
        wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        },
        wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        },
        wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        },
    ];

    for opts in adapter_opts {
        let Some(adapter) = instance.request_adapter(&opts).await else {
            continue;
        };
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            continue;
        }

        let Ok((device, queue)) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aerogpu executor bc direct test device"),
                    required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
        else {
            continue;
        };

        return Some((device, queue));
    }

    None
}

fn fullscreen_triangle_vb_bytes() -> Vec<u8> {
    // Full-screen triangle (pos: vec2<f32>).
    let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
    let mut vb_bytes = Vec::new();
    for v in verts {
        vb_bytes.extend_from_slice(&v.to_le_bytes());
    }
    vb_bytes
}

fn build_bc_sample_stream(format: AerogpuFormat, bc_data: &[u8], vb_bytes: &[u8]) -> Vec<u8> {
    build_stream(|out| {
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
            push_u32(out, AerogpuShaderStage::Pixel as u32);
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
            out.extend_from_slice(vb_bytes);
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
    })
}

async fn assert_wgpu_texture_is_native_bc(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
) {
    // A 4x4 BC texture is a single 4x4 block, so the copy uses rows_per_image=1 (block row).
    //
    // If the executor fell back to an RGBA8 texture, `rows_per_image=1` would be interpreted as 1
    // texel-row and validation should fail because the copy height is 4 texel-rows.
    device.push_error_scope(wgpu::ErrorFilter::Validation);

    // Allocate enough room to satisfy both interpretations of the copy:
    // - RGBA8: 4 rows => at least bytes_per_row*(4-1)+16
    // - BC: 1 block-row => at least 8/16 bytes
    // (Buffer size isn't the thing we're trying to validate here.)
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aerogpu.executor.bc.native_copy_probe"),
        size: 1024,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("aerogpu.executor.bc.native_copy_probe.encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::Maintain::Wait);

    let err = device.pop_error_scope().await;
    assert!(
        err.is_none(),
        "expected native BC texture, got wgpu validation error: {err:?}"
    );
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

    let vb_bytes = fullscreen_triangle_vb_bytes();
    let stream = build_bc_sample_stream(format, bc_data, &vb_bytes);

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

async fn run_bc_direct_sample_test(
    test_name: &str,
    format: AerogpuFormat,
    bc_data: &[u8],
    expected_rgba: [u8; 4],
) {
    if env_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
        common::skip_or_panic(
            test_name,
            "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set; skipping native BC path tests",
        );
        return;
    }

    let (device, queue) = match create_device_queue_bc().await {
        Some(v) => v,
        None => {
            common::skip_or_panic(test_name, "no wgpu adapter supports TEXTURE_COMPRESSION_BC");
            return;
        }
    };

    let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
    let mut guest = VecGuestMemory::new(0x1000);

    let vb_bytes = fullscreen_triangle_vb_bytes();
    let stream = build_bc_sample_stream(format, bc_data, &vb_bytes);

    let report = exec.process_cmd_stream(&stream, &mut guest, None);
    assert!(report.is_ok(), "report had errors: {:#?}", report.events);

    let bc_tex = exec.texture(2).expect("sampled BC texture");
    assert_wgpu_texture_is_native_bc(exec.device(), exec.queue(), bc_tex).await;

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
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc1_cpu_fallback_and_sample"
    );
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
fn executor_upload_bc1_srgb_cpu_fallback_decodes_on_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc1_srgb_cpu_fallback_decodes_on_sample"
    );
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(TEST_NAME, "no wgpu adapter available");
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

        // A single BC1 block with color0=white, color1=black, and all indices set to 2 (the 2/3
        // white entry in 4-color mode). This decompresses to an sRGB value of 170.
        //
        // When sampled from an sRGB BC1 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc1_srgb_gray_170 = [
            0xff, 0xff, // color0 (white)
            0x00, 0x00, // color1 (black)
            0xaa, 0xaa, 0xaa, 0xaa, // indices: all 2
        ];

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

            // CREATE_TEXTURE2D (handle=2) 4x4 BC1 sRGB texture (sampled).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC1RgbaUnormSrgb as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE: BC bytes for texture 2.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, bc1_srgb_gray_170.len() as u64); // size_bytes
                out.extend_from_slice(&bc1_srgb_gray_170);
            });

            // CREATE_TEXTURE2D (handle=3) 1x1 linear render target.
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

            // SET_TEXTURE (ps slot 0) = texture 2.
            emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
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

        let px = &rgba[0..4];
        assert!(
            (95..=110).contains(&px[0]) && px[0] == px[1] && px[1] == px[2],
            "expected decoded ~103 gray, got {px:?}"
        );
        assert_eq!(px[3], 255);
    });
}

#[test]
fn executor_upload_bc1_direct_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc1_direct_and_sample");
    pollster::block_on(async {
        // Single BC1 block for a 4x4 texture. Make it solid red by setting
        // color0=color1=RGB565(255,0,0)=0xF800 and all indices to 0.
        let bc1_solid_red = [0x00, 0xF8, 0x00, 0xF8, 0x00, 0x00, 0x00, 0x00];
        let decompressed = aero_gpu::decompress_bc1_rgba8(4, 4, &bc1_solid_red);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();

        // Ensure the block is truly solid so our 1x1 sample result is stable.
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_direct_sample_test(
            TEST_NAME,
            AerogpuFormat::BC1RgbaUnorm,
            &bc1_solid_red,
            expected_rgba,
        )
        .await;
    });
}

#[test]
fn executor_upload_bc2_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc2_cpu_fallback_and_sample"
    );
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
fn executor_upload_bc2_direct_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc2_direct_and_sample");
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
        let decompressed = aero_gpu::decompress_bc2_rgba8(4, 4, &bc2_solid_red);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_direct_sample_test(
            TEST_NAME,
            AerogpuFormat::BC2RgbaUnorm,
            &bc2_solid_red,
            expected_rgba,
        )
        .await;
    });
}

#[test]
fn executor_upload_bc3_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc3_cpu_fallback_and_sample"
    );
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
fn executor_upload_bc3_direct_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc3_direct_and_sample");
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
        let decompressed = aero_gpu::decompress_bc3_rgba8(4, 4, &bc3_solid_red);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_direct_sample_test(
            TEST_NAME,
            AerogpuFormat::BC3RgbaUnorm,
            &bc3_solid_red,
            expected_rgba,
        )
        .await;
    });
}

#[test]
fn executor_upload_bc7_cpu_fallback_and_sample() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_upload_bc7_cpu_fallback_and_sample"
    );
    pollster::block_on(async {
        // Single BC7 block for a 4x4 texture. Use a simple known vector that decodes to a solid
        // color, so the executor's fixed sampling UV (0.5, 0.5) is deterministic across backends.
        let bc7_solid = [0xffu8; 16];
        let decompressed = aero_gpu::decompress_bc7_rgba8(4, 4, &bc7_solid);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();

        // Ensure the block is truly solid so our 1x1 sample result is stable.
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        // Also ensure the chosen block differs from the clear color, so the draw is observable.
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_cpu_fallback_sample_test(
            TEST_NAME,
            AerogpuFormat::BC7RgbaUnorm,
            &bc7_solid,
            expected_rgba,
        )
        .await;
    });
}

#[test]
fn executor_upload_bc7_direct_and_sample() {
    const TEST_NAME: &str = concat!(module_path!(), "::executor_upload_bc7_direct_and_sample");
    pollster::block_on(async {
        // Single BC7 block for a 4x4 texture. Use a simple known vector that decodes to a solid
        // color, so the executor's fixed sampling UV (0.5, 0.5) is deterministic across backends.
        let bc7_solid = [0xffu8; 16];
        let decompressed = aero_gpu::decompress_bc7_rgba8(4, 4, &bc7_solid);
        let expected_rgba: [u8; 4] = decompressed[0..4].try_into().unwrap();

        // Ensure the block is truly solid so our 1x1 sample result is stable.
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &expected_rgba);
        }
        // Also ensure the chosen block differs from the clear color, so the draw is observable.
        assert_ne!(expected_rgba, [0, 0, 0, 255]);

        run_bc_direct_sample_test(
            TEST_NAME,
            AerogpuFormat::BC7RgbaUnorm,
            &bc7_solid,
            expected_rgba,
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

#[test]
fn executor_falls_back_for_unaligned_bc_texture_dimensions_even_when_bc_is_enabled() {
    const TEST_NAME: &str = concat!(
        module_path!(),
        "::executor_falls_back_for_unaligned_bc_texture_dimensions_even_when_bc_is_enabled"
    );

    pollster::block_on(async {
        if env_truthy("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION") {
            common::skip_or_panic(
                TEST_NAME,
                "AERO_DISABLE_WGPU_TEXTURE_COMPRESSION is set; skipping BC-enabled test",
            );
            return;
        }

        let (device, queue) = match create_device_queue_bc().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(TEST_NAME, "no wgpu adapter supports TEXTURE_COMPRESSION_BC");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x1000);

        // Regression: wgpu/WebGPU rejects BC textures unless the base dimensions are multiples of
        // the 4x4 block size. Creating a 9x9 BC1 texture must not panic; the executor should
        // transparently fall back to an RGBA8 texture + CPU decompression.
        let stream = build_stream(|out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE); // usage_flags
                push_u32(out, AerogpuFormat::BC1RgbaUnorm as u32); // format
                push_u32(out, 9); // width (not block-aligned)
                push_u32(out, 9); // height (not block-aligned)
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        });

        let report = exec.process_cmd_stream(&stream, &mut guest, None);
        assert!(report.is_ok(), "report had errors: {:#?}", report.events);
    });
}
