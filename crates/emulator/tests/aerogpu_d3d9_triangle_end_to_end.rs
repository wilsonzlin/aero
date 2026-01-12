#![cfg(feature = "aerogpu-native")]

mod common;

use std::time::{Duration, Instant};

use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdOpcode, AerogpuIndexFormat, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use emulator::devices::aerogpu_regs::{irq_bits, mmio, ring_control};
use emulator::devices::aerogpu_ring::{
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC,
};
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::gpu_worker::aerogpu_backend::NativeAeroGpuBackend;
use emulator::gpu_worker::aerogpu_executor::{AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
use emulator::io::pci::MmioDevice;
use memory::Bus;
use memory::MemoryBus;

fn test_device_config() -> AeroGpuDeviceConfig {
    AeroGpuDeviceConfig {
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
        ..Default::default()
    }
}

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

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);
    let end_aligned = align4(out.len());
    out.resize(end_aligned, 0);
    let size_bytes = (end_aligned - start) as u32;
    out[start + 4..start + 8].copy_from_slice(&size_bytes.to_le_bytes());
}

fn build_stream(packets: impl FnOnce(&mut Vec<u8>), abi_version: u32) -> Vec<u8> {
    let mut out = Vec::new();

    // aerogpu_cmd_stream_header (24 bytes)
    push_u32(&mut out, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut out, abi_version);
    push_u32(&mut out, 0); // size_bytes (patch later)
    push_u32(&mut out, 0); // flags
    push_u32(&mut out, 0); // reserved0
    push_u32(&mut out, 0); // reserved1

    packets(&mut out);

    let size_bytes = out.len() as u32;
    out[8..12].copy_from_slice(&size_bytes.to_le_bytes());
    out
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

fn assemble_vs_passthrough_pos_color() -> Vec<u8> {
    // Matches `drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_fixedfunc_shaders.h`:
    // vs_2_0:
    //   mov oPos, v0
    //   mov oD0, v1
    //   end
    let mut words = vec![0xFFFE_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.extend(enc_inst(0x0001, &[enc_dst(5, 0, 0xF), enc_src(1, 1, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

fn assemble_ps_passthrough_color() -> Vec<u8> {
    // ps_2_0:
    //   mov oC0, v0
    //   end
    let mut words = vec![0xFFFF_0200];
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.push(0x0000_FFFF);
    to_bytes(&words)
}

#[test]
fn aerogpu_ring_submission_executes_d3d9_cmd_stream_and_presents_scanout() {
    let mut mem = Bus::new(0x40_000);

    let mut dev = AeroGpuPciDevice::new(test_device_config(), 0);
    let backend = match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => backend,
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::aerogpu_ring_submission_executes_d3d9_cmd_stream_and_presents_scanout"
                ),
                "wgpu request_adapter returned None",
            );
            return;
        }
        Err(err) => panic!("failed to initialize native AeroGPU backend: {err}"),
    };
    dev.set_backend(Box::new(backend));

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Command buffer: render two solid-color triangles (Draw + DrawIndexed) into RT0 and present.
    let cmd_gpa = 0x4000u64;
    let (width, height) = (64u32, 64u32);

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const IB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let verts = [
        // Top triangle (draw)
        (-0.8f32, 0.0f32, 0.0f32, 1.0f32),
        (0.0f32, 0.8f32, 0.0f32, 1.0f32),
        (0.8f32, 0.0f32, 0.0f32, 1.0f32),
        // Bottom triangle (draw_indexed, using base_vertex=3)
        (-0.8f32, 0.0f32, 0.0f32, 1.0f32),
        (0.8f32, 0.0f32, 0.0f32, 1.0f32),
        (0.0f32, -0.8f32, 0.0f32, 1.0f32),
    ];
    let mut vb_data = Vec::new();
    for (x, y, z, w) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, z);
        push_f32(&mut vb_data, w);
    }
    assert_eq!(vb_data.len(), 6 * 16);

    // Use an 8-byte upload to satisfy wgpu's 4-byte `queue.write_buffer` alignment requirements.
    let mut ib_data = Vec::new();
    for idx in [0u16, 1, 2] {
        push_u16(&mut ib_data, idx);
    }
    while ib_data.len() % 4 != 0 {
        ib_data.push(0);
    }
    assert_eq!(ib_data.len(), 8);

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

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, RT_HANDLE);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, VB_HANDLE);
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
                push_u64(out, vb_data.len() as u64);
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, VB_HANDLE);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_data.len() as u64);
                out.extend_from_slice(&vb_data);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, IB_HANDLE);
                push_u32(out, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER);
                push_u64(out, ib_data.len() as u64);
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, IB_HANDLE);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, ib_data.len() as u64);
                out.extend_from_slice(&ib_data);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, AerogpuShaderStage::Vertex as u32);
                push_u32(out, vs_bytes.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&vs_bytes);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
                push_u32(out, PS_HANDLE);
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, ps_bytes.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&ps_bytes);
            });

            emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, PS_HANDLE);
                push_u32(out, 0); // cs
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, vertex_decl.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&vertex_decl);
            });

            emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, VB_HANDLE);
                push_u32(out, 16); // stride_bytes
                push_u32(out, 0); // offset_bytes
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
                push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, RT_HANDLE);
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, width as f32);
                push_f32(out, height as f32);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
                push_i32(out, 0);
                push_i32(out, 0);
                push_i32(out, width as i32);
                push_i32(out, height as i32);
            });

            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 1.0); // depth
                push_u32(out, 0); // stencil
            });

            // c0 = green
            emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // start_register
                push_u32(out, 1); // vec4_count
                push_u32(out, 0); // reserved0
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });

            // c0 = red
            emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // start_register
                push_u32(out, 1); // vec4_count
                push_u32(out, 0); // reserved0
                push_f32(out, 1.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::SetIndexBuffer as u32, |out| {
                push_u32(out, IB_HANDLE);
                push_u32(out, AerogpuIndexFormat::Uint16 as u32);
                push_u32(out, 0); // offset_bytes
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::DrawIndexed as u32, |out| {
                push_u32(out, 3); // index_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_index
                push_i32(out, 3); // base_vertex
                push_u32(out, 0); // first_instance
            });

            emit_packet(out, AerogpuCmdOpcode::Present as u32, |out| {
                push_u32(out, 0); // scanout_id
                push_u32(out, 0); // flags
            });
        },
        dev.regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + 24, stream.len() as u32); // cmd_size_bytes
    mem.write_u64(desc_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 1); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(
        &mut mem,
        mmio::IRQ_ENABLE,
        4,
        irq_bits::FENCE | irq_bits::ERROR,
    );

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Drive polling until the fence completes.
    let start = Instant::now();
    let mut now = start;
    for _ in 0..200 {
        if dev.regs.completed_fence >= 1 {
            break;
        }
        now += Duration::from_millis(1);
        dev.tick(&mut mem, now);
    }

    assert_eq!(dev.regs.completed_fence, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);

    let (out_w, out_h, rgba) = dev
        .read_presented_scanout_rgba8(0)
        .expect("scanout should be readable");
    assert_eq!((out_w, out_h), (width, height));
    assert_eq!(rgba.len(), (width * height * 4) as usize);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    assert_eq!(px(32, 2), [0, 0, 0, 255], "top probe should be background");
    assert_eq!(
        px(32, 16),
        [0, 255, 0, 255],
        "upper probe should be inside the first triangle (Draw)"
    );
    assert_eq!(
        px(32, 48),
        [255, 0, 0, 255],
        "lower probe should be inside the second triangle (DrawIndexed)"
    );
    assert_eq!(
        px(32, 62),
        [0, 0, 0, 255],
        "bottom probe should be background"
    );
}

