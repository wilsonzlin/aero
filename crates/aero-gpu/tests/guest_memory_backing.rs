mod common;

use aero_gpu::aerogpu_executor::{AeroGpuExecutor, ExecutorEvent};
use aero_gpu::{readback_rgba8, GuestMemory, TextureRegion, VecGuestMemory};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
        AEROGPU_COPY_FLAG_WRITEBACK_DST,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
    aerogpu_ring::{
        AerogpuAllocEntry as ProtocolAllocEntry,
        AerogpuAllocTableHeader as ProtocolAllocTableHeader, AEROGPU_ALLOC_FLAG_READONLY,
        AEROGPU_ALLOC_TABLE_MAGIC,
    },
};

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);
const ALLOC_TABLE_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolAllocTableHeader, size_bytes);

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32_bits(out: &mut Vec<u8>, v: f32) {
    push_u32(out, v.to_bits());
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

    // Pad to 4-byte alignment.
    while !(out.len() - start).is_multiple_of(4) {
        out.push(0);
    }

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert!(size_bytes.is_multiple_of(4));
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

async fn create_device_queue() -> Option<(wgpu::Device, wgpu::Queue)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(|v| v.is_empty())
            .unwrap_or(true);

        if needs_runtime_dir {
            let dir = std::env::temp_dir().join(format!(
                "aero-gpu-guest-backing-xdg-runtime-{}",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
    }

    // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: if cfg!(target_os = "linux") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::all()
        },
        dx12_shader_compiler: Default::default(),
        flags: wgpu::InstanceFlags::default(),
        gles_minor_version: wgpu::Gles3MinorVersion::Automatic,
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
                label: Some("aero-gpu guest backing test device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )
        .await
        .ok()?;

    Some((device, queue))
}

#[test]
fn resource_dirty_range_uploads_from_guest_memory_before_draw() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        // Guest memory + allocation table.
        let mut guest = VecGuestMemory::new(0x20_000);
        const ALLOC_VB: u32 = 1;
        const ALLOC_TEX: u32 = 2;
        let vb_gpa = 0x1000u64;
        let tex_gpa = 0x2000u64;
        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 2); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_VB);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, vb_gpa);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            push_u32(&mut out, ALLOC_TEX);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, tex_gpa);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }
        guest.write(vb_gpa, &vb_bytes).expect("write vertex data");

        // 1x1 RGBA8 texture, solid red.
        guest
            .write(tex_gpa, &[255, 0, 0, 255])
            .expect("write texture data");

        // Build a minimal command stream that draws using the guest-backed resources.
        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) backed by ALLOC_VB.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, ALLOC_VB); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) backed by ALLOC_TEX.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 4); // row_pitch_bytes
                push_u32(out, ALLOC_TEX); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=3) host-owned render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // RESOURCE_DIRTY_RANGE for buffer and texture.
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
            });
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
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

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let rgba = {
            let rt_tex = exec.texture(3).expect("render target texture");
            readback_rgba8(
                exec.device(),
                exec.queue(),
                rt_tex,
                TextureRegion {
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    size: wgpu::Extent3d {
                        width: 4,
                        height: 4,
                        depth_or_array_layers: 1,
                    },
                },
            )
            .await
        };

        // Sample the center pixel and ensure it matches the uploaded texture (solid red).
        let idx = ((2 * 4 + 2) * 4) as usize;
        assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);
    });
}

#[test]
fn alloc_table_is_resolved_per_submission_instead_of_caching_gpa() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(
                    concat!(
                        module_path!(),
                        "::alloc_table_is_resolved_per_submission_instead_of_caching_gpa"
                    ),
                    "no wgpu adapter available",
                );
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        // Guest memory containing two potential backing locations for the same alloc_id.
        let mut guest = VecGuestMemory::new(0x20_000);
        const ALLOC_TEX: u32 = 1;
        let tex_gpa_a = 0x1000u64;
        let tex_gpa_b = 0x2000u64;
        guest
            .write(tex_gpa_a, &[255, 0, 0, 255])
            .expect("write red texel");
        guest
            .write(tex_gpa_b, &[0, 255, 0, 255])
            .expect("write green texel");

        fn build_single_entry_alloc_table(alloc_id: u32, gpa: u64, size_bytes: u64) -> Vec<u8> {
            let mut out = Vec::new();
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            push_u32(&mut out, alloc_id);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, gpa);
            push_u64(&mut out, size_bytes);
            push_u64(&mut out, 0); // reserved0

            let total_size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&total_size_bytes.to_le_bytes());
            out
        }

        let alloc_table_gpa = 0x8000u64;

        // First submission: create resources and draw using alloc_id -> tex_gpa_a (red).
        let alloc_table_a = build_single_entry_alloc_table(ALLOC_TEX, tex_gpa_a, 0x100);
        guest
            .write(alloc_table_gpa, &alloc_table_a)
            .expect("write alloc table A");

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        let stream_a = build_stream(|out| {
            // CREATE_BUFFER (handle=1) host-owned vertex buffer.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) guest-backed sampled texture (1x1).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 4); // row_pitch_bytes
                push_u32(out, ALLOC_TEX); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=3) host-owned render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE vertex buffer.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // RESOURCE_DIRTY_RANGE for texture 2 (forces upload from alloc table mapping).
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
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

        let cmd_a_gpa = 0x9000u64;
        guest
            .write(cmd_a_gpa, &stream_a)
            .expect("write cmd stream A");
        let report_a = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_a_gpa,
            stream_a.len() as u32,
            alloc_table_gpa,
            alloc_table_a.len() as u32,
        );
        assert!(
            report_a.is_ok(),
            "submission A errors: {:#?}",
            report_a.events
        );

        // Verify the first draw sampled red from tex_gpa_a.
        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;
        let idx = ((2 * 4 + 2) * 4) as usize;
        assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);

        // Second submission: remap alloc_id -> tex_gpa_b (green) and re-upload via dirty range.
        let alloc_table_b = build_single_entry_alloc_table(ALLOC_TEX, tex_gpa_b, 0x100);
        guest
            .write(alloc_table_gpa, &alloc_table_b)
            .expect("write alloc table B");

        let stream_b = build_stream(|out| {
            // RESOURCE_DIRTY_RANGE for texture 2 (forces upload from the new alloc table mapping).
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
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

        let cmd_b_gpa = 0xA000u64;
        guest
            .write(cmd_b_gpa, &stream_b)
            .expect("write cmd stream B");
        let report_b = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_b_gpa,
            stream_b.len() as u32,
            alloc_table_gpa,
            alloc_table_b.len() as u32,
        );
        assert!(
            report_b.is_ok(),
            "submission B errors: {:#?}",
            report_b.events
        );

        let rgba = {
            let rt_tex = exec.texture(3).expect("render target texture");
            readback_rgba8(
                exec.device(),
                exec.queue(),
                rt_tex,
                TextureRegion {
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    size: wgpu::Extent3d {
                        width: 4,
                        height: 4,
                        depth_or_array_layers: 1,
                    },
                },
            )
            .await
        };
        let idx = ((2 * 4 + 2) * 4) as usize;
        assert_eq!(&rgba[idx..idx + 4], &[0, 255, 0, 255]);
    });
}

