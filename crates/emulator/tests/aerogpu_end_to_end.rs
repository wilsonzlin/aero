#![cfg(feature = "aerogpu-native")]

use std::time::{Duration, Instant};

use aero_protocol::aerogpu::aerogpu_cmd::{AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use emulator::devices::aerogpu_regs::{irq_bits, mmio, ring_control};
use emulator::devices::aerogpu_ring::{AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC};
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::gpu_worker::aerogpu_backend::NativeAeroGpuBackend;
use emulator::gpu_worker::aerogpu_executor::{AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
use emulator::io::pci::MmioDevice;
use memory::Bus;
use memory::MemoryBus;

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

#[test]
fn aerogpu_ring_submission_executes_and_updates_scanout() {
    let mut mem = Bus::new(0x20_000);

    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.executor = AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    };

    let mut dev = AeroGpuPciDevice::new(cfg, 0);
    dev.set_backend(Box::new(
        NativeAeroGpuBackend::new_headless().expect("native backend should initialize"),
    ));

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Ring header.
    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Command buffer: create RT texture, bind it, clear green, present scanout0.
    let cmd_gpa = 0x4000u64;
    let (width, height) = (4u32, 4u32);
    let stream = build_stream(
        |out| {
            // CREATE_TEXTURE2D (56 bytes)
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

            // CLEAR (36 bytes)
            emit_packet(out, 0x600, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits()); // r
                push_u32(out, 1.0f32.to_bits()); // g
                push_u32(out, 0.0f32.to_bits()); // b
                push_u32(out, 1.0f32.to_bits()); // a
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });

            // PRESENT (16 bytes)
            emit_packet(out, 0x700, |out| {
                push_u32(out, 0); // scanout_id
                push_u32(out, 0); // flags
            });
        },
        dev.regs.abi_version,
    );

    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa + 0, 64); // desc_size_bytes
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
    for _ in 0..100 {
        if dev.regs.completed_fence >= 1 {
            break;
        }
        now += Duration::from_millis(1);
        dev.tick(&mut mem, now);
    }

    assert_eq!(dev.regs.completed_fence, 1);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);

    let (out_w, out_h, rgba8) = dev
        .read_presented_scanout_rgba8(0)
        .expect("scanout should be readable");

    assert_eq!((out_w, out_h), (width, height));
    assert_eq!(rgba8.len(), (width * height * 4) as usize);

    for px in rgba8.chunks_exact(4) {
        assert_eq!(px, [0, 255, 0, 255]);
    }
}