#[test]
fn aerogpu_ring_submission_executes_d3d9_cmd_stream_with_alloc_table_and_dirty_ranges() {
    let mut mem = Bus::new(0x80_000);

    let mut dev = AeroGpuPciDevice::new(test_device_config(), 0);
    let backend = match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => backend,
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::aerogpu_ring_submission_executes_d3d9_cmd_stream_with_alloc_table_and_dirty_ranges"
                ),
                "wgpu request_adapter returned None",
            );
            return;
        }
        Err(err) => panic!("failed to initialize native AeroGPU backend: {err}"),
    };
    dev.set_backend(Box::new(backend));

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    let cmd_gpa = 0x4000u64;
    let (width, height) = (64u32, 64u32);

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const IB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    let verts = [
        // Top triangle (draw)
        (-0.8f32, 0.0f32, 0.0f32, 1.0f32),
        (0.0f32, 0.8f32, 0.0f32, 1.0f32),
        (0.8f32, 0.0f32, 0.0f32, 1.0f32),
        // Bottom triangle (draw_indexed, using base_vertex=3)
        (-0.8f32, 0.0f32, 0.0f32, 1.0f32),
        (0.8f32, 0.0f32, 0.0f32, 1.0f32),
        (0.0f32, -0.8f32, 0.0f32, 1.0f32),
    ];
    let mut vb_data = Vec::new();
    for (x, y, z, w) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, z);
        push_f32(&mut vb_data, w);
    }
    assert_eq!(vb_data.len(), 6 * 16);

    // Use an 8-byte index buffer so dirty-range uploads satisfy COPY_BUFFER_ALIGNMENT (4 bytes).
    let mut ib_data = Vec::new();
    for idx in [0u16, 1, 2] {
        push_u16(&mut ib_data, idx);
    }
    while ib_data.len() % 4 != 0 {
        ib_data.push(0);
    }
    assert_eq!(ib_data.len(), 8);

    // Write backing allocations into guest memory.
    let vb_gpa = 0x8000u64;
    let ib_gpa = 0x9000u64;
    mem.write_physical(vb_gpa, &vb_data);
    mem.write_physical(ib_gpa, &ib_data);

    // Build alloc table with 2 entries.
    let alloc_table_gpa = 0x6000u64;
    let alloc_entry_count = 2u32;
    let alloc_entry_stride = 32u32;
    let alloc_table_size_bytes = 24u32 + alloc_entry_count * alloc_entry_stride;

    mem.write_u32(alloc_table_gpa, AEROGPU_ALLOC_TABLE_MAGIC);
    mem.write_u32(alloc_table_gpa + 4, dev.regs.abi_version);
    mem.write_u32(alloc_table_gpa + 8, alloc_table_size_bytes);
    mem.write_u32(alloc_table_gpa + 12, alloc_entry_count);
    mem.write_u32(alloc_table_gpa + 16, alloc_entry_stride);
    mem.write_u32(alloc_table_gpa + 20, 0); // reserved0

    // Entry 0: VB backing.
    let entry0 = alloc_table_gpa + 24;
    mem.write_u32(entry0, 1); // alloc_id
    mem.write_u32(entry0 + 4, 0); // flags
    mem.write_u64(entry0 + 8, vb_gpa);
    mem.write_u64(entry0 + 16, vb_data.len() as u64);
    mem.write_u64(entry0 + 24, 0); // reserved0

    // Entry 1: IB backing.
    let entry1 = alloc_table_gpa + 24 + 32;
    mem.write_u32(entry1, 2); // alloc_id
    mem.write_u32(entry1 + 4, 0); // flags
    mem.write_u64(entry1 + 8, ib_gpa);
    mem.write_u64(entry1 + 16, ib_data.len() as u64);
    mem.write_u64(entry1 + 24, 0); // reserved0

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

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, RT_HANDLE);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // Create VB/IB backed by guest memory (via alloc table) and mark them dirty.
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, VB_HANDLE);
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
                push_u64(out, vb_data.len() as u64);
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, IB_HANDLE);
                push_u32(out, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER);
                push_u64(out, ib_data.len() as u64);
                push_u32(out, 2); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, VB_HANDLE);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_data.len() as u64);
            });

            emit_packet(out, AerogpuCmdOpcode::ResourceDirtyRange as u32, |out| {
                push_u32(out, IB_HANDLE);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, ib_data.len() as u64);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, AerogpuShaderStage::Vertex as u32);
                push_u32(out, vs_bytes.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&vs_bytes);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
                push_u32(out, PS_HANDLE);
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, ps_bytes.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&ps_bytes);
            });

            emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, PS_HANDLE);
                push_u32(out, 0); // cs
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, vertex_decl.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&vertex_decl);
            });

            emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, VB_HANDLE);
                push_u32(out, 16); // stride_bytes
                push_u32(out, 0); // offset_bytes
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
                push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, RT_HANDLE);
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, width as f32);
                push_f32(out, height as f32);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
                push_i32(out, 0);
                push_i32(out, 0);
                push_i32(out, width as i32);
                push_i32(out, height as i32);
            });

            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 1.0); // depth
                push_u32(out, 0); // stencil
            });

            // c0 = green
            emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // start_register
                push_u32(out, 1); // vec4_count
                push_u32(out, 0); // reserved0
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });

            // c0 = red
            emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // start_register
                push_u32(out, 1); // vec4_count
                push_u32(out, 0); // reserved0
                push_f32(out, 1.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::SetIndexBuffer as u32, |out| {
                push_u32(out, IB_HANDLE);
                push_u32(out, AerogpuIndexFormat::Uint16 as u32);
                push_u32(out, 0); // offset_bytes
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::DrawIndexed as u32, |out| {
                push_u32(out, 3); // index_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_index
                push_i32(out, 3); // base_vertex
                push_u32(out, 0); // first_instance
            });

            emit_packet(out, AerogpuCmdOpcode::Present as u32, |out| {
                push_u32(out, 0); // scanout_id
                push_u32(out, 0); // flags
            });
        },
        dev.regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + 24, stream.len() as u32); // cmd_size_bytes
    mem.write_u64(desc_gpa + 32, alloc_table_gpa); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, alloc_table_size_bytes); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 1); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(
        &mut mem,
        mmio::IRQ_ENABLE,
        4,
        irq_bits::FENCE | irq_bits::ERROR,
    );

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Drive polling until the fence completes.
    let start = Instant::now();
    let mut now = start;
    for _ in 0..200 {
        if dev.regs.completed_fence >= 1 {
            break;
        }
        now += Duration::from_millis(1);
        dev.tick(&mut mem, now);
    }

    assert_eq!(dev.regs.completed_fence, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);

    let (out_w, out_h, rgba) = dev
        .read_presented_scanout_rgba8(0)
        .expect("scanout should be readable");
    assert_eq!((out_w, out_h), (width, height));
    assert_eq!(rgba.len(), (width * height * 4) as usize);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    assert_eq!(px(32, 2), [0, 0, 0, 255], "top probe should be background");
    assert_eq!(
        px(32, 16),
        [0, 255, 0, 255],
        "upper probe should be inside the first triangle (Draw)"
    );
    assert_eq!(
        px(32, 48),
        [255, 0, 0, 255],
        "lower probe should be inside the second triangle (DrawIndexed)"
    );
    assert_eq!(
        px(32, 62),
        [0, 0, 0, 255],
        "bottom probe should be background"
    );
}

