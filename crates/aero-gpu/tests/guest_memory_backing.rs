use aero_gpu::aerogpu_executor::AeroGpuExecutor;
use aero_gpu::{readback_rgba8, TextureRegion, VecGuestMemory};
use aero_protocol::aerogpu::{
    aerogpu_cmd::{
        AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
        AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    },
    aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32},
    aerogpu_ring::{
        AerogpuAllocEntry as ProtocolAllocEntry, AerogpuAllocTableHeader as ProtocolAllocTableHeader,
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
    while (out.len() - start) % 4 != 0 {
        out.push(0);
    }

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert_eq!(size_bytes % 4, 0);
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

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
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
                eprintln!("skipping guest backing test: no wgpu adapter available");
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
            &guest,
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
fn resource_dirty_range_uploads_guest_backed_index_buffer_before_draw_indexed() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                eprintln!("skipping guest backing indexed test: no wgpu adapter available");
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
            &guest,
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
                eprintln!("skipping upload_resource test: no wgpu adapter available");
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

        let report =
            exec.process_submission_from_guest_memory(&guest, cmd_gpa, stream.len() as u32, 0, 0);
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
fn resource_dirty_range_texture_row_pitch_is_respected() {
    pollster::block_on(async {
        let (device, queue) = match create_device_queue().await {
            Some(v) => v,
            None => {
                eprintln!("skipping row_pitch test: no wgpu adapter available");
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
            &guest,
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
                eprintln!("skipping bgra rt test: no wgpu adapter available");
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

        let report =
            exec.process_submission_from_guest_memory(&guest, cmd_gpa, stream.len() as u32, 0, 0);
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