#[test]
fn copy_buffer_writeback_roundtrips_bytes() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x20_000);

        const ALLOC_DST: u32 = 1;
        let dst_gpa = 0x1000u64;
        let dst_size = 256usize;
        let dst_init = vec![0xEEu8; dst_size];
        guest.write(dst_gpa, &dst_init).expect("write dst init");

        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_DST);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, dst_gpa);
            push_u64(&mut out, dst_size as u64);
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        let src_pattern: Vec<u8> = (0u8..=255u8).collect();
        assert_eq!(src_pattern.len(), dst_size);

        let src_offset = 16u64;
        let dst_offset = 32u64;
        let copy_size = 64u64;

        let stream = build_stream(|out| {
            // CREATE_BUFFER SRC (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, dst_size as u64);
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE to SRC.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, src_pattern.len() as u64);
                out.extend_from_slice(&src_pattern);
            });

            // CREATE_BUFFER DST (handle=2) guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 2); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, dst_size as u64);
                push_u32(out, ALLOC_DST);
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_BUFFER with WRITEBACK_DST.
            emit_packet(out, AerogpuCmdOpcode::CopyBuffer as u32, |out| {
                push_u32(out, 2); // dst_buffer
                push_u32(out, 1); // src_buffer
                push_u64(out, dst_offset);
                push_u64(out, src_offset);
                push_u64(out, copy_size);
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
                push_u32(out, 0); // reserved0
            });
        });

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let mut got = vec![0u8; dst_size];
        guest.read(dst_gpa, &mut got).expect("read dst bytes");

        let dst_offset_usize = dst_offset as usize;
        let src_offset_usize = src_offset as usize;
        let copy_size_usize = copy_size as usize;

        assert_eq!(got[..dst_offset_usize], dst_init[..dst_offset_usize]);
        assert_eq!(
            got[dst_offset_usize..dst_offset_usize + copy_size_usize],
            src_pattern[src_offset_usize..src_offset_usize + copy_size_usize]
        );
        assert_eq!(
            got[dst_offset_usize + copy_size_usize..],
            dst_init[dst_offset_usize + copy_size_usize..]
        );
    });
}

#[test]
fn copy_texture2d_writeback_roundtrips_bytes() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x20_000);

        const ALLOC_DST: u32 = 1;
        let width = 7u32;
        let height = 4u32;
        let row_pitch = 32u32; // larger than width*4; not wgpu-aligned

        let dst_gpa = 0x1000u64;
        let dst_size = (row_pitch * height) as usize;
        let dst_init = vec![0x11u8; dst_size];
        guest
            .write(dst_gpa, &dst_init)
            .expect("write dst init texture bytes");

        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_DST);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, dst_gpa);
            push_u64(&mut out, dst_size as u64);
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        // Patterned RGBA8 source texture (tightly packed).
        let mut src_bytes = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                src_bytes[idx] = x as u8;
                src_bytes[idx + 1] = y as u8;
                src_bytes[idx + 2] = (x as u8).wrapping_add(y as u8);
                src_bytes[idx + 3] = 0xFF;
            }
        }

        let dst_x = 1u32;
        let dst_y = 2u32;
        let src_x = 2u32;
        let src_y = 1u32;
        let copy_w = 3u32;
        let copy_h = 2u32;

        let stream = build_stream(|out| {
            // CREATE_TEXTURE2D SRC (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE to SRC.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, src_bytes.len() as u64);
                out.extend_from_slice(&src_bytes);
            });

            // CREATE_TEXTURE2D DST (handle=2) guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, row_pitch);
                push_u32(out, ALLOC_DST);
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_TEXTURE2D with WRITEBACK_DST.
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 2); // dst_texture
                push_u32(out, 1); // src_texture
                push_u32(out, 0); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 0); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, dst_x);
                push_u32(out, dst_y);
                push_u32(out, src_x);
                push_u32(out, src_y);
                push_u32(out, copy_w);
                push_u32(out, copy_h);
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
                push_u32(out, 0); // reserved0
            });
        });

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let mut got = vec![0u8; dst_size];
        guest.read(dst_gpa, &mut got).expect("read dst bytes");

        for y in 0..height {
            for x in 0..width {
                let idx = (y as usize * row_pitch as usize) + (x as usize * 4);
                let px = &got[idx..idx + 4];

                let in_rect =
                    x >= dst_x && x < (dst_x + copy_w) && y >= dst_y && y < (dst_y + copy_h);
                if in_rect {
                    let sx = src_x + (x - dst_x);
                    let sy = src_y + (y - dst_y);
                    let expected = [sx as u8, sy as u8, (sx as u8).wrapping_add(sy as u8), 0xFF];
                    assert_eq!(px, expected);
                } else {
                    assert_eq!(px, [0x11, 0x11, 0x11, 0x11]);
                }
            }

            // Verify the row padding bytes were not clobbered.
            let pad_start = (y as usize * row_pitch as usize) + (width as usize * 4);
            let pad_end = (y as usize + 1) * row_pitch as usize;
            assert!(pad_start <= pad_end);
            assert_eq!(&got[pad_start..pad_end], &dst_init[pad_start..pad_end]);
        }
    });
}

#[test]
fn copy_buffer_executes_before_draw() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        // Host-owned buffers are updated through UPLOAD_RESOURCE, so no alloc table is needed.
        let mut guest = VecGuestMemory::new(0x20_000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }
        let zero_bytes = vec![0u8; vb_bytes.len()];

        // Build a minimal command stream that copies a vertex buffer and then draws with it.
        let stream = build_stream(|out| {
            // CREATE_BUFFER src (handle=1).
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_BUFFER dst (handle=2).
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 2); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE: initialize dst with zeros (deterministic baseline).
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, zero_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&zero_bytes);
            });

            // UPLOAD_RESOURCE: fill src with vertex data.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // COPY_BUFFER: dst <- src.
            emit_packet(out, AerogpuCmdOpcode::CopyBuffer as u32, |out| {
                push_u32(out, 2); // dst_buffer
                push_u32(out, 1); // src_buffer
                push_u64(out, 0); // dst_offset_bytes
                push_u64(out, 0); // src_offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // flags
                push_u32(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=3) 1x1 host-owned texture (solid red).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE texture 3: solid red.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 3); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
                out.extend_from_slice(&[255, 0, 0, 255]);
            });

            // CREATE_TEXTURE2D (handle=4) host-owned render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 4); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // SET_RENDER_TARGETS: color0 = texture 4.
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 4); // colors[0]
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

            // SET_TEXTURE (ps slot 0) = texture 3.
            emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                push_u32(out, 1); // shader_stage: PIXEL
                push_u32(out, 0); // slot
                push_u32(out, 3); // texture handle
                push_u32(out, 0); // reserved0
            });

            // SET_VERTEX_BUFFERS: slot 0 = buffer 2.
            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, 2); // binding[0].buffer
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

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            /*alloc_table_gpa=*/ 0,
            /*alloc_table_size_bytes=*/ 0,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let rt_tex = exec.texture(4).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        // Sample the center pixel and ensure it matches the uploaded texture (solid red).
        let idx = ((2 * 4 + 2) * 4) as usize;
        assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);
    });
}