#[test]
fn aerogpu_ring_submission_executes_win7_fixedfunc_triangle_stream() {
    let mut mem = Bus::new(0x40_000);

    let mut dev = AeroGpuPciDevice::new(test_device_config(), 0);
    let backend = match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => backend,
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::aerogpu_ring_submission_executes_win7_fixedfunc_triangle_stream"
                ),
                "wgpu request_adapter returned None",
            );
            return;
        }
        Err(err) => panic!("failed to initialize native AeroGPU backend: {err}"),
    };
    dev.set_backend(Box::new(backend));

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    let cmd_gpa = 0x4000u64;
    let (width, height) = (64u32, 64u32);

    const RT_HANDLE: u32 = 1;
    const VB_HANDLE: u32 = 2;
    const VS_HANDLE: u32 = 3;
    const PS_HANDLE: u32 = 4;
    const IL_HANDLE: u32 = 5;

    // Vertex format mirrors the Win7 bring-up test: position + D3DCOLOR.
    // Use a non-symmetric D3DCOLOR (red) so we catch BGRA->RGBA conversion regressions.
    let verts = [
        (-0.5f32, 0.5f32, 0.0f32, 1.0f32, 0xFFFF0000u32),
        (0.5f32, 0.5f32, 0.0f32, 1.0f32, 0xFFFF0000u32),
        (0.0f32, -0.5f32, 0.0f32, 1.0f32, 0xFFFF0000u32),
    ];

    let mut vb_data = Vec::new();
    for (x, y, z, w, color) in verts {
        push_f32(&mut vb_data, x);
        push_f32(&mut vb_data, y);
        push_f32(&mut vb_data, z);
        push_f32(&mut vb_data, w);
        push_u32(&mut vb_data, color);
    }
    assert_eq!(vb_data.len(), 3 * 20);

    let vs_bytes = assemble_vs_passthrough_pos_color();
    let ps_bytes = assemble_ps_passthrough_color();

    // D3DVERTEXELEMENT9 stream (little-endian).
    // Element 0: POSITION0 float4 at stream 0 offset 0.
    // Element 1: COLOR0 d3dcolor at stream 0 offset 16.
    // End marker: stream 0xFF, type UNUSED.
    let mut vertex_decl = Vec::new();
    push_u16(&mut vertex_decl, 0); // stream
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 3); // type = FLOAT4
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage = POSITION
    push_u8(&mut vertex_decl, 0); // usage_index
    push_u16(&mut vertex_decl, 0); // stream
    push_u16(&mut vertex_decl, 16); // offset
    push_u8(&mut vertex_decl, 4); // type = D3DCOLOR
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 10); // usage = COLOR
    push_u8(&mut vertex_decl, 0); // usage_index
    push_u16(&mut vertex_decl, 0x00FF); // stream = 0xFF
    push_u16(&mut vertex_decl, 0); // offset
    push_u8(&mut vertex_decl, 17); // type = UNUSED
    push_u8(&mut vertex_decl, 0); // method
    push_u8(&mut vertex_decl, 0); // usage
    push_u8(&mut vertex_decl, 0); // usage_index
    assert_eq!(vertex_decl.len(), 24);

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, RT_HANDLE);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::B8G8R8X8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, VB_HANDLE);
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
                push_u64(out, vb_data.len() as u64);
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, VB_HANDLE);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_data.len() as u64);
                out.extend_from_slice(&vb_data);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, AerogpuShaderStage::Vertex as u32);
                push_u32(out, vs_bytes.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&vs_bytes);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
                push_u32(out, PS_HANDLE);
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, ps_bytes.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&ps_bytes);
            });

            emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, PS_HANDLE);
                push_u32(out, 0); // cs
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, vertex_decl.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&vertex_decl);
            });

            emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, VB_HANDLE);
                push_u32(out, 20); // stride_bytes
                push_u32(out, 0); // offset_bytes
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
                push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, RT_HANDLE);
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, width as f32);
                push_f32(out, height as f32);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
                push_i32(out, 0);
                push_i32(out, 0);
                push_i32(out, width as i32);
                push_i32(out, height as i32);
            });

            // Clear black; triangle should leave top-left corner untouched.
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 1.0); // depth
                push_u32(out, 0); // stencil
            });

            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });

            emit_packet(out, AerogpuCmdOpcode::Present as u32, |out| {
                push_u32(out, 0); // scanout_id
                push_u32(out, 0); // flags
            });
        },
        dev.regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + 24, stream.len() as u32); // cmd_size_bytes
    mem.write_u64(desc_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 1); // signal_fence

    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(
        &mut mem,
        mmio::IRQ_ENABLE,
        4,
        irq_bits::FENCE | irq_bits::ERROR,
    );

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Drive polling until the fence completes.
    let start = Instant::now();
    let mut now = start;
    for _ in 0..200 {
        if dev.regs.completed_fence >= 1 {
            break;
        }
        now += Duration::from_millis(1);
        dev.tick(&mut mem, now);
    }

    assert_eq!(dev.regs.completed_fence, 1);

    let (out_w, out_h, rgba) = dev
        .read_presented_scanout_rgba8(0)
        .expect("scanout should be readable");
    assert_eq!((out_w, out_h), (width, height));
    assert_eq!(rgba.len(), (width * height * 4) as usize);

    let px = |x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    // Geometry based on Win7 `d3d9ex_triangle`: center should be vertex color, corner should be
    // clear color. We use a non-symmetric vertex color (red) to catch D3DCOLOR channel-ordering
    // regressions.
    assert_eq!(px(width / 2, height / 2), [255, 0, 0, 255]);
    assert_eq!(px(5, 5), [0, 0, 0, 255]);
}

