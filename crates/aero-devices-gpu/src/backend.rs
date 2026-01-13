use std::collections::VecDeque;

use memory::MemoryBus;

#[derive(Debug, Clone)]
pub struct AeroGpuBackendSubmission {
    pub flags: u32,
    pub context_id: u32,
    pub engine_id: u32,
    pub signal_fence: u64,
    /// Bytes for `aerogpu_cmd_stream_header` + packets (`aerogpu_cmd.h`).
    pub cmd_stream: Vec<u8>,
    /// Optional bytes for the submission's allocation table (`aerogpu_ring.h`).
    pub alloc_table: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct AeroGpuBackendCompletion {
    pub fence: u64,
    /// If set, the submission failed validation/execution on the host.
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AeroGpuBackendScanout {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

/// Boundary between device-model side ring processing and host GPU execution.
///
/// Implementations may execute immediately (synchronous) or enqueue work and later surface fence
/// completions via [`AeroGpuCommandBackend::poll_completions`].
///
/// This trait is intentionally minimal and does not assume any particular threading model so it
/// can be implemented by:
/// - a native in-process executor (wgpu/WebGPU),
/// - a message-based web worker backend, or
/// - a stub backend for headless / no-GPU builds.
pub trait AeroGpuCommandBackend {
    /// Reset backend state (resources, outstanding work, pending completions).
    fn reset(&mut self);

    /// Submit a command buffer for execution.
    ///
    /// `mem` provides access to guest physical memory for implementations that support
    /// `RESOURCE_DIRTY_RANGE` / alloc-table-backed resources.
    ///
    /// Backends must ensure guest fences always make forward progress. Even if the submission
    /// fails validation/execution, implementations should still enqueue a completion for
    /// `submission.signal_fence` and surface the failure via
    /// [`AeroGpuBackendCompletion::error`].
    ///
    /// Return `Err` only for fatal backend failures where a completion cannot be queued.
    fn submit(
        &mut self,
        mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) -> Result<(), String>;

    /// Drain any completed fences since the last poll.
    fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion>;

    /// Read back the last-presented scanout (if available) as RGBA8.
    fn read_scanout_rgba8(&mut self, scanout_id: u32) -> Option<AeroGpuBackendScanout>;
}

/// Backend that ignores submissions and never reports completions.
#[derive(Debug, Default)]
pub struct NullAeroGpuBackend;

impl NullAeroGpuBackend {
    pub fn new() -> Self {
        Self
    }
}

impl AeroGpuCommandBackend for NullAeroGpuBackend {
    fn reset(&mut self) {
        // Stateless.
    }

    fn submit(
        &mut self,
        _mem: &mut dyn MemoryBus,
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

/// Fallback backend: completes fences immediately and performs no rendering.
#[derive(Debug, Default)]
pub struct ImmediateAeroGpuBackend {
    completed: VecDeque<AeroGpuBackendCompletion>,
}

impl ImmediateAeroGpuBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl AeroGpuCommandBackend for ImmediateAeroGpuBackend {
    fn reset(&mut self) {
        self.completed.clear();
    }

    fn submit(
        &mut self,
        _mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        self.completed.push_back(AeroGpuBackendCompletion {
            fence: submission.signal_fence,
            error: None,
        });
        Ok(())
    }

    fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
        self.completed.drain(..).collect()
    }

    fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ZeroMem;

    impl MemoryBus for ZeroMem {
        fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
            buf.fill(0);
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
    }

    #[test]
    fn immediate_backend_completes_fence() {
        let mut backend = ImmediateAeroGpuBackend::new();
        let mut mem = ZeroMem;

        backend
            .submit(
                &mut mem,
                AeroGpuBackendSubmission {
                    flags: 0,
                    context_id: 1,
                    engine_id: 2,
                    signal_fence: 42,
                    cmd_stream: vec![1, 2, 3],
                    alloc_table: None,
                },
            )
            .unwrap();

        let completions = backend.poll_completions();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].fence, 42);
        assert!(completions[0].error.is_none());

        // Drain semantics.
        assert!(backend.poll_completions().is_empty());
    }

    #[test]
    fn reset_clears_pending_completions() {
        let mut backend = ImmediateAeroGpuBackend::new();
        let mut mem = ZeroMem;

        backend
            .submit(
                &mut mem,
                AeroGpuBackendSubmission {
                    flags: 0,
                    context_id: 1,
                    engine_id: 2,
                    signal_fence: 1,
                    cmd_stream: Vec::new(),
                    alloc_table: None,
                },
            )
            .unwrap();

        backend.reset();
        assert!(backend.poll_completions().is_empty());
    }

    #[test]
    fn null_backend_never_completes() {
        let mut backend = NullAeroGpuBackend::new();
        let mut mem = ZeroMem;

        backend
            .submit(
                &mut mem,
                AeroGpuBackendSubmission {
                    flags: 0,
                    context_id: 1,
                    engine_id: 2,
                    signal_fence: 123,
                    cmd_stream: vec![],
                    alloc_table: None,
                },
            )
            .unwrap();

        assert!(backend.poll_completions().is_empty());
    }
}

