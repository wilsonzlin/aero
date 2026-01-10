use aero_gpu_device::abi;
use aero_gpu_device::backend::SoftGpuBackend;
use aero_gpu_device::device::{GpuDevice, InterruptSink};
use aero_gpu_device::guest::{Completion, SyntheticGuest};
use aero_gpu_device::guest_memory::{GuestMemory, VecGuestMemory};
use aero_gpu_device::ring::RingLocation;

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

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn expect_ok(cpl: &Completion) {
    assert_eq!(
        cpl.status,
        abi::status::OK,
        "completion for opcode=0x{:04x} seq={} failed with status={}",
        cpl.opcode,
        cpl.seq,
        cpl.status
    );
}

#[test]
fn golden_triangle_list_present() {
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

    // Device.
    let mut dev = GpuDevice::new(SoftGpuBackend::new());
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

    // Vertex data: (pos.x, pos.y, color.rgba) with f32 values.
    // Triangle in NDC space.
    let verts: [[f32; 6]; 3] = [
        [0.0, 0.8, 1.0, 0.0, 0.0, 1.0],   // red (top)
        [-0.8, -0.8, 0.0, 1.0, 0.0, 1.0], // green (left)
        [0.8, -0.8, 0.0, 0.0, 1.0, 1.0],  // blue (right)
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
    let _seq_create_tex = guest
        .submit(&mut mem, abi::opcode::CREATE_TEXTURE2D, &payload)
        .unwrap();

    // Bind render target.
    let mut payload = Vec::new();
    push_u32(&mut payload, 1);
    push_u32(&mut payload, 0);
    push_u64(&mut payload, 0);
    let _seq_rt = guest
        .submit(&mut mem, abi::opcode::SET_RENDER_TARGET, &payload)
        .unwrap();

    // Clear to opaque black.
    let mut payload = Vec::new();
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 1.0);
    push_u64(&mut payload, 0);
    let _seq_clear = guest
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
    let _seq_create_buf = guest
        .submit(&mut mem, abi::opcode::CREATE_BUFFER, &payload)
        .unwrap();

    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // buffer_id
    push_u32(&mut payload, 0);
    push_u64(&mut payload, 0); // dst_offset
    push_u64(&mut payload, vertex_paddr);
    push_u32(&mut payload, vb.len() as u32);
    push_u32(&mut payload, 0);
    let _seq_write_buf = guest
        .submit(&mut mem, abi::opcode::WRITE_BUFFER, &payload)
        .unwrap();

    // Viewport matches render target.
    let mut payload = Vec::new();
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 0.0);
    push_f32(&mut payload, 64.0);
    push_f32(&mut payload, 64.0);
    push_u64(&mut payload, 0);
    let _seq_viewport = guest
        .submit(&mut mem, abi::opcode::SET_VIEWPORT, &payload)
        .unwrap();

    // Pipeline + VB binding.
    let mut payload = Vec::new();
    push_u32(&mut payload, abi::pipeline::BASIC_VERTEX_COLOR);
    push_u32(&mut payload, 0);
    push_u64(&mut payload, 0);
    let _seq_pipeline = guest
        .submit(&mut mem, abi::opcode::SET_PIPELINE, &payload)
        .unwrap();

    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // buffer_id
    push_u32(&mut payload, 24); // stride
    push_u64(&mut payload, 0); // offset
    push_u64(&mut payload, 0);
    let _seq_vb = guest
        .submit(&mut mem, abi::opcode::SET_VERTEX_BUFFER, &payload)
        .unwrap();

    // Draw + present.
    let mut payload = Vec::new();
    push_u32(&mut payload, 3); // vertex_count
    push_u32(&mut payload, 0); // first_vertex
    push_u64(&mut payload, 0);
    let _seq_draw = guest.submit(&mut mem, abi::opcode::DRAW, &payload).unwrap();

    let mut payload = Vec::new();
    push_u32(&mut payload, 1); // texture_id
    push_u32(&mut payload, 0);
    push_u64(&mut payload, 0);
    let seq_present = guest
        .submit(&mut mem, abi::opcode::PRESENT, &payload)
        .unwrap();

    // Ring the doorbell once; the device should batch-process the full stream.
    dev.mmio_write32(abi::mmio::REG_DOORBELL, 1, &mut mem, Some(&mut irq))
        .unwrap();
    assert!(
        irq.raised,
        "device should raise an interrupt when completions are available"
    );

    // Drain completions and ensure we got through present successfully.
    let mut present_ok = false;
    for cpl in guest.drain_completions(&mut mem).unwrap() {
        expect_ok(&cpl);
        if cpl.seq == seq_present {
            present_ok = true;
        }
    }
    assert!(
        present_ok,
        "missing completion for present seq={seq_present}"
    );

    let frame = dev
        .take_presented_frame()
        .expect("expected a presented frame");
    assert_eq!((frame.width, frame.height), (64, 64));
    assert_eq!(frame.rgba8.len(), 64 * 64 * 4);

    // Golden hash of the full RGBA8 framebuffer.
    let hash = fnv1a64(&frame.rgba8);
    const GOLDEN_HASH: u64 = 0xe891_c505_625f_2831;
    assert_eq!(
        hash, GOLDEN_HASH,
        "framebuffer hash mismatch: 0x{hash:016x}"
    );
}
