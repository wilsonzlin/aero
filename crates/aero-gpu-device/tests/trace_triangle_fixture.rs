use aero_gpu_device::abi;
use aero_gpu_device::backend::SoftGpuBackend;
use aero_gpu_device::device::{GpuDevice, InterruptSink};
use aero_gpu_device::guest::SyntheticGuest;
use aero_gpu_device::guest_memory::{GuestMemory, VecGuestMemory};
use aero_gpu_device::ring::RingLocation;

use aero_gpu_trace::{BlobKind, TraceReader, TraceRecord};

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct TestIrq {
    raised: bool,
}

impl InterruptSink for TestIrq {
    fn raise_irq(&mut self) {
        self.raised = true;
    }

    fn lower_irq(&mut self) {
        self.raised = false;
    }
}

fn fixture_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` = `.../crates/aero-gpu-device`
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/aerogpu_triangle.aerogputrace")
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

fn generate_trace() -> Vec<u8> {
    // Guest physical memory (test-only).
    let mut mem = VecGuestMemory::new(2 * 1024 * 1024);

    let cmd_ring_base = 0x1000;
    let cpl_ring_base = 0x3000;
    let vertex_paddr = 0x10_000;

    // Rings.
    let mut guest = SyntheticGuest::init_rings(
        &mut mem,
        RingLocation {
            base_paddr: cmd_ring_base,
        },
        0x1000,
        RingLocation {
            base_paddr: cpl_ring_base,
        },
        0x1000,
    )
    .unwrap();

    // Device + trace recorder.
    let mut dev = GpuDevice::new(SoftGpuBackend::new());
    dev.start_trace_in_memory("0.0.0-dev").unwrap();
    let mut irq = TestIrq::default();

    // Configure MMIO registers (as a guest driver would).
    dev.mmio_write32(
        abi::mmio::REG_CMD_RING_BASE_LO,
        cmd_ring_base as u32,
        &mut mem,
        Some(&mut irq),
    )
    .unwrap();
    dev.mmio_write32(
        abi::mmio::REG_CMD_RING_BASE_HI,
        (cmd_ring_base >> 32) as u32,
        &mut mem,
        Some(&mut irq),
    )
    .unwrap();
    dev.mmio_write32(
        abi::mmio::REG_CMD_RING_SIZE,
        0x1000,
        &mut mem,
        Some(&mut irq),
    )
    .unwrap();

    dev.mmio_write32(
        abi::mmio::REG_CPL_RING_BASE_LO,
        cpl_ring_base as u32,
        &mut mem,
        Some(&mut irq),
    )
    .unwrap();
    dev.mmio_write32(
        abi::mmio::REG_CPL_RING_BASE_HI,
        (cpl_ring_base >> 32) as u32,
        &mut mem,
        Some(&mut irq),
    )
    .unwrap();
    dev.mmio_write32(
        abi::mmio::REG_CPL_RING_SIZE,
        0x1000,
        &mut mem,
        Some(&mut irq),
    )
    .unwrap();

    dev.mmio_write32(
        abi::mmio::REG_INT_MASK,
        abi::mmio::INT_STATUS_CPL_AVAIL,
        &mut mem,
        Some(&mut irq),
    )
    .unwrap();

    // Vertex data: fullscreen triangle, solid red.
    let verts: [[f32; 6]; 3] = [
        [-1.0, -1.0, 1.0, 0.0, 0.0, 1.0],
        [3.0, -1.0, 1.0, 0.0, 0.0, 1.0],
        [-1.0, 3.0, 1.0, 0.0, 0.0, 1.0],
    ];
    let mut vb = Vec::new();
    for v in verts {
        for f in v {
            push_f32(&mut vb, f);
        }
    }
    mem.write(vertex_paddr, &vb).unwrap();

    // Create render target texture.
    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // texture_id
    push_u32(&mut payload, 64);
    push_u32(&mut payload, 64);
    push_u32(&mut payload, abi::TextureFormat::Rgba8Unorm as u32);
    push_u32(&mut payload, abi::texture_usage::RENDER_ATTACHMENT);
    push_u32(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::CREATE_TEXTURE2D, &payload)
        .unwrap();

    // Bind render target.
    let mut payload = Vec::new();
    push_u32(&mut payload, 1);
    push_u32(&mut payload, 0);
    push_u64(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::SET_RENDER_TARGET, &payload)
        .unwrap();

    // Clear to opaque black.
    let mut payload = Vec::new();
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 1.0);
    push_u64(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::CLEAR, &payload)
        .unwrap();

    // Create vertex buffer + upload.
    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // buffer_id
    push_u32(&mut payload, 0);
    push_u64(&mut payload, vb.len() as u64);
    push_u32(
        &mut payload,
        abi::buffer_usage::VERTEX | abi::buffer_usage::TRANSFER_DST,
    );
    push_u32(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::CREATE_BUFFER, &payload)
        .unwrap();

    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // buffer_id
    push_u32(&mut payload, 0);
    push_u64(&mut payload, 0); // dst_offset
    push_u64(&mut payload, vertex_paddr);
    push_u32(&mut payload, vb.len() as u32);
    push_u32(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::WRITE_BUFFER, &payload)
        .unwrap();

    // Viewport matches render target.
    let mut payload = Vec::new();
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 64.0);
    push_f32(&mut payload, 64.0);
    push_u64(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::SET_VIEWPORT, &payload)
        .unwrap();

    // Pipeline + VB binding.
    let mut payload = Vec::new();
    push_u32(&mut payload, abi::pipeline::BASIC_VERTEX_COLOR);
    push_u32(&mut payload, 0);
    push_u64(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::SET_PIPELINE, &payload)
        .unwrap();

    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // buffer_id
    push_u32(&mut payload, 24); // stride
    push_u64(&mut payload, 0); // offset
    push_u64(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::SET_VERTEX_BUFFER, &payload)
        .unwrap();

    // Draw + present.
    let mut payload = Vec::new();
    push_u32(&mut payload, 3); // vertex_count
    push_u32(&mut payload, 0); // first_vertex
    push_u64(&mut payload, 0);
    guest.submit(&mut mem, abi::opcode::DRAW, &payload).unwrap();

    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // texture_id
    push_u32(&mut payload, 0);
    push_u64(&mut payload, 0);
    guest
        .submit(&mut mem, abi::opcode::PRESENT, &payload)
        .unwrap();

    // Ring the doorbell once; the device should batch-process the full stream.
    dev.mmio_write32(abi::mmio::REG_DOORBELL, 1, &mut mem, Some(&mut irq))
        .unwrap();
    assert!(irq.raised);

    let bytes = dev
        .finish_trace()
        .unwrap()
        .expect("trace should be enabled");

    // Sanity-check trace structure.
    let mut reader = TraceReader::open(Cursor::new(bytes.clone())).expect("TraceReader::open");
    assert_eq!(reader.frame_entries().len(), 1);
    let toc = reader.frame_entries()[0];
    let records = reader
        .read_records_in_range(toc.start_offset, toc.end_offset)
        .expect("read frame records");
    assert!(
        records.iter().any(|r| matches!(
            r,
            TraceRecord::Blob {
                kind: BlobKind::BufferData,
                ..
            }
        )),
        "expected at least one buffer upload blob"
    );

    bytes
}

#[test]
fn aerogpu_triangle_trace_fixture_is_stable() {
    let bytes = generate_trace();

    let path = fixture_path();
    if std::env::var_os("AERO_UPDATE_TRACE_FIXTURES").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        return;
    }

    let fixture =
        fs::read(&path).expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1");
    assert_eq!(bytes, fixture);
}