#[test]
fn copy_texture2d_executes_before_draw() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x20_000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        let stream = build_stream(|out| {
            // Vertex buffer (handle=4).
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 4); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 4); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // Source texture (handle=1) = red.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
                out.extend_from_slice(&[255, 0, 0, 255]);
            });

            // Destination texture (handle=2) = blue (then overwritten by COPY_TEXTURE2D).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
                out.extend_from_slice(&[0, 0, 255, 255]);
            });

            // COPY_TEXTURE2D: dst <- src.
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 2); // dst_texture
                push_u32(out, 1); // src_texture
                push_u32(out, 0); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 0); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 0); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 0); // src_x
                push_u32(out, 0); // src_y
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 0); // flags
                push_u32(out, 0); // reserved0
            });

            // Render target (handle=3).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
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

            // SET_TEXTURE (ps slot 0) = texture 2 (copied).
            emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                push_u32(out, 1); // shader_stage: PIXEL
                push_u32(out, 0); // slot
                push_u32(out, 2); // texture handle
                push_u32(out, 0); // reserved0
            });

            // SET_VERTEX_BUFFERS: slot 0 = buffer 4.
            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, 4); // binding[0].buffer
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

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            /*alloc_table_gpa=*/ 0,
            /*alloc_table_size_bytes=*/ 0,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        // Sample the center pixel and ensure it matches the copied texture (solid red).
        let idx = ((2 * 4 + 2) * 4) as usize;
        assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);
    });
}

#[test]
fn copy_buffer_writeback_writes_guest_backing() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };
        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        let mut guest = VecGuestMemory::new(0x20_000);

        const ALLOC_DST: u32 = 1;
        const DST_GPA: u64 = 0x1000;
        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_DST);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, DST_GPA);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        guest
            .write(DST_GPA, &[0xEE; 16])
            .expect("write dst sentinel");

        let payload = *b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B\x0C\x0D\x0E\x0F";
        let stream = build_stream(|out| {
            // CREATE_BUFFER src (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_BUFFER dst (handle=2) guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 2); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, ALLOC_DST); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE src.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, payload.len() as u64); // size_bytes
                out.extend_from_slice(&payload);
            });

            // COPY_BUFFER: dst <- src (with writeback).
            emit_packet(out, AerogpuCmdOpcode::CopyBuffer as u32, |out| {
                push_u32(out, 2); // dst_buffer
                push_u32(out, 1); // src_buffer
                push_u64(out, 0); // dst_offset_bytes
                push_u64(out, 0); // src_offset_bytes
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST); // flags
                push_u32(out, 0); // reserved0
            });
        });

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let mut readback = [0u8; 16];
        guest.read(DST_GPA, &mut readback).expect("read writeback");
        assert_eq!(readback, payload);
    });
}

#[test]
fn copy_buffer_writeback_requires_alloc_table_each_submit() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };
        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        let mut guest = VecGuestMemory::new(0x20_000);

        const ALLOC_DST: u32 = 1;
        const DST_GPA: u64 = 0x1000;
        let alloc_table_gpa = 0x8000u64;

        let build_alloc_table = |alloc_id: u32| -> Vec<u8> {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, alloc_id);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, DST_GPA);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };

        let alloc_table_bytes = build_alloc_table(ALLOC_DST);
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        let sentinel = [0xEEu8; 16];
        guest.write(DST_GPA, &sentinel).expect("write dst sentinel");

        let payload = *b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B\x0C\x0D\x0E\x0F";

        // Submission 1: create src host-owned buffer and dst guest-backed buffer.
        let create_stream = build_stream(|out| {
            // CREATE_BUFFER src (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_BUFFER dst (handle=2) guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 2); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, ALLOC_DST); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        });
        let cmd_create_gpa = 0x9000u64;
        guest
            .write(cmd_create_gpa, &create_stream)
            .expect("write create stream");
        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_create_gpa,
            create_stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(report.is_ok(), "create submission failed: {:#?}", report);

        // Submission 2: upload src bytes (valid without alloc_table).
        let upload_stream = build_stream(|out| {
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, payload.len() as u64); // size_bytes
                out.extend_from_slice(&payload);
            });
        });
        let cmd_upload_gpa = 0xA000u64;
        guest
            .write(cmd_upload_gpa, &upload_stream)
            .expect("write upload stream");
        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_upload_gpa,
            upload_stream.len() as u32,
            0,
            0,
        );
        assert!(report.is_ok(), "upload submission failed: {:#?}", report);

        // Submission 3: COPY_BUFFER with WRITEBACK_DST but *no* alloc table.
        let copy_stream = build_stream(|out| {
            emit_packet(out, AerogpuCmdOpcode::CopyBuffer as u32, |out| {
                push_u32(out, 2); // dst_buffer
                push_u32(out, 1); // src_buffer
                push_u64(out, 0); // dst_offset_bytes
                push_u64(out, 0); // src_offset_bytes
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST); // flags
                push_u32(out, 0); // reserved0
            });
        });

        let cmd_copy_missing_table_gpa = 0xB000u64;
        guest
            .write(cmd_copy_missing_table_gpa, &copy_stream)
            .expect("write copy stream (missing table)");
        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_copy_missing_table_gpa,
            copy_stream.len() as u32,
            0,
            0,
        );
        assert!(!report.is_ok(), "expected missing alloc_table error");
        assert!(
            report.events.iter().any(|e| matches!(
                e,
                ExecutorEvent::Error { message, .. } if message.to_ascii_lowercase().contains("requires alloc_table")
            )),
            "expected alloc_table-required validation error, got: {:#?}",
            report.events
        );

        let mut readback = [0u8; 16];
        guest.read(DST_GPA, &mut readback).expect("read dst");
        assert_eq!(readback, sentinel);

        // Submission 4: COPY_BUFFER with WRITEBACK_DST but alloc_table missing the destination alloc_id.
        let bad_alloc_table_bytes = build_alloc_table(ALLOC_DST + 1);
        guest
            .write(alloc_table_gpa, &bad_alloc_table_bytes)
            .expect("write bad alloc table");

        let cmd_copy_missing_entry_gpa = 0xC000u64;
        guest
            .write(cmd_copy_missing_entry_gpa, &copy_stream)
            .expect("write copy stream (missing entry)");
        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_copy_missing_entry_gpa,
            copy_stream.len() as u32,
            alloc_table_gpa,
            bad_alloc_table_bytes.len() as u32,
        );
        assert!(!report.is_ok(), "expected missing alloc table entry error");
        assert!(
            report.events.iter().any(|e| matches!(
                e,
                ExecutorEvent::Error { message, .. } if {
                    let msg = message.to_ascii_lowercase();
                    msg.contains("missing alloc table entry") && msg.contains("dst_buffer")
                }
            )),
            "expected missing alloc table entry validation error, got: {:#?}",
            report.events
        );

        guest.read(DST_GPA, &mut readback).expect("read dst");
        assert_eq!(readback, sentinel);
    });
}