#[test]
fn aerogpu_ring_submission_isolates_pixel_constants_per_context() {
    let mut mem = Bus::new(0x60_000);

    let mut dev = AeroGpuPciDevice::new(test_device_config(), 0);
    let backend = match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => backend,
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::aerogpu_ring_submission_isolates_pixel_constants_per_context"
                ),
                "wgpu request_adapter returned None",
            );
            return;
        }
        Err(err) => panic!("failed to initialize native AeroGPU backend: {err}"),
    };
    dev.set_backend(Box::new(backend));

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 2); // tail (2 submissions)

    // Two independent render targets, one per context.
    let cmd0_gpa = 0x4000u64;
    let cmd1_gpa = 0x6000u64;
    let (width, height) = (64u32, 64u32);

    const RT0_HANDLE: u32 = 1;
    const RT1_HANDLE: u32 = 2;
    const VB_HANDLE: u32 = 3;
    const VS_HANDLE: u32 = 4;
    const PS_HANDLE: u32 = 5;
    const IL_HANDLE: u32 = 6;

    // Single triangle that covers the center pixel while leaving the top-left corner untouched.
    let verts = [
        (-0.5f32, -0.5f32, 0.0f32, 1.0f32),
        (0.0f32, 0.5f32, 0.0f32, 1.0f32),
        (0.5f32, -0.5f32, 0.0f32, 1.0f32),
    ];
    let mut vb_data = Vec::new();
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

    // Context 1: create resources, clear red, set pixel constant c0=green, draw, present scanout0.
    let stream0 = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, RT0_HANDLE);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, RT1_HANDLE);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, VB_HANDLE);
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
                push_u64(out, vb_data.len() as u64);
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::UploadResource as u32, |out| {
                push_u32(out, VB_HANDLE);
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, vb_data.len() as u64);
                out.extend_from_slice(&vb_data);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, AerogpuShaderStage::Vertex as u32);
                push_u32(out, vs_bytes.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&vs_bytes);
            });

            emit_packet(out, AerogpuCmdOpcode::CreateShaderDxbc as u32, |out| {
                push_u32(out, PS_HANDLE);
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, ps_bytes.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&ps_bytes);
            });

            emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, PS_HANDLE);
                push_u32(out, 0); // cs
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, vertex_decl.len() as u32);
                push_u32(out, 0); // reserved0
                out.extend_from_slice(&vertex_decl);
            });

            emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, VB_HANDLE);
                push_u32(out, 16); // stride_bytes
                push_u32(out, 0); // offset_bytes
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
                push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, RT0_HANDLE);
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, width as f32);
                push_f32(out, height as f32);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
                push_i32(out, 0);
                push_i32(out, 0);
                push_i32(out, width as i32);
                push_i32(out, height as i32);
            });

            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_f32(out, 1.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 1.0); // depth
                push_u32(out, 0); // stencil
            });

            // c0 = green
            emit_packet(out, AerogpuCmdOpcode::SetShaderConstantsF as u32, |out| {
                push_u32(out, AerogpuShaderStage::Pixel as u32);
                push_u32(out, 0); // start_register
                push_u32(out, 1); // vec4_count
                push_u32(out, 0); // reserved0
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });

            emit_packet(out, AerogpuCmdOpcode::Present as u32, |out| {
                push_u32(out, 0); // scanout_id
                push_u32(out, 0); // flags
            });
        },
        dev.regs.abi_version,
    );

    // Context 2: clear red, *do not* set shader constants (defaults to zero), draw, present scanout1.
    let stream1 = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::BindShaders as u32, |out| {
                push_u32(out, VS_HANDLE);
                push_u32(out, PS_HANDLE);
                push_u32(out, 0); // cs
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetInputLayout as u32, |out| {
                push_u32(out, IL_HANDLE);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |out| {
                push_u32(out, 0); // start_slot
                push_u32(out, 1); // buffer_count
                push_u32(out, VB_HANDLE);
                push_u32(out, 16); // stride_bytes
                push_u32(out, 0); // offset_bytes
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |out| {
                push_u32(out, AerogpuPrimitiveTopology::TriangleList as u32);
                push_u32(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, RT1_HANDLE);
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |out| {
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, width as f32);
                push_f32(out, height as f32);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
            });

            emit_packet(out, AerogpuCmdOpcode::SetScissor as u32, |out| {
                push_i32(out, 0);
                push_i32(out, 0);
                push_i32(out, width as i32);
                push_i32(out, height as i32);
            });

            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_f32(out, 1.0);
                push_f32(out, 0.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 1.0); // depth
                push_u32(out, 0); // stencil
            });

            emit_packet(out, AerogpuCmdOpcode::Draw as u32, |out| {
                push_u32(out, 3); // vertex_count
                push_u32(out, 1); // instance_count
                push_u32(out, 0); // first_vertex
                push_u32(out, 0); // first_instance
            });

            emit_packet(out, AerogpuCmdOpcode::Present as u32, |out| {
                push_u32(out, 1); // scanout_id
                push_u32(out, 0); // flags
            });
        },
        dev.regs.abi_version,
    );

    mem.write_physical(cmd0_gpa, &stream0);
    mem.write_physical(cmd1_gpa, &stream1);

    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc0_gpa, 64); // desc_size_bytes
    mem.write_u32(desc0_gpa + 4, 0); // flags
    mem.write_u32(desc0_gpa + 8, 1); // context_id
    mem.write_u32(desc0_gpa + 12, 0); // engine_id
    mem.write_u64(desc0_gpa + 16, cmd0_gpa); // cmd_gpa
    mem.write_u32(desc0_gpa + 24, stream0.len() as u32); // cmd_size_bytes
    mem.write_u64(desc0_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc0_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc0_gpa + 48, 1); // signal_fence

    let desc1_gpa = desc0_gpa + entry_stride as u64;
    mem.write_u32(desc1_gpa, 64); // desc_size_bytes
    mem.write_u32(desc1_gpa + 4, 0); // flags
    mem.write_u32(desc1_gpa + 8, 2); // context_id
    mem.write_u32(desc1_gpa + 12, 0); // engine_id
    mem.write_u64(desc1_gpa + 16, cmd1_gpa); // cmd_gpa
    mem.write_u32(desc1_gpa + 24, stream1.len() as u32); // cmd_size_bytes
    mem.write_u64(desc1_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc1_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc1_gpa + 48, 2); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(
        &mut mem,
        mmio::IRQ_ENABLE,
        4,
        irq_bits::FENCE | irq_bits::ERROR,
    );

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 2);

    // Drive polling until both fences complete.
    let start = Instant::now();
    let mut now = start;
    for _ in 0..400 {
        if dev.regs.completed_fence >= 2 {
            break;
        }
        now += Duration::from_millis(1);
        dev.tick(&mut mem, now);
    }

    assert_eq!(dev.regs.completed_fence, 2);

    let (_, _, rgba0) = dev
        .read_presented_scanout_rgba8(0)
        .expect("scanout0 should be readable");
    let (_, _, rgba1) = dev
        .read_presented_scanout_rgba8(1)
        .expect("scanout1 should be readable");

    let px = |rgba: &[u8], x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * width + x) * 4) as usize;
        rgba[idx..idx + 4].try_into().unwrap()
    };

    // Context 1 wrote green triangle.
    assert_eq!(px(&rgba0, width / 2, height / 2), [0, 255, 0, 255]);
    assert_eq!(px(&rgba0, 5, 5), [255, 0, 0, 255]);

    // Context 2 did not set pixel constants, so c0 defaults to 0 and the triangle is black/transparent.
    assert_eq!(px(&rgba1, width / 2, height / 2), [0, 0, 0, 0]);
    assert_eq!(px(&rgba1, 5, 5), [255, 0, 0, 255]);
}

