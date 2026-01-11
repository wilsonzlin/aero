use aero_gpu_device::abi;
use aero_gpu_device::backend::WebGpuBackend;
use aero_gpu_device::device::{AgpcGpuDevice, InterruptSink};
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
fn headless_webgpu_triangle_renders() {
    pollster::block_on(async {
        let backend = match WebGpuBackend::request_headless(Default::default()).await {
            Ok(backend) => backend,
            Err(err) => {
                eprintln!("skipping aero-gpu-device webgpu test: {err}");
                return;
            }
        };

        let mut mem = VecGuestMemory::new(2 * 1024 * 1024);

        let cmd_ring_base = 0x1000;
        let cpl_ring_base = 0x3000;
        let vertex_paddr = 0x10_000;

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

        let mut dev = AgpcGpuDevice::new(backend);
        let mut irq = TestIrq::default();

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

        // Fullscreen triangle in clip space, constant red.
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

        // Render target (needs TRANSFER_SRC for readback/present).
        let mut payload = Vec::new();
        push_u32(&mut payload, 1); // texture_id
        push_u32(&mut payload, 64);
        push_u32(&mut payload, 64);
        push_u32(&mut payload, abi::TextureFormat::Rgba8Unorm as u32);
        push_u32(
            &mut payload,
            abi::texture_usage::RENDER_ATTACHMENT | abi::texture_usage::TRANSFER_SRC,
        );
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

        // Vertex buffer + upload.
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

        // Viewport matches target size.
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

        let mut payload = Vec::new();
        push_u32(&mut payload, 3); // vertex_count
        push_u32(&mut payload, 0); // first_vertex
        push_u64(&mut payload, 0);
        guest.submit(&mut mem, abi::opcode::DRAW, &payload).unwrap();

        let mut payload = Vec::new();
        push_u32(&mut payload, 1); // texture_id
        push_u32(&mut payload, 0);
        push_u64(&mut payload, 0);
        let seq_present = guest
            .submit(&mut mem, abi::opcode::PRESENT, &payload)
            .unwrap();

        dev.mmio_write32(abi::mmio::REG_DOORBELL, 1, &mut mem, Some(&mut irq))
            .unwrap();
        assert!(
            irq.raised,
            "interrupt should be raised when completions are ready"
        );

        // Ensure present completed successfully.
        let mut present_ok = false;
        for cpl in guest.drain_completions(&mut mem).unwrap() {
            expect_ok(&cpl);
            if cpl.seq == seq_present {
                present_ok = true;
            }
        }
        assert!(present_ok, "missing present completion");

        let frame = dev.take_presented_frame().expect("presented frame");
        assert_eq!((frame.width, frame.height), (64, 64));

        let x = frame.width / 2;
        let y = frame.height / 2;
        let idx = ((y * frame.width + x) * 4) as usize;
        let px = &frame.rgba8[idx..idx + 4];

        assert!(px[0] >= 250, "R channel too low: {}", px[0]);
        assert!(px[1] <= 5, "G channel too high: {}", px[1]);
        assert!(px[2] <= 5, "B channel too high: {}", px[2]);
        assert_eq!(px[3], 255, "A channel not opaque: {}", px[3]);
    });
}