#[test]
fn copy_buffer_writeback_rejects_readonly_alloc() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };
        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        let mut guest = VecGuestMemory::new(0x20_000);

        const ALLOC_DST: u32 = 1;
        const DST_GPA: u64 = 0x1000;
        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_DST);
            push_u32(&mut out, AEROGPU_ALLOC_FLAG_READONLY); // flags
            push_u64(&mut out, DST_GPA);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        let sentinel = [0xEEu8; 16];
        guest.write(DST_GPA, &sentinel).expect("write dst sentinel");

        let payload = *b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0A\x0B\x0C\x0D\x0E\x0F";
        let stream = build_stream(|out| {
            // CREATE_BUFFER src (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_BUFFER dst (handle=2) guest-backed (READONLY alloc).
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 2); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, ALLOC_DST); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE src.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, payload.len() as u64); // size_bytes
                out.extend_from_slice(&payload);
            });

            // COPY_BUFFER: dst <- src (with writeback).
            emit_packet(out, AerogpuCmdOpcode::CopyBuffer as u32, |out| {
                push_u32(out, 2); // dst_buffer
                push_u32(out, 1); // src_buffer
                push_u64(out, 0); // dst_offset_bytes
                push_u64(out, 0); // src_offset_bytes
                push_u64(out, payload.len() as u64); // size_bytes
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST); // flags
                push_u32(out, 0); // reserved0
            });
        });

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(
            !report.is_ok(),
            "expected executor report to contain errors"
        );
        assert!(
            report.events.iter().any(|e| matches!(
                e,
                ExecutorEvent::Error { message, .. } if {
                    let msg = message.to_ascii_lowercase();
                    msg.contains("readonly") || msg.contains("read-only")
                }
            )),
            "expected read-only validation error, got: {:#?}",
            report.events
        );

        let mut readback = [0u8; 16];
        guest.read(DST_GPA, &mut readback).expect("read dst");
        assert_eq!(readback, sentinel);
    });
}

#[test]
fn copy_texture2d_writeback_writes_guest_backing() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };
        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        let mut guest = VecGuestMemory::new(0x20_000);

        const ALLOC_DST: u32 = 1;
        const DST_GPA: u64 = 0x1000;
        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_DST);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, DST_GPA);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        // 2x2 RGBA8 with row_pitch_bytes=12 means 4 padding bytes at the end of each row.
        let sentinel = vec![0xEEu8; 24];
        guest.write(DST_GPA, &sentinel).expect("write dst sentinel");

        let src_pixels: [u8; 16] = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ];

        let stream = build_stream(|out| {
            // Source texture (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 2); // width
                push_u32(out, 2); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, src_pixels.len() as u64); // size_bytes
                out.extend_from_slice(&src_pixels);
            });

            // Destination texture (handle=2) guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 2); // width
                push_u32(out, 2); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 12); // row_pitch_bytes
                push_u32(out, ALLOC_DST); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_TEXTURE2D: dst <- src (with writeback).
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 2); // dst_texture
                push_u32(out, 1); // src_texture
                push_u32(out, 0); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 0); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 0); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 0); // src_x
                push_u32(out, 0); // src_y
                push_u32(out, 2); // width
                push_u32(out, 2); // height
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST); // flags
                push_u32(out, 0); // reserved0
            });
        });

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let mut readback = vec![0u8; 24];
        guest.read(DST_GPA, &mut readback).expect("read writeback");

        let mut expected = vec![0xEEu8; 24];
        expected[0..8].copy_from_slice(&src_pixels[0..8]);
        expected[12..20].copy_from_slice(&src_pixels[8..16]);
        assert_eq!(readback, expected);
    });
}

#[test]
fn copy_texture2d_writeback_encodes_x8_alpha_as_255() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };
        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        let mut guest = VecGuestMemory::new(0x20_000);

        const ALLOC_DST: u32 = 99;
        const DST_GPA: u64 = 0x1000;
        const BACKING_OFFSET_BYTES: u32 = 4;
        const ROW_PITCH_BYTES: u32 = 12;

        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_DST);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, DST_GPA);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        let backing_bytes = (BACKING_OFFSET_BYTES + ROW_PITCH_BYTES * 2) as usize;
        let sentinel = vec![0xEEu8; backing_bytes];

        let mut upload = vec![0u8; (ROW_PITCH_BYTES * 2) as usize];
        // Row 0 (y=0): pixel(0,0)=[1,2,3,0] pixel(1,0)=[4,5,6,0]
        upload[0..8].copy_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        // Row 1 (y=1): pixel(0,1)=[7,8,9,0] pixel(1,1)=[10,11,12,0]
        let row1 = ROW_PITCH_BYTES as usize;
        upload[row1..row1 + 8].copy_from_slice(&[7, 8, 9, 0, 10, 11, 12, 0]);

        let cmd_gpa = 0x9000u64;
        let pixel_off = (BACKING_OFFSET_BYTES + 4) as usize;
        let formats = [
            AerogpuFormat::B8G8R8X8Unorm,
            AerogpuFormat::B8G8R8X8UnormSrgb,
            AerogpuFormat::R8G8B8X8Unorm,
            AerogpuFormat::R8G8B8X8UnormSrgb,
        ];

        for (case_idx, format) in formats.iter().copied().enumerate() {
            // 2x2 {BGRAX/RGBAX} texture with padded rows (row_pitch=12, row_bytes=8) and a non-zero
            // backing offset. Fill with a sentinel to prove we only overwrite the single target
            // texel.
            guest.write(DST_GPA, &sentinel).expect("write dst sentinel");

            let src_handle = 100 + (case_idx as u32) * 2;
            let dst_handle = src_handle + 1;

            let stream = build_stream(|out| {
                // Source texture host-owned.
                emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                    push_u32(out, src_handle); // texture_handle
                    push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                    push_u32(out, format as u32); // format
                    push_u32(out, 2); // width
                    push_u32(out, 2); // height
                    push_u32(out, 1); // mip_levels
                    push_u32(out, 1); // array_layers
                    push_u32(out, ROW_PITCH_BYTES); // row_pitch_bytes
                    push_u32(out, 0); // backing_alloc_id
                    push_u32(out, 0); // backing_offset_bytes
                    push_u64(out, 0); // reserved0
                });
                emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                    push_u32(out, src_handle); // resource_handle
                    push_u32(out, 0); // reserved0
                    push_u64(out, 0); // offset_bytes
                    push_u64(out, upload.len() as u64); // size_bytes
                    out.extend_from_slice(&upload);
                });

                // Destination texture guest-backed with padding and backing offset.
                emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                    push_u32(out, dst_handle); // texture_handle
                    push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                    push_u32(out, format as u32); // format
                    push_u32(out, 2); // width
                    push_u32(out, 2); // height
                    push_u32(out, 1); // mip_levels
                    push_u32(out, 1); // array_layers
                    push_u32(out, ROW_PITCH_BYTES); // row_pitch_bytes
                    push_u32(out, ALLOC_DST); // backing_alloc_id
                    push_u32(out, BACKING_OFFSET_BYTES); // backing_offset_bytes
                    push_u64(out, 0); // reserved0
                });

                // Copy src pixel (0,1) into dst pixel (1,0) (with writeback).
                emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                    push_u32(out, dst_handle); // dst_texture
                    push_u32(out, src_handle); // src_texture
                    push_u32(out, 0); // dst_mip_level
                    push_u32(out, 0); // dst_array_layer
                    push_u32(out, 0); // src_mip_level
                    push_u32(out, 0); // src_array_layer
                    push_u32(out, 1); // dst_x
                    push_u32(out, 0); // dst_y
                    push_u32(out, 0); // src_x
                    push_u32(out, 1); // src_y
                    push_u32(out, 1); // width
                    push_u32(out, 1); // height
                    push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST); // flags
                    push_u32(out, 0); // reserved0
                });
            });

            guest.write(cmd_gpa, &stream).expect("write command stream");

            let report = exec.process_submission_from_guest_memory(
                &mut guest,
                cmd_gpa,
                stream.len() as u32,
                alloc_table_gpa,
                alloc_table_bytes.len() as u32,
            );
            assert!(
                report.is_ok(),
                "executor report had errors for format {format:?}: {:#?}",
                report.events
            );

            let mut readback = vec![0u8; backing_bytes];
            guest.read(DST_GPA, &mut readback).expect("read writeback");

            let mut expected = sentinel.clone();
            expected[pixel_off..pixel_off + 4].copy_from_slice(&[7, 8, 9, 255]);
            assert_eq!(readback, expected, "format {format:?}");
        }
    });
}