#[test]
fn aerogpu_ring_submission_copy_texture2d_writeback_writes_guest_memory() {
    let mut mem = Bus::new(0x40_000);

    let mut dev = AeroGpuPciDevice::new(test_device_config(), 0);
    let backend = match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => backend,
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::aerogpu_ring_submission_copy_texture2d_writeback_writes_guest_memory"
                ),
                "wgpu request_adapter returned None",
            );
            return;
        }
        Err(err) => panic!("failed to initialize native AeroGPU backend: {err}"),
    };
    dev.set_backend(Box::new(backend));

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Allocation table: one entry backing a 1x1 RGBA8 texture.
    let alloc_table_gpa = 0x6000u64;
    let alloc_entry_count = 1u32;
    let alloc_entry_stride = 32u32;
    let alloc_table_size_bytes = 24u32 + alloc_entry_count * alloc_entry_stride;

    let dst_gpa = 0x8000u64;
    mem.write_physical(dst_gpa, &[0xAA, 0xAA, 0xAA, 0xAA]);

    mem.write_u32(alloc_table_gpa, AEROGPU_ALLOC_TABLE_MAGIC);
    mem.write_u32(alloc_table_gpa + 4, dev.regs.abi_version);
    mem.write_u32(alloc_table_gpa + 8, alloc_table_size_bytes);
    mem.write_u32(alloc_table_gpa + 12, alloc_entry_count);
    mem.write_u32(alloc_table_gpa + 16, alloc_entry_stride);
    mem.write_u32(alloc_table_gpa + 20, 0); // reserved0

    let entry0 = alloc_table_gpa + 24;
    mem.write_u32(entry0, 1); // alloc_id
    mem.write_u32(entry0 + 4, 0); // flags
    mem.write_u64(entry0 + 8, dst_gpa);
    mem.write_u64(entry0 + 16, 4096); // size_bytes
    mem.write_u64(entry0 + 24, 0); // reserved0

    let cmd_gpa = 0x4000u64;
    let (width, height) = (1u32, 1u32);

    const SRC_RT_HANDLE: u32 = 1;
    const DST_TEX_HANDLE: u32 = 2;

    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, SRC_RT_HANDLE);
                push_u32(
                    out,
                    AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
                );
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
                push_u32(out, DST_TEX_HANDLE);
                push_u32(out, AEROGPU_RESOURCE_USAGE_TEXTURE);
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32);
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, width * 4); // row_pitch_bytes
                push_u32(out, 1); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, SRC_RT_HANDLE);
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            // Clear the render target to green.
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 0.0);
                push_f32(out, 1.0);
                push_f32(out, 1.0); // depth
                push_u32(out, 0); // stencil
            });

            emit_packet(out, AerogpuCmdOpcode::CopyTexture2d as u32, |out| {
                push_u32(out, DST_TEX_HANDLE);
                push_u32(out, SRC_RT_HANDLE);
                push_u32(out, 0); // dst_mip_level
                push_u32(out, 0); // dst_array_layer
                push_u32(out, 0); // src_mip_level
                push_u32(out, 0); // src_array_layer
                push_u32(out, 0); // dst_x
                push_u32(out, 0); // dst_y
                push_u32(out, 0); // src_x
                push_u32(out, 0); // src_y
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
                push_u32(out, 0); // reserved0
            });
        },
        dev.regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + 24, stream.len() as u32); // cmd_size_bytes
    mem.write_u64(desc_gpa + 32, alloc_table_gpa); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, alloc_table_size_bytes); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 1); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(
        &mut mem,
        mmio::IRQ_ENABLE,
        4,
        irq_bits::FENCE | irq_bits::ERROR,
    );

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Drive polling until the fence completes.
    let start = Instant::now();
    let mut now = start;
    for _ in 0..200 {
        if dev.regs.completed_fence >= 1 {
            break;
        }
        now += Duration::from_millis(1);
        dev.tick(&mut mem, now);
    }

    assert_eq!(dev.regs.completed_fence, 1);

    let mut out = [0u8; 4];
    mem.read_physical(dst_gpa, &mut out);
    assert_eq!(out, [0, 255, 0, 255]);
}

