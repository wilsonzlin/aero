#![cfg(feature = "aerogpu-webgpu")]

mod common;

use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CLEAR_COLOR;
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use emulator::devices::aerogpu_regs::{irq_bits, ring_control, AeroGpuRegs};
use emulator::devices::aerogpu_ring::{
    AeroGpuSubmitDesc, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC, RING_HEAD_OFFSET,
    RING_TAIL_OFFSET,
};
use emulator::devices::aerogpu_scanout::AeroGpuFormat as EmuFormat;
use emulator::gpu_worker::aerogpu_executor::{
    AeroGpuExecutor, AeroGpuExecutorConfig, AeroGpuFenceCompletionMode,
};
use emulator::gpu_worker::aerogpu_webgpu_backend::WebgpuAeroGpuBackend;
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

#[test]
fn webgpu_backend_copies_present_to_guest_scanout() {
    let backend = match WebgpuAeroGpuBackend::new() {
        Ok(backend) => backend,
        Err(err) => {
            common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
            return;
        }
    };

    let mut regs = AeroGpuRegs::default();
    let mut mem = VecMemory::new(0x20_000);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;
    let ring_size_bytes = u32::try_from(
        AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * u64::from(entry_stride),
    )
    .unwrap();

    // Ring header.
    mem.write_u32(ring_gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size_bytes);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    // Scanout framebuffer.
    let fb_gpa = 0x8000u64;
    let (width, height) = (4u32, 4u32);
    let pitch_bytes = width * 4;

    // Command buffer: create RT texture, bind it, clear red, present.
    let cmd_gpa = 0x4000u64;
    let mut w = AerogpuCmdWriter::new();
    w.create_texture2d(
        1,
        aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET
            | aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_SCANOUT,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        width,
        height,
        1,
        1,
        0,
        0,
        0,
    );
    w.set_render_targets(&[1], 0);
    w.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
    w.present(0, 0);
    let mut stream = w.finish();
    stream[4..8].copy_from_slice(&regs.abi_version.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa, AeroGpuSubmitDesc::SIZE_BYTES);
    mem.write_u32(desc_gpa + 4, AeroGpuSubmitDesc::FLAG_PRESENT);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream.len() as u32);
    mem.write_u64(desc_gpa + 32, 0);
    mem.write_u32(desc_gpa + 40, 0);
    mem.write_u64(desc_gpa + 48, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size_bytes;
    regs.ring_control = ring_control::ENABLE;
    regs.irq_enable = irq_bits::FENCE | irq_bits::ERROR;

    regs.scanout0.enable = true;
    regs.scanout0.width = width;
    regs.scanout0.height = height;
    regs.scanout0.format = EmuFormat::B8G8R8X8Unorm;
    regs.scanout0.pitch_bytes = pitch_bytes;
    regs.scanout0.fb_gpa = fb_gpa;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::new(backend));

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(regs.completed_fence, 1);
    assert_eq!(regs.stats.gpu_exec_errors, 0);

    // Scanout0 is configured as B8G8R8X8; clearing RGBA (255,0,0,255) should yield BGRA bytes
    // (0,0,255,255) for the first pixel.
    let mut first_px = [0u8; 4];
    mem.read_physical(fb_gpa, &mut first_px);
    assert_eq!(first_px, [0, 0, 255, 255]);
}
