pub fn require_webgpu() -> bool {
    let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
        return false;
    };

    let v = raw.trim();
    v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
}

pub fn skip_or_panic(test_name: &str, reason: &str) {
    if require_webgpu() {
        panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
    }
    eprintln!("skipping {test_name}: {reason}");
}

/// Return a shared, leaked `NativeAeroGpuBackend` for this integration-test binary.
///
/// Some wgpu backends/drivers have been observed to crash (or hit OOMs) when repeatedly creating/
/// dropping devices across many `#[test]` cases in a single process. Integration tests that
/// exercise the native backend frequently construct/destroy `NativeAeroGpuBackend`, so we
/// centralize backend creation here and reuse it across tests.
#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn native_backend(
    test_name: &str,
) -> Option<Box<dyn emulator::gpu_worker::aerogpu_backend::AeroGpuCommandBackend>> {
    // The native backend is host-only; keep wasm builds working by treating these tests as skipped.
    skip_or_panic(test_name, "NativeAeroGpuBackend is host-only");
    None
}

#[allow(dead_code)]
#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
pub fn native_backend(
    test_name: &str,
) -> Option<Box<dyn emulator::gpu_worker::aerogpu_backend::AeroGpuCommandBackend>> {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use emulator::gpu_worker::aerogpu_backend::{
        AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
        AeroGpuCommandBackend, NativeAeroGpuBackend,
    };

    struct LockedNativeBackend {
        inner: MutexGuard<'static, NativeAeroGpuBackend>,
    }

    impl AeroGpuCommandBackend for LockedNativeBackend {
        fn reset(&mut self) {
            self.inner.reset();
        }

        fn submit(
            &mut self,
            mem: &mut dyn memory::MemoryBus,
            submission: AeroGpuBackendSubmission,
        ) -> Result<(), String> {
            self.inner.submit(mem, submission)
        }

        fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
            self.inner.poll_completions()
        }

        fn read_scanout_rgba8(&mut self, scanout_id: u32) -> Option<AeroGpuBackendScanout> {
            self.inner.read_scanout_rgba8(scanout_id)
        }
    }

    static BACKEND: OnceLock<Option<&'static Mutex<NativeAeroGpuBackend>>> = OnceLock::new();

    let backend = BACKEND.get_or_init(|| match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => Some(Box::leak(Box::new(Mutex::new(backend)))),
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => None,
        Err(err) => panic!("failed to initialize native AeroGPU backend: {err}"),
    });

    let Some(backend) = backend.as_ref() else {
        skip_or_panic(test_name, "wgpu request_adapter returned None");
        return None;
    };

    let mut guard = backend.lock().unwrap_or_else(|poison| poison.into_inner());
    guard.reset();
    Some(Box::new(LockedNativeBackend { inner: guard }))
}