#[test]
fn aerogpu_ring_submission_completes_fence_on_d3d9_executor_error() {
    let mut mem = Bus::new(0x40_000);

    let mut dev = AeroGpuPciDevice::new(test_device_config(), 0);
    let backend = match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => backend,
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::aerogpu_ring_submission_completes_fence_on_d3d9_executor_error"
                ),
                "wgpu request_adapter returned None",
            );
            return;
        }
        Err(err) => panic!("failed to initialize native AeroGPU backend: {err}"),
    };
    dev.set_backend(Box::new(backend));

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Command buffer: CREATE_BUFFER with an invalid size (not aligned to COPY_BUFFER_ALIGNMENT).
    let cmd_gpa = 0x4000u64;
    let stream = build_stream(
        |out| {
            emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
                push_u64(out, 1); // size_bytes (invalid; must be 4-byte aligned)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });
        },
        dev.regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + 24, stream.len() as u32); // cmd_size_bytes
    mem.write_u64(desc_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 1); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(
        &mut mem,
        mmio::IRQ_ENABLE,
        4,
        irq_bits::FENCE | irq_bits::ERROR,
    );

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Drive polling until the fence completes. Even though the command stream is invalid, we must
    // still make forward progress to avoid deadlocking the guest.
    let start = Instant::now();
    let mut now = start;
    for _ in 0..200 {
        if dev.regs.completed_fence >= 1 {
            break;
        }
        now += Duration::from_millis(1);
        dev.tick(&mut mem, now);
    }

    assert_eq!(dev.regs.completed_fence, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert_ne!(
        dev.regs.irq_status & irq_bits::ERROR,
        0,
        "executor error should raise ERROR IRQ"
    );
    assert_eq!(dev.regs.stats.gpu_exec_errors, 1);
}