#[test]
fn copy_texture2d_writeback_rejects_readonly_alloc() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };
        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        let mut guest = VecGuestMemory::new(0x20_000);

        const ALLOC_DST: u32 = 1;
        const DST_GPA: u64 = 0x1000;
        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_DST);
            push_u32(&mut out, AEROGPU_ALLOC_FLAG_READONLY); // flags
            push_u64(&mut out, DST_GPA);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        // 2x2 RGBA8 with row_pitch_bytes=12 means 4 padding bytes at the end of each row.
        let sentinel = vec![0xEEu8; 24];
        guest.write(DST_GPA, &sentinel).expect("write dst sentinel");

        let src_pixels: [u8; 16] = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ];

        let stream = build_stream(|out| {
            // Source texture (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 2); // width
                push_u32(out, 2); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, src_pixels.len() as u64); // size_bytes
                out.extend_from_slice(&src_pixels);
            });

            // Destination texture (handle=2) guest-backed.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, 2); // width
                push_u32(out, 2); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 12); // row_pitch_bytes
                push_u32(out, ALLOC_DST); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_TEXTURE2D: dst <- src (with writeback).
            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, 2); // dst_texture
                push_u32(out, 1); // src_texture
                push_u32(out, 0); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 0); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 0); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 0); // src_x
                push_u32(out, 0); // src_y
                push_u32(out, 2); // width
                push_u32(out, 2); // height
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
                push_u32(out, 0); // reserved0
            });
        });

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(!report.is_ok(), "expected error report");
        assert!(
            report.events.iter().any(|e| matches!(
                e,
                ExecutorEvent::Error { message, .. } if {
                    let msg = message.to_ascii_lowercase();
                    msg.contains("readonly") || msg.contains("read-only")
                }
            )),
            "expected read-only validation error, got: {:#?}",
            report.events
        );

        let mut readback = vec![0u8; sentinel.len()];
        guest
            .read(DST_GPA, &mut readback)
            .expect("read dst sentinel");
        assert_eq!(readback, sentinel);
    });
}

#[test]
fn resource_dirty_range_uploads_guest_backed_index_buffer_before_draw_indexed() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        // Guest memory + allocation table.
        let mut guest = VecGuestMemory::new(0x30_000);
        const ALLOC_VB: u32 = 1;
        const ALLOC_TEX: u32 = 2;
        const ALLOC_IB: u32 = 3;
        let vb_gpa = 0x1000u64;
        let tex_gpa = 0x2000u64;
        let ib_gpa = 0x3000u64;
        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 3); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_VB);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, vb_gpa);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            push_u32(&mut out, ALLOC_TEX);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, tex_gpa);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            push_u32(&mut out, ALLOC_IB);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, ib_gpa);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }
        guest.write(vb_gpa, &vb_bytes).expect("write vertex data");

        // Index buffer (u16 indices), padded to 8 bytes to satisfy WebGPU write_buffer alignment.
        let ib_bytes: [u16; 4] = [0, 1, 2, 0];
        let mut ib_raw = Vec::new();
        for i in ib_bytes {
            ib_raw.extend_from_slice(&i.to_le_bytes());
        }
        assert_eq!(ib_raw.len(), 8);
        guest.write(ib_gpa, &ib_raw).expect("write index data");

        // 1x1 RGBA8 texture, solid red.
        guest
            .write(tex_gpa, &[255, 0, 0, 255])
            .expect("write texture data");

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) guest-backed vertex buffer.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, ALLOC_VB); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_BUFFER (handle=4) guest-backed index buffer.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 4); // buffer_handle
                push_u32(out, 1u32 << 1); // usage_flags: INDEX_BUFFER
                push_u64(out, ib_raw.len() as u64); // size_bytes
                push_u32(out, ALLOC_IB); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) guest-backed sampled texture.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 4); // row_pitch_bytes
                push_u32(out, ALLOC_TEX); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=3) host-owned render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // RESOURCE_DIRTY_RANGE for vb, ib, texture.
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
            });
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 4); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, ib_raw.len() as u64); // size_bytes
            });
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
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

            // SET_VERTEX_BUFFERS: slot 0 = buffer 1.
            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, 1); // binding[0].buffer
                push_u32(out, 8); // binding[0].stride_bytes
                push_u32(out, 0); // binding[0].offset_bytes
                push_u32(out, 0); // binding[0].reserved0
            });

            // SET_INDEX_BUFFER: buffer 4, uint16, offset 0.
            emit_packet(out, AerogpuCmdOpcode::SetIndexBuffer as u32, |out| {
                push_u32(out, 4); // buffer
                push_u32(out, 0); // format: UINT16
                push_u32(out, 0); // offset_bytes
                push_u32(out, 0); // reserved0
            });

            // DRAW_INDEXED: 3 indices.
            emit_packet(out, AerogpuCmdOpcode::DrawIndexed as u32, |out| {
                push_u32(out, 3); // index_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_index
                push_u32(out, 0); // base_vertex (i32 bits)
                push_u32(out, 0); // first_instance
            });
        });

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        // Sample the center pixel and ensure it matches the uploaded texture (solid red).
        let idx = ((2 * 4 + 2) * 4) as usize;
        assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);
    });
}

