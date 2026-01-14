#![cfg(not(target_arch = "wasm32"))]

use aero_devices_gpu::backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission, AeroGpuCommandBackend,
};
use aero_machine::{Machine, MachineConfig};
use memory::MemoryBus;
use pretty_assertions::assert_eq;

#[derive(Default)]
struct FakeScanoutBackend {
    scanout0: Option<AeroGpuBackendScanout>,
    completions: Vec<AeroGpuBackendCompletion>,
}

impl FakeScanoutBackend {
    fn with_solid_rgba8(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let mut rgba8 = Vec::new();
        rgba8.resize((width * height * 4) as usize, 0);
        for px in rgba8.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        Self {
            scanout0: Some(AeroGpuBackendScanout {
                width,
                height,
                rgba8,
            }),
            completions: Vec::new(),
        }
    }
}

impl AeroGpuCommandBackend for FakeScanoutBackend {
    fn reset(&mut self) {
        self.completions.clear();
    }

    fn submit(
        &mut self,
        _mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        self.completions.push(AeroGpuBackendCompletion {
            fence: submission.signal_fence,
            error: None,
        });
        Ok(())
    }

    fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
        std::mem::take(&mut self.completions)
    }

    fn read_scanout_rgba8(&mut self, scanout_id: u32) -> Option<AeroGpuBackendScanout> {
        if scanout_id == 0 {
            self.scanout0.clone()
        } else {
            None
        }
    }
}

#[test]
fn aerogpu_backend_scanout_is_presented_via_machine_display_present() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal/deterministic for this unit test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let mmio = m.aerogpu_mmio().expect("aerogpu enabled");
    mmio.borrow_mut().set_backend(Box::new(FakeScanoutBackend::with_solid_rgba8(
        2,
        2,
        [1, 2, 3, 255],
    )));

    m.display_present();
    assert_eq!(m.display_resolution(), (2, 2));

    let fb = m.display_framebuffer();
    assert_eq!(fb.len(), 4);
    assert_eq!(fb[0], u32::from_le_bytes([1, 2, 3, 255]));
}

