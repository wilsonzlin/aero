#![cfg(feature = "aerogpu-exec")]

use aero_protocol::aerogpu::aerogpu_cmd::{
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use emulator::devices::aerogpu_regs::ring_control;
use emulator::devices::aerogpu_ring::AEROGPU_RING_MAGIC;
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::gpu_worker::aerogpu_executor::{
    AeroGpuExecutor, AeroGpuExecutorConfig, AeroGpuFenceCompletionMode,
};
use emulator::gpu_worker::aerogpu_wgpu_backend::AerogpuWgpuBackend;
use memory::MemoryBus;

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let range = self.range(paddr, buf.len());
        buf.copy_from_slice(&self.data[range]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let range = self.range(paddr, buf.len());
        self.data[range].copy_from_slice(buf);
    }
}

fn push_cmd(buf: &mut Vec<u8>, opcode: u32, payload: &[u8]) {
    let size_bytes = (8 + payload.len()) as u32;
    buf.extend_from_slice(&opcode.to_le_bytes());
    buf.extend_from_slice(&size_bytes.to_le_bytes());
    buf.extend_from_slice(payload);
}

#[test]
fn doorbell_executes_present_and_updates_scanout() {
    let backend = match AerogpuWgpuBackend::new() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("skipping aerogpu_exec_present: {e:#}");
            return;
        }
    };

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });
    exec.set_backend(Box::new(backend));

    let mut mem = VecMemory::new(0x10000);
    let mut regs = emulator::devices::aerogpu_regs::AeroGpuRegs::default();

    // Configure scanout0 in guest memory (1x1 BGRA).
    let scanout_fb_gpa = 0x8000u64;
    regs.scanout0.enable = true;
    regs.scanout0.width = 1;
    regs.scanout0.height = 1;
    regs.scanout0.format = AeroGpuFormat::B8G8R8A8Unorm;
    regs.scanout0.pitch_bytes = 4;
    regs.scanout0.fb_gpa = scanout_fb_gpa;

    // Build a tiny aerogpu_cmd stream:
    //   CreateTexture2d(handle=1, 1x1)
    //   SetRenderTargets(color0=1)
    //   Clear(color=red)
    //   Present
    let mut cmd = Vec::new();
    cmd.extend_from_slice(&0x444D_4341u32.to_le_bytes()); // "ACMD"
    cmd.extend_from_slice(&regs.abi_version.to_le_bytes());
    cmd.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched below)
    cmd.extend_from_slice(&0u32.to_le_bytes()); // flags
    cmd.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    cmd.extend_from_slice(&0u32.to_le_bytes()); // reserved1

    // CreateTexture2d: payload is 48 bytes after hdr.
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes()); // texture_handle
    payload.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes()); // usage_flags
    payload.extend_from_slice(&(AeroGpuFormat::R8G8B8A8Unorm as u32).to_le_bytes()); // format
    payload.extend_from_slice(&1u32.to_le_bytes()); // width
    payload.extend_from_slice(&1u32.to_le_bytes()); // height
    payload.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
    payload.extend_from_slice(&1u32.to_le_bytes()); // array_layers
    payload.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
    payload.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    payload.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    payload.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    push_cmd(&mut cmd, 0x101, &payload);

    // SetRenderTargets: payload is 40 bytes after hdr.
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes()); // color_count
    payload.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
    payload.extend_from_slice(&1u32.to_le_bytes()); // colors[0]
    for _ in 1..8 {
        payload.extend_from_slice(&0u32.to_le_bytes());
    }
    push_cmd(&mut cmd, 0x400, &payload);

    // Clear: payload is 28 bytes after hdr.
    let mut payload = Vec::new();
    payload.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes()); // flags
    payload.extend_from_slice(&(1.0f32.to_bits()).to_le_bytes()); // r
    payload.extend_from_slice(&(0.0f32.to_bits()).to_le_bytes()); // g
    payload.extend_from_slice(&(0.0f32.to_bits()).to_le_bytes()); // b
    payload.extend_from_slice(&(1.0f32.to_bits()).to_le_bytes()); // a
    payload.extend_from_slice(&(0.0f32.to_bits()).to_le_bytes()); // depth
    payload.extend_from_slice(&0u32.to_le_bytes()); // stencil
    push_cmd(&mut cmd, 0x600, &payload);

    // Present: payload is 8 bytes after hdr.
    let payload = [0u32.to_le_bytes(), 0u32.to_le_bytes()].concat(); // scanout_id=0, flags=0
    push_cmd(&mut cmd, 0x700, &payload);

    // Patch stream size.
    let size_bytes = cmd.len() as u32;
    cmd[8..12].copy_from_slice(&size_bytes.to_le_bytes());

    // Place command buffer in guest memory.
    let cmd_gpa = 0x4000u64;
    mem.write_physical(cmd_gpa, &cmd);

    // Build a ring with one submission.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa + 0, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 1); // flags (PRESENT)
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, cmd.len() as u32);
    mem.write_u64(desc_gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + 48, 1); // signal_fence

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(regs.completed_fence, 1);
    assert_eq!(regs.stats.gpu_exec_errors, 0);

    let head_after = mem.read_u32(ring_gpa + 24);
    assert_eq!(head_after, 1);

    let mut pixel = [0u8; 4];
    mem.read_physical(scanout_fb_gpa, &mut pixel);
    // Scanout is BGRA; clear color was RGBA(255,0,0,255) so expect BGRA(0,0,255,255).
    assert_eq!(pixel, [0, 0, 255, 255]);
}
