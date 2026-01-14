//! AeroGPU command backend abstraction used by the emulator's device model.
//!
//! This module re-exports the canonical backend API from `aero-devices-gpu` so the emulator and
//! the standalone device crates cannot drift.

pub use aero_devices_gpu::backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend, ImmediateAeroGpuBackend, NullAeroGpuBackend,
};

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
pub use aero_devices_gpu::backend::NativeAeroGpuBackend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_api_is_reexported_from_aero_devices_gpu() {
        // DTOs should be the canonical `aero-devices-gpu` definitions (not local copies).
        let submission = aero_devices_gpu::backend::AeroGpuBackendSubmission {
            flags: 0,
            context_id: 1,
            engine_id: 2,
            signal_fence: 3,
            cmd_stream: vec![1, 2, 3],
            alloc_table: None,
        };
        let _: AeroGpuBackendSubmission = submission;

        let _: aero_devices_gpu::backend::NullAeroGpuBackend = NullAeroGpuBackend::new();
        let _: aero_devices_gpu::backend::ImmediateAeroGpuBackend = ImmediateAeroGpuBackend::new();

        // Trait should also be canonical (i.e. implementing the emulator-exported trait should
        // satisfy the `aero-devices-gpu` trait object).
        struct StubBackend;

        impl AeroGpuCommandBackend for StubBackend {
            fn reset(&mut self) {}

            fn submit(
                &mut self,
                _mem: &mut dyn memory::MemoryBus,
                _submission: AeroGpuBackendSubmission,
            ) -> Result<(), String> {
                Ok(())
            }

            fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
                Vec::new()
            }

            fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
                None
            }
        }

        fn accepts_canonical(_backend: &mut dyn aero_devices_gpu::backend::AeroGpuCommandBackend) {}

        let mut backend = StubBackend;
        accepts_canonical(&mut backend);
    }
}