#[test]
fn upload_resource_updates_host_owned_resources() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x20_000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) host-owned sampled texture (1x1).
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=3) host-owned render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE buffer data.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // UPLOAD_RESOURCE texture data (solid red).
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
                out.extend_from_slice(&[255, 0, 0, 255]);
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

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            0,
            0,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        // Sample the center pixel and ensure it matches the uploaded texture (solid red).
        let idx = ((2 * 4 + 2) * 4) as usize;
        assert_eq!(&rgba[idx..idx + 4], &[255, 0, 0, 255]);
    });
}

#[test]
fn alloc_table_descriptor_requires_gpa_and_size_bytes_to_match() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x10_000);

        // Minimal valid command stream (header only).
        let stream = build_stream(|_out| {});
        let cmd_gpa = 0x1000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        // alloc_table_gpa set but size=0 must be rejected.
        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            0x2000,
            0,
        );
        assert!(
            !report.is_ok(),
            "expected error for inconsistent alloc_table_gpa/size, got ok"
        );
        assert!(
            matches!(report.events.first(), Some(ExecutorEvent::Error { message, .. }) if message.contains("alloc table")),
            "expected alloc table error, got: {:#?}",
            report.events
        );

        // alloc_table_size_bytes set but gpa=0 must also be rejected.
        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            0,
            24,
        );
        assert!(
            !report.is_ok(),
            "expected error for inconsistent alloc_table_gpa/size, got ok"
        );
        assert!(
            matches!(report.events.first(), Some(ExecutorEvent::Error { message, .. }) if message.contains("alloc table")),
            "expected alloc table error, got: {:#?}",
            report.events
        );
    });
}

#[test]
fn cmd_descriptor_requires_gpa_and_size_bytes_to_match() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };
        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x10_000);

        // cmd_gpa set but cmd_size_bytes=0 must be rejected.
        let report = exec.process_submission_from_guest_memory(&mut guest, 0x1000, 0, 0, 0);
        assert!(
            !report.is_ok(),
            "expected error for inconsistent cmd_gpa/size, got ok"
        );
        assert!(
            matches!(report.events.first(), Some(ExecutorEvent::Error { message, .. }) if message.contains("command stream descriptor")),
            "expected command stream descriptor error, got: {:#?}",
            report.events
        );

        // cmd_size_bytes set but cmd_gpa=0 must also be rejected.
        let report = exec.process_submission_from_guest_memory(&mut guest, 0, 24, 0, 0);
        assert!(
            !report.is_ok(),
            "expected error for inconsistent cmd_gpa/size, got ok"
        );
        assert!(
            matches!(report.events.first(), Some(ExecutorEvent::Error { message, .. }) if message.contains("command stream descriptor")),
            "expected command stream descriptor error, got: {:#?}",
            report.events
        );

        // Empty submissions (cmd_gpa=0, cmd_size_bytes=0) are treated as no-ops.
        let report = exec.process_submission_from_guest_memory(&mut guest, 0, 0, 0, 0);
        assert!(
            report.is_ok(),
            "expected ok for empty submission, got: {:#?}",
            report.events
        );
        assert_eq!(report.packets_processed, 0);
        assert!(report.events.is_empty());
    });
}

#[test]
fn resource_dirty_range_texture_row_pitch_is_respected() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");

        // Guest memory + allocation table.
        let mut guest = VecGuestMemory::new(0x40_000);
        const ALLOC_VB: u32 = 1;
        const ALLOC_TEX: u32 = 2;
        let vb_gpa = 0x1000u64;
        let tex_gpa = 0x2000u64;
        let alloc_table_gpa = 0x8000u64;

        let alloc_table_bytes = {
            let mut out = Vec::new();

            // aerogpu_alloc_table_header (24 bytes)
            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 2); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            // aerogpu_alloc_entry (32 bytes)
            push_u32(&mut out, ALLOC_VB);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, vb_gpa);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            push_u32(&mut out, ALLOC_TEX);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, tex_gpa);
            push_u64(&mut out, 0x100); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }
        guest.write(vb_gpa, &vb_bytes).expect("write vertex data");

        // 1x2 RGBA8 texture in guest memory with a padded row pitch.
        // row0 = red, row1 = green. Padding bytes are filled with a sentinel to ensure the
        // uploader ignores them.
        let row_pitch = 8u32;
        let mut tex_bytes = [0u8; 16];
        tex_bytes[0..4].copy_from_slice(&[255, 0, 0, 255]);
        tex_bytes[4..8].copy_from_slice(&[9, 9, 9, 9]);
        tex_bytes[8..12].copy_from_slice(&[0, 255, 0, 255]);
        tex_bytes[12..16].copy_from_slice(&[7, 7, 7, 7]);
        guest
            .write(tex_gpa, &tex_bytes)
            .expect("write texture data");

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) backed by ALLOC_VB.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, ALLOC_VB); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) backed by ALLOC_TEX.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 1); // width
                push_u32(out, 2); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, row_pitch); // row_pitch_bytes
                push_u32(out, ALLOC_TEX); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=3) host-owned render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused when backing_alloc_id == 0)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // RESOURCE_DIRTY_RANGE for buffer and texture.
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
            });
            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 16); // size_bytes
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
                push_u32(out, 1); // shader_stage: PIXEL
                push_u32(out, 0); // slot
                push_u32(out, 2); // texture handle
                push_u32(out, 0); // reserved0
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

            // DRAW: 3 vertices. This triggers the dirty-range flush.
            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });
        });

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            alloc_table_gpa,
            alloc_table_bytes.len() as u32,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let tex = exec.texture(2).expect("uploaded texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            tex,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 1,
                    height: 2,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);
    });
}

#[test]
fn draw_to_bgra_render_target_is_supported() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x20_000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        let stream = build_stream(|out| {
            // CREATE_BUFFER (handle=1) host-owned.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=2) host-owned sampled texture (1x1), RGBA.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: R8G8B8A8_UNORM
                push_u32(out, 1); // width
                push_u32(out, 1); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // CREATE_TEXTURE2D (handle=3) host-owned BGRA render target.
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 3); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::B8G8R8A8Unorm as u32); // format: B8G8R8A8_UNORM
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (unused)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE buffer data.
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });

            // UPLOAD_RESOURCE texture data (solid red RGBA).
            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, 4); // size_bytes
                out.extend_from_slice(&[255, 0, 0, 255]);
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

        let cmd_gpa = 0x9000u64;
        guest.write(cmd_gpa, &stream).expect("write command stream");

        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            stream.len() as u32,
            0,
            0,
        );
        assert!(
            report.is_ok(),
            "executor report had errors: {:#?}",
            report.events
        );

        let rt_tex = exec.texture(3).expect("render target texture");
        let rgba = readback_rgba8(
            exec.device(),
            exec.queue(),
            rt_tex,
            TextureRegion {
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
            },
        )
        .await;

        // BGRA8 render target stores bytes as B,G,R,A.
        let idx = ((2 * 4 + 2) * 4) as usize;
        assert_eq!(&rgba[idx..idx + 4], &[0, 0, 255, 255]);
    });
}

