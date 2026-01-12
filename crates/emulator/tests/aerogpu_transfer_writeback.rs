#![cfg(feature = "aerogpu-native")]

use std::time::{Duration, Instant};

use aero_protocol::aerogpu::aerogpu_cmd::{
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_WRITEBACK_DST,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring::AEROGPU_ALLOC_TABLE_MAGIC;
use emulator::devices::aerogpu_regs::{irq_bits, mmio, ring_control, FEATURE_TRANSFER};
use emulator::devices::aerogpu_ring::{AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC};
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::gpu_worker::aerogpu_backend::NativeAeroGpuBackend;
use emulator::gpu_worker::aerogpu_executor::{AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
use emulator::io::pci::{MmioDevice, PciDevice};
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

fn new_test_device() -> AeroGpuPciDevice {
    let mut dev = AeroGpuPciDevice::new(test_device_config(), 0);
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn emit_packet(out: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = out.len();
    push_u32(out, opcode);
    push_u32(out, 0); // size_bytes placeholder
    payload(out);

    let size_bytes = (out.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert_eq!(size_bytes % 4, 0);
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

fn drive_until_fence(mem: &mut Bus, dev: &mut AeroGpuPciDevice, fence: u64) {
    let start = Instant::now();
    let mut now = start;
    for _ in 0..200 {
        if dev.regs.completed_fence >= fence {
            break;
        }
        now += Duration::from_millis(1);
        dev.tick(mem, now);
    }
    assert_eq!(dev.regs.completed_fence, fence);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
}

#[test]
fn aerogpu_copy_texture2d_writeback_updates_guest_memory() {
    let mut mem = Bus::new(0x20_000);

    let mut dev = new_test_device();
    dev.set_backend(Box::new(
        NativeAeroGpuBackend::new_headless().expect("native backend should initialize"),
    ));
    assert_ne!(dev.regs.features & FEATURE_TRANSFER, 0);

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

    // Destination allocation (guest-visible memory) + alloc table.
    let alloc_id = 1u32;
    let (width, height) = (4u32, 4u32);
    let row_pitch = width * 4;
    let dst_gpa = 0x8000u64;
    let dst_size = (row_pitch * height) as u64;

    let alloc_table_gpa = 0x6000u64;
    let alloc_table = {
        let mut bytes = Vec::new();
        // aerogpu_alloc_table_header (24 bytes)
        push_u32(&mut bytes, AEROGPU_ALLOC_TABLE_MAGIC);
        push_u32(&mut bytes, dev.regs.abi_version);
        push_u32(&mut bytes, 24 + 32); // size_bytes
        push_u32(&mut bytes, 1); // entry_count
        push_u32(&mut bytes, 32); // entry_stride_bytes
        push_u32(&mut bytes, 0); // reserved0

        // aerogpu_alloc_entry (32 bytes)
        push_u32(&mut bytes, alloc_id);
        push_u32(&mut bytes, 0); // flags
        push_u64(&mut bytes, dst_gpa);
        push_u64(&mut bytes, dst_size);
        push_u64(&mut bytes, 0); // reserved0
        bytes
    };
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Command buffer:
    // - create src texture (host allocated)
    // - clear it to solid green
    // - create dst texture backed by alloc_id
    // - COPY_TEXTURE2D with WRITEBACK_DST
    let cmd_gpa = 0x4000u64;
    let stream = build_stream(
        |out| {
            // CREATE_TEXTURE2D src (56 bytes)
            emit_packet(out, 0x101, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // SET_RENDER_TARGETS (48 bytes)
            emit_packet(out, 0x400, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 1); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            // CLEAR green (36 bytes)
            emit_packet(out, 0x600, |out| {
                push_u32(out, 1); // AEROGPU_CLEAR_COLOR
                push_u32(out, 0.0f32.to_bits()); // r
                push_u32(out, 1.0f32.to_bits()); // g
                push_u32(out, 0.0f32.to_bits()); // b
                push_u32(out, 1.0f32.to_bits()); // a
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });

            // CREATE_TEXTURE2D dst (56 bytes)
            emit_packet(out, 0x101, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, row_pitch); // row_pitch_bytes (required for guest-backed)
                push_u32(out, alloc_id); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_TEXTURE2D (64 bytes)
            emit_packet(out, 0x106, |out| {
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
    mem.write_u32(desc_gpa + 40, alloc_table.len() as u32); // alloc_table_size_bytes
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
    drive_until_fence(&mut mem, &mut dev, 1);

    // Validate guest memory contains RGBA8 green pixels after COPY_TEXTURE2D writeback.
    let mut got = vec![0u8; dst_size as usize];
    mem.read_physical(dst_gpa, &mut got);

    for px in got.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}

#[test]
fn aerogpu_copy_buffer_writeback_updates_guest_memory() {
    let mut mem = Bus::new(0x20_000);

    let mut dev = new_test_device();
    dev.set_backend(Box::new(
        NativeAeroGpuBackend::new_headless().expect("native backend should initialize"),
    ));
    assert_ne!(dev.regs.features & FEATURE_TRANSFER, 0);

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

    // Destination allocation (guest-visible memory) + alloc table.
    let alloc_id = 1u32;
    let dst_gpa = 0x9000u64;
    let pattern: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];

    let alloc_table_gpa = 0x6000u64;
    let alloc_table = {
        let mut bytes = Vec::new();
        // aerogpu_alloc_table_header (24 bytes)
        push_u32(&mut bytes, AEROGPU_ALLOC_TABLE_MAGIC);
        push_u32(&mut bytes, dev.regs.abi_version);
        push_u32(&mut bytes, 24 + 32); // size_bytes
        push_u32(&mut bytes, 1); // entry_count
        push_u32(&mut bytes, 32); // entry_stride_bytes
        push_u32(&mut bytes, 0); // reserved0

        // aerogpu_alloc_entry (32 bytes)
        push_u32(&mut bytes, alloc_id);
        push_u32(&mut bytes, 0); // flags
        push_u64(&mut bytes, dst_gpa);
        push_u64(&mut bytes, pattern.len() as u64);
        push_u64(&mut bytes, 0); // reserved0
        bytes
    };
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Command buffer:
    // - create src buffer (host allocated)
    // - upload a known byte pattern
    // - create dst buffer backed by alloc_id
    // - COPY_BUFFER with WRITEBACK_DST
    let cmd_gpa = 0x4000u64;
    let stream = build_stream(
        |out| {
            // CREATE_BUFFER src (40 bytes)
            emit_packet(out, 0x100, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, pattern.len() as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE (32 + payload)
            emit_packet(out, 0x104, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, pattern.len() as u64); // size_bytes
                out.extend_from_slice(&pattern);
            });

            // CREATE_BUFFER dst (40 bytes)
            emit_packet(out, 0x100, |out| {
                push_u32(out, 2); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, pattern.len() as u64); // size_bytes
                push_u32(out, alloc_id); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_BUFFER (48 bytes)
            emit_packet(out, 0x105, |out| {
                push_u32(out, 2); // dst_buffer
                push_u32(out, 1); // src_buffer
                push_u64(out, 0); // dst_offset_bytes
                push_u64(out, 0); // src_offset_bytes
                push_u64(out, pattern.len() as u64); // size_bytes
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
    mem.write_u32(desc_gpa + 40, alloc_table.len() as u32); // alloc_table_size_bytes
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

    drive_until_fence(&mut mem, &mut dev, 1);

    let mut got = vec![0u8; pattern.len()];
    mem.read_physical(dst_gpa, &mut got);
    assert_eq!(got, pattern);
}

#[test]
fn aerogpu_copy_buffer_writeback_respects_offsets() {
    let mut mem = Bus::new(0x20_000);

    let mut dev = new_test_device();
    dev.set_backend(Box::new(
        NativeAeroGpuBackend::new_headless().expect("native backend should initialize"),
    ));
    assert_ne!(dev.regs.features & FEATURE_TRANSFER, 0);

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

    let dst_size = 256usize;
    let dst_gpa = 0x9000u64;
    let dst_init = vec![0xEEu8; dst_size];
    mem.write_physical(dst_gpa, &dst_init);

    // Destination allocation (guest-visible memory) + alloc table.
    let alloc_id = 1u32;
    let alloc_table_gpa = 0x6000u64;
    let alloc_table = {
        let mut bytes = Vec::new();
        // aerogpu_alloc_table_header (24 bytes)
        push_u32(&mut bytes, AEROGPU_ALLOC_TABLE_MAGIC);
        push_u32(&mut bytes, dev.regs.abi_version);
        push_u32(&mut bytes, 24 + 32); // size_bytes
        push_u32(&mut bytes, 1); // entry_count
        push_u32(&mut bytes, 32); // entry_stride_bytes
        push_u32(&mut bytes, 0); // reserved0

        // aerogpu_alloc_entry (32 bytes)
        push_u32(&mut bytes, alloc_id);
        push_u32(&mut bytes, 0); // flags
        push_u64(&mut bytes, dst_gpa);
        push_u64(&mut bytes, dst_size as u64);
        push_u64(&mut bytes, 0); // reserved0
        bytes
    };
    mem.write_physical(alloc_table_gpa, &alloc_table);

    let src_pattern: Vec<u8> = (0u8..=255u8).collect();
    assert_eq!(src_pattern.len(), dst_size);

    let src_offset = 16u64;
    let dst_offset = 32u64;
    let copy_size = 64u64;

    // Command buffer:
    // - create src buffer (host allocated)
    // - upload a known byte pattern
    // - create dst buffer backed by alloc_id
    // - COPY_BUFFER with WRITEBACK_DST (with offsets)
    let cmd_gpa = 0x4000u64;
    let stream = build_stream(
        |out| {
            // CREATE_BUFFER src (40 bytes)
            emit_packet(out, 0x100, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, dst_size as u64); // size_bytes
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE (32 + payload)
            emit_packet(out, 0x104, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, src_pattern.len() as u64); // size_bytes
                out.extend_from_slice(&src_pattern);
            });

            // CREATE_BUFFER dst (40 bytes)
            emit_packet(out, 0x100, |out| {
                push_u32(out, 2); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, dst_size as u64); // size_bytes
                push_u32(out, alloc_id); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_BUFFER (48 bytes)
            emit_packet(out, 0x105, |out| {
                push_u32(out, 2); // dst_buffer
                push_u32(out, 1); // src_buffer
                push_u64(out, dst_offset); // dst_offset_bytes
                push_u64(out, src_offset); // src_offset_bytes
                push_u64(out, copy_size); // size_bytes
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
    mem.write_u32(desc_gpa + 40, alloc_table.len() as u32); // alloc_table_size_bytes
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

    drive_until_fence(&mut mem, &mut dev, 1);

    let mut got = vec![0u8; dst_size];
    mem.read_physical(dst_gpa, &mut got);

    // Ensure the copy landed at the correct offsets and did not clobber untouched bytes.
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
}

#[test]
fn aerogpu_copy_texture2d_writeback_subrect_updates_guest_memory() {
    let mut mem = Bus::new(0x20_000);

    let mut dev = new_test_device();
    dev.set_backend(Box::new(
        NativeAeroGpuBackend::new_headless().expect("native backend should initialize"),
    ));
    assert_ne!(dev.regs.features & FEATURE_TRANSFER, 0);

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

    let (width, height) = (7u32, 4u32);
    let row_pitch = 32u32; // larger than width*4; not wgpu-aligned

    let dst_gpa = 0x8000u64;
    let dst_size = (row_pitch * height) as usize;
    let dst_init = vec![0x11u8; dst_size];
    mem.write_physical(dst_gpa, &dst_init);

    // Destination allocation (guest-visible memory) + alloc table.
    let alloc_id = 1u32;
    let alloc_table_gpa = 0x6000u64;
    let alloc_table = {
        let mut bytes = Vec::new();
        // aerogpu_alloc_table_header (24 bytes)
        push_u32(&mut bytes, AEROGPU_ALLOC_TABLE_MAGIC);
        push_u32(&mut bytes, dev.regs.abi_version);
        push_u32(&mut bytes, 24 + 32); // size_bytes
        push_u32(&mut bytes, 1); // entry_count
        push_u32(&mut bytes, 32); // entry_stride_bytes
        push_u32(&mut bytes, 0); // reserved0

        // aerogpu_alloc_entry (32 bytes)
        push_u32(&mut bytes, alloc_id);
        push_u32(&mut bytes, 0); // flags
        push_u64(&mut bytes, dst_gpa);
        push_u64(&mut bytes, dst_size as u64);
        push_u64(&mut bytes, 0); // reserved0
        bytes
    };
    mem.write_physical(alloc_table_gpa, &alloc_table);

    let src_size = (width * height * 4) as usize;
    let mut src_bytes = vec![0u8; src_size];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let idx = (y * width as usize + x) * 4;
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

    let cmd_gpa = 0x4000u64;
    let stream = build_stream(
        |out| {
            // CREATE_TEXTURE2D src (56 bytes)
            emit_packet(out, 0x101, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, 0); // row_pitch_bytes (host allocated)
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE into src texture (32 + payload).
            emit_packet(out, 0x104, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, src_bytes.len() as u64); // size_bytes
                out.extend_from_slice(&src_bytes);
            });

            // CREATE_TEXTURE2D dst (56 bytes, guest-backed)
            emit_packet(out, 0x101, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 0); // usage_flags
                push_u32(out, AerogpuFormat::R8G8B8A8Unorm as u32); // format
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, 1); // mip_levels
                push_u32(out, 1); // array_layers
                push_u32(out, row_pitch); // row_pitch_bytes
                push_u32(out, alloc_id); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // Mark the guest-backed dst as dirty so the executor uploads initial bytes before the copy.
            emit_packet(out, 0x103, |out| {
                push_u32(out, 2); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, dst_size as u64); // size_bytes
            });

            // COPY_TEXTURE2D (64 bytes, writeback sub-rect)
            emit_packet(out, 0x106, |out| {
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
    mem.write_u32(desc_gpa + 40, alloc_table.len() as u32); // alloc_table_size_bytes
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

    drive_until_fence(&mut mem, &mut dev, 1);

    let mut got = vec![0u8; dst_size];
    mem.read_physical(dst_gpa, &mut got);

    for y in 0..height {
        for x in 0..width {
            let idx = (y as usize * row_pitch as usize) + (x as usize * 4);
            let px = &got[idx..idx + 4];

            let in_rect = x >= dst_x && x < (dst_x + copy_w) && y >= dst_y && y < (dst_y + copy_h);
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
}

#[test]
fn aerogpu_copy_buffer_writeback_requires_guest_backing() {
    let mut mem = Bus::new(0x20_000);

    let mut dev = new_test_device();
    dev.set_backend(Box::new(
        NativeAeroGpuBackend::new_headless().expect("native backend should initialize"),
    ));
    assert_ne!(dev.regs.features & FEATURE_TRANSFER, 0);

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

    let pattern: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];

    // Command stream:
    // - create src buffer (host allocated)
    // - upload a known byte pattern
    // - create dst buffer (host allocated)
    // - COPY_BUFFER with WRITEBACK_DST: should raise ERROR because dst has no guest backing.
    let cmd_gpa = 0x4000u64;
    let stream = build_stream(
        |out| {
            // CREATE_BUFFER src (40 bytes)
            emit_packet(out, 0x100, |out| {
                push_u32(out, 1); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, pattern.len() as u64);
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // UPLOAD_RESOURCE into src buffer (32 + payload)
            emit_packet(out, 0x104, |out| {
                push_u32(out, 1); // resource_handle
                push_u32(out, 0); // reserved0
                push_u64(out, 0); // offset_bytes
                push_u64(out, pattern.len() as u64);
                out.extend_from_slice(&pattern);
            });

            // CREATE_BUFFER dst (40 bytes)
            emit_packet(out, 0x100, |out| {
                push_u32(out, 2); // buffer_handle
                push_u32(out, 0); // usage_flags
                push_u64(out, pattern.len() as u64);
                push_u32(out, 0); // backing_alloc_id
                push_u32(out, 0); // backing_offset_bytes
                push_u64(out, 0); // reserved0
            });

            // COPY_BUFFER (48 bytes)
            emit_packet(out, 0x105, |out| {
                push_u32(out, 2); // dst_buffer
                push_u32(out, 1); // src_buffer
                push_u64(out, 0); // dst_offset_bytes
                push_u64(out, 0); // src_offset_bytes
                push_u64(out, pattern.len() as u64);
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
                push_u32(out, 0); // reserved0
            });
        },
        dev.regs.abi_version,
    );
    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0 (no alloc table required because both buffers are host-backed).
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

    drive_until_fence(&mut mem, &mut dev, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn aerogpu_copy_texture2d_writeback_requires_guest_backing() {
    let mut mem = Bus::new(0x20_000);

    let mut dev = new_test_device();
    dev.set_backend(Box::new(
        NativeAeroGpuBackend::new_headless().expect("native backend should initialize"),
    ));
    assert_ne!(dev.regs.features & FEATURE_TRANSFER, 0);

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

    let (width, height) = (4u32, 4u32);

    // Command stream:
    // - create src texture (host allocated)
    // - create dst texture (host allocated)
    // - COPY_TEXTURE2D with WRITEBACK_DST: should raise ERROR because dst has no guest backing.
    let cmd_gpa = 0x4000u64;
    let stream = build_stream(
        |out| {
            // CREATE_TEXTURE2D src (56 bytes)
            emit_packet(out, 0x101, |out| {
                push_u32(out, 1); // texture_handle
                push_u32(out, 0); // usage_flags
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

            // CREATE_TEXTURE2D dst (56 bytes)
            emit_packet(out, 0x101, |out| {
                push_u32(out, 2); // texture_handle
                push_u32(out, 0); // usage_flags
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

            // COPY_TEXTURE2D (64 bytes)
            emit_packet(out, 0x106, |out| {
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
                push_u32(out, width);
                push_u32(out, height);
                push_u32(out, AEROGPU_COPY_FLAG_WRITEBACK_DST);
                push_u32(out, 0); // reserved0
            });
        },
        dev.regs.abi_version,
    );
    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0 (no alloc table required because both textures are host-backed).
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

    drive_until_fence(&mut mem, &mut dev, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
}
