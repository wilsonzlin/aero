#![cfg(feature = "aerogpu-native")]

mod common;

use std::time::{Duration, Instant};

use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CLEAR_COLOR,
    AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuRingHeader as ProtocolRingHeader, AerogpuSubmitDesc as ProtocolSubmitDesc,
};
use emulator::devices::aerogpu_regs::{irq_bits, mmio, ring_control};
use emulator::devices::aerogpu_ring::{
    AeroGpuSubmitDesc, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC, RING_HEAD_OFFSET,
    RING_TAIL_OFFSET,
};
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::gpu_worker::aerogpu_backend::NativeAeroGpuBackend;
use emulator::gpu_worker::aerogpu_executor::{AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
use emulator::io::pci::{MmioDevice, PciDevice};
use memory::Bus;
use memory::MemoryBus;

const RING_MAGIC_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, magic) as u64;
const RING_ABI_VERSION_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, abi_version) as u64;
const RING_SIZE_BYTES_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, size_bytes) as u64;
const RING_ENTRY_COUNT_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, entry_count) as u64;
const RING_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolRingHeader, entry_stride_bytes) as u64;
const RING_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, flags) as u64;

const SUBMIT_DESC_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, desc_size_bytes) as u64;
const SUBMIT_DESC_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, flags) as u64;
const SUBMIT_DESC_CONTEXT_ID_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, context_id) as u64;
const SUBMIT_DESC_ENGINE_ID_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, engine_id) as u64;
const SUBMIT_DESC_CMD_GPA_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, cmd_gpa) as u64;
const SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, cmd_size_bytes) as u64;
const SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_gpa) as u64;
const SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_size_bytes) as u64;
const SUBMIT_DESC_SIGNAL_FENCE_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, signal_fence) as u64;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

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
    assert!(size_bytes.is_multiple_of(4));
    out[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
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
    out[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    out
}

#[test]
fn aerogpu_ring_submission_executes_and_updates_scanout() {
    let mut mem = Bus::new(0x20_000);

    let cfg = AeroGpuDeviceConfig {
        executor: AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        },
        ..Default::default()
    };

    let mut dev = AeroGpuPciDevice::new(cfg, 0);
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    let backend = match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => backend,
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => {
            common::skip_or_panic(
                concat!(
                    module_path!(),
                    "::aerogpu_ring_submission_executes_and_updates_scanout"
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
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Ring header.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command buffer: create RT texture, bind it, clear green, present scanout0.
    let cmd_gpa = 0x4000u64;
    let (width, height) = (4u32, 4u32);
    let stream = build_stream(
        |out| {
            // CREATE_TEXTURE2D (56 bytes)
            emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |out| {
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
            emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |out| {
                push_u32(out, 1); // color_count
                push_u32(out, 0); // depth_stencil
                push_u32(out, 1); // colors[0]
                for _ in 0..7 {
                    push_u32(out, 0);
                }
            });

            // CLEAR (36 bytes)
            emit_packet(out, AerogpuCmdOpcode::Clear as u32, |out| {
                push_u32(out, AEROGPU_CLEAR_COLOR);
                push_u32(out, 0.0f32.to_bits()); // r
                push_u32(out, 1.0f32.to_bits()); // g
                push_u32(out, 0.0f32.to_bits()); // b
                push_u32(out, 1.0f32.to_bits()); // a
                push_u32(out, 1.0f32.to_bits()); // depth
                push_u32(out, 0); // stencil
            });

            // PRESENT (16 bytes)
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
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    ); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa); // cmd_gpa
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET,
        stream.len() as u32,
    ); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 1); // signal_fence

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