#[test]
fn executor_supports_16bit_formats_b5g6r5_and_b5g5r5a1() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x40_000);

        // Full-screen triangle (pos: vec2<f32>).
        let verts: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let mut vb_bytes = Vec::new();
        for v in verts {
            vb_bytes.extend_from_slice(&v.to_le_bytes());
        }

        // Init: create the shared vertex buffer and RGBA8 render target.
        let init_stream = build_stream(|out| {
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 1u32 << 0); // usage_flags: VERTEX_BUFFER
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 1u32 << 4); // usage_flags: RENDER_TARGET
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format: RGBA8
                push_u32(out, 4); // width
                push_u32(out, 4); // height
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&vb_bytes);
            });
        });

        let cmd_gpa = 0x9000u64;
        guest
            .write(cmd_gpa, &init_stream)
            .expect("write init stream");
        let report = exec.process_submission_from_guest_memory(
            &mut guest,
            cmd_gpa,
            init_stream.len() as u32,
            0,
            0,
        );
        assert!(report.is_ok(), "init stream failed: {:#?}", report.events);

        let formats: [(&str, u32, [u8; 2]); 2] = [
            (
                "B5G6R5Unorm",
                AerogpuFormat::B5G6R5Unorm as u32,
                0xF800u16.to_le_bytes(),
            ),
            (
                "B5G5R5A1Unorm",
                AerogpuFormat::B5G5R5A1Unorm as u32,
                0xFC00u16.to_le_bytes(),
            ),
        ];

        for (idx, (name, format_raw, pixel)) in formats.into_iter().enumerate() {
            let host_tex = 10 + idx as u32;
            let guest_tex = 20 + idx as u32;

            // 4x4 packed texture (all texels identical).
            let mut packed = Vec::with_capacity(4 * 4 * 2);
            for _ in 0..(4 * 4) {
                packed.extend_from_slice(&pixel);
            }
            assert_eq!(packed.len(), 32);

            // Host-owned upload path.
            let stream = build_stream(|out| {
                emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                    push_u32(out, host_tex); // texture_handle
                    push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                    push_u32(out, format_raw);
                    push_u32(out, 4); // width
                    push_u32(out, 4); // height
                    push_u32(out, 1); // mip_levels
                    push_u32(out, 1); // array_layers
                    push_u32(out, 8); // row_pitch_bytes (4 * 2 bytes)
                    push_u32(out, 0); // backing_alloc_id
                    push_u32(out, 0); // backing_offset_bytes
                    push_u64(out, 0); // reserved0
                });

                emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                    push_u32(out, host_tex);
                    push_u32(out, 0); // reserved0
                    push_u64(out, 0); // offset_bytes
                    push_u64(out, packed.len() as u64); // size_bytes
                    out.extend_from_slice(&packed);
                });

                emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                    push_u32(out, 1); // color_count
                    push_u32(out, 0); // depth_stencil
                    push_u32(out, 2); // colors[0]
                    for _ in 1..8 {
                        push_u32(out, 0);
                    }
                });

                emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                    push_u32(out, 1); // flags: CLEAR_COLOR
                    push_f32_bits(out, 0.0);
                    push_f32_bits(out, 0.0);
                    push_f32_bits(out, 0.0);
                    push_f32_bits(out, 1.0);
                    push_f32_bits(out, 1.0); // depth (unused)
                    push_u32(out, 0); // stencil
                });

                emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                    push_u32(out, 1); // shader_stage: PIXEL
                    push_u32(out, 0); // slot
                    push_u32(out, host_tex);
                    push_u32(out, 0); // reserved0
                });

                emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                    push_u32(out, 0); // start_slot
                    push_u32(out, 1); // buffer_count
                    push_u32(out, 1); // binding[0].buffer
                    push_u32(out, 8); // binding[0].stride_bytes
                    push_u32(out, 0); // binding[0].offset_bytes
                    push_u32(out, 0); // binding[0].reserved0
                });

                emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                    push_u32(out, 3); // vertex_count
                    push_u32(out, 1); // instance_count
                    push_u32(out, 0); // first_vertex
                    push_u32(out, 0); // first_instance
                });
            });

            guest.write(cmd_gpa, &stream).expect("write cmd stream");
            let report = exec.process_submission_from_guest_memory(
                &mut guest,
                cmd_gpa,
                stream.len() as u32,
                0,
                0,
            );
            assert!(
                report.is_ok(),
                "host-owned {name} stream failed: {:#?}",
                report.events
            );

            let rt_tex = exec.texture(2).expect("render target");
            let rgba = readback_rgba8(
                exec.device(),
                exec.queue(),
                rt_tex,
                TextureRegion {
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    size: wgpu::Extent3d {
                        width: 4,
                        height: 4,
                        depth_or_array_layers: 1,
                    },
                },
            )
            .await;
            let idx_bytes = ((2 * 4 + 2) * 4) as usize;
            assert_eq!(
                &rgba[idx_bytes..idx_bytes + 4],
                &[255, 0, 0, 255],
                "host-owned {name} sampled color mismatch"
            );

            // Guest-backed dirty-range flush path.
            const ALLOC_TEX: u32 = 1;
            let tex_gpa = 0x1000u64;
            let alloc_table_gpa = 0x8000u64;
            guest.write(tex_gpa, &packed).expect("write packed tex");

            let alloc_table_bytes = {
                let mut out = Vec::new();

                push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
                push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
                push_u32(&mut out, 0); // size_bytes (patch later)
                push_u32(&mut out, 1); // entry_count
                push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
                push_u32(&mut out, 0); // reserved0

                push_u32(&mut out, ALLOC_TEX);
                push_u32(&mut out, 0); // flags
                push_u64(&mut out, tex_gpa);
                push_u64(&mut out, 0x1000); // size_bytes
                push_u64(&mut out, 0); // reserved0

                let size_bytes = out.len() as u32;
                out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                    .copy_from_slice(&size_bytes.to_le_bytes());
                out
            };
            guest
                .write(alloc_table_gpa, &alloc_table_bytes)
                .expect("write alloc table");

            let stream = build_stream(|out| {
                emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                    push_u32(out, guest_tex); // texture_handle
                    push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                    push_u32(out, format_raw);
                    push_u32(out, 4); // width
                    push_u32(out, 4); // height
                    push_u32(out, 1); // mip_levels
                    push_u32(out, 1); // array_layers
                    push_u32(out, 8); // row_pitch_bytes
                    push_u32(out, ALLOC_TEX); // backing_alloc_id
                    push_u32(out, 0); // backing_offset_bytes
                    push_u64(out, 0); // reserved0
                });

                emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                    push_u32(out, guest_tex); // resource_handle
                    push_u32(out, 0); // reserved0
                    push_u64(out, 0); // offset_bytes
                    push_u64(out, packed.len() as u64); // size_bytes
                });

                emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                    push_u32(out, 1); // color_count
                    push_u32(out, 0); // depth_stencil
                    push_u32(out, 2); // colors[0]
                    for _ in 1..8 {
                        push_u32(out, 0);
                    }
                });

                emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                    push_u32(out, 1); // flags: CLEAR_COLOR
                    push_f32_bits(out, 0.0);
                    push_f32_bits(out, 0.0);
                    push_f32_bits(out, 0.0);
                    push_f32_bits(out, 1.0);
                    push_f32_bits(out, 1.0); // depth (unused)
                    push_u32(out, 0); // stencil
                });

                emit_packet(out, AerogpuCmdOpcode::SetTexture as u32, |out| {
                    push_u32(out, 1); // shader_stage: PIXEL
                    push_u32(out, 0); // slot
                    push_u32(out, guest_tex);
                    push_u32(out, 0); // reserved0
                });

                emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                    push_u32(out, 0); // start_slot
                    push_u32(out, 1); // buffer_count
                    push_u32(out, 1); // binding[0].buffer
                    push_u32(out, 8); // binding[0].stride_bytes
                    push_u32(out, 0); // binding[0].offset_bytes
                    push_u32(out, 0); // binding[0].reserved0
                });

                emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                    push_u32(out, 3); // vertex_count
                    push_u32(out, 1); // instance_count
                    push_u32(out, 0); // first_vertex
                    push_u32(out, 0); // first_instance
                });
            });

            guest.write(cmd_gpa, &stream).expect("write cmd stream");
            let report = exec.process_submission_from_guest_memory(
                &mut guest,
                cmd_gpa,
                stream.len() as u32,
                alloc_table_gpa,
                alloc_table_bytes.len() as u32,
            );
            assert!(
                report.is_ok(),
                "guest-backed {name} stream failed: {:#?}",
                report.events
            );

            let rt_tex = exec.texture(2).expect("render target");
            let rgba = readback_rgba8(
                exec.device(),
                exec.queue(),
                rt_tex,
                TextureRegion {
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    size: wgpu::Extent3d {
                        width: 4,
                        height: 4,
                        depth_or_array_layers: 1,
                    },
                },
            )
            .await;
            let idx_bytes = ((2 * 4 + 2) * 4) as usize;
            assert_eq!(
                &rgba[idx_bytes..idx_bytes + 4],
                &[255, 0, 0, 255],
                "guest-backed {name} sampled color mismatch"
            );
        }
    });
}

#[test]
fn copy_texture2d_writeback_packs_16bit_formats_b5g6r5_and_b5g5r5a1() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                common::skip_or_panic(module_path!(), "no wgpu adapter available");
                return;
            }
        };

        let mut exec = AeroGpuExecutor::new(device, queue).expect("create executor");
        let mut guest = VecGuestMemory::new(0x40_000);

        const ALLOC_DST: u32 = 1;
        let dst_gpa = 0x1000u64;
        let alloc_table_gpa = 0x8000u64;
        let alloc_table_bytes = {
            let mut out = Vec::new();

            push_u32(&mut out, AEROGPU_ALLOC_TABLE_MAGIC);
            push_u32(&mut out, AEROGPU_ABI_VERSION_U32);
            push_u32(&mut out, 0); // size_bytes (patch later)
            push_u32(&mut out, 1); // entry_count
            push_u32(&mut out, ProtocolAllocEntry::SIZE_BYTES as u32); // entry_stride_bytes
            push_u32(&mut out, 0); // reserved0

            push_u32(&mut out, ALLOC_DST);
            push_u32(&mut out, 0); // flags
            push_u64(&mut out, dst_gpa);
            push_u64(&mut out, 0x1000); // size_bytes
            push_u64(&mut out, 0); // reserved0

            let size_bytes = out.len() as u32;
            out[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            out
        };
        guest
            .write(alloc_table_gpa, &alloc_table_bytes)
            .expect("write alloc table");

        let formats: [(&str, u32, [u8; 2]); 2] = [
            (
                "B5G6R5Unorm",
                AerogpuFormat::B5G6R5Unorm as u32,
                0xF800u16.to_le_bytes(),
            ),
            (
                "B5G5R5A1Unorm",
                AerogpuFormat::B5G5R5A1Unorm as u32,
                0xFC00u16.to_le_bytes(),
            ),
        ];

        let cmd_gpa = 0x9000u64;
        for (idx, (name, format_raw, pixel)) in formats.into_iter().enumerate() {
            let src_tex = 10 + idx as u32;
            let dst_tex = 20 + idx as u32;

            let mut packed = Vec::with_capacity(4 * 4 * 2);
            for _ in 0..(4 * 4) {
                packed.extend_from_slice(&pixel);
            }
            assert_eq!(packed.len(), 32);

            // Clear the backing before each iteration so it's obvious the writeback ran.
            guest
                .write(dst_gpa, &vec![0u8; packed.len()])
                .expect("clear backing");

            let stream = build_stream(|out| {
                emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                    push_u32(out, src_tex); // texture_handle
                    push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                    push_u32(out, format_raw);
                    push_u32(out, 4); // width
                    push_u32(out, 4); // height
                    push_u32(out, 1); // mip_levels
                    push_u32(out, 1); // array_layers
                    push_u32(out, 8); // row_pitch_bytes
                    push_u32(out, 0); // backing_alloc_id
                    push_u32(out, 0); // backing_offset_bytes
                    push_u64(out, 0); // reserved0
                });

                emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                    push_u32(out, dst_tex); // texture_handle
                    push_u32(out, 1u32 << 3); // usage_flags: TEXTURE
                    push_u32(out, format_raw);
                    push_u32(out, 4); // width
                    push_u32(out, 4); // height
                    push_u32(out, 1); // mip_levels
                    push_u32(out, 1); // array_layers
                    push_u32(out, 8); // row_pitch_bytes
                    push_u32(out, ALLOC_DST); // backing_alloc_id
                    push_u32(out, 0); // backing_offset_bytes
                    push_u64(out, 0); // reserved0
                });

                emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                    push_u32(out, src_tex);
                    push_u32(out, 0); // reserved0
                    push_u64(out, 0); // offset_bytes
                    push_u64(out, packed.len() as u64); // size_bytes
                    out.extend_from_slice(&packed);
                });

                emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                    push_u32(out, dst_tex);
                    push_u32(out, src_tex);
                    push_u32(out, 0); // dst_mip_level
                    push_u32(out, 0); // dst_array_layer
                    push_u32(out, 0); // src_mip_level
                    push_u32(out, 0); // src_array_layer
                    push_u32(out, 0); // dst_x
                    push_u32(out, 0); // dst_y
                    push_u32(out, 0); // src_x
                    push_u32(out, 0); // src_y
                    push_u32(out, 4); // width
                    push_u32(out, 4); // height
                    push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
                    push_u32(out, 0); // reserved0
                });
            });

            guest.write(cmd_gpa, &stream).expect("write cmd stream");
            let report = exec.process_submission_from_guest_memory(
                &mut guest,
                cmd_gpa,
                stream.len() as u32,
                alloc_table_gpa,
                alloc_table_bytes.len() as u32,
            );
            assert!(
                report.is_ok(),
                "copy+writeback {name} stream failed: {:#?}",
                report.events
            );

            let mut out = vec![0u8; packed.len()];
            guest.read(dst_gpa, &mut out).expect("read back bytes");
            assert_eq!(out, packed, "COPY_TEXTURE2D writeback mismatch for {name}");
        }
    });
}
