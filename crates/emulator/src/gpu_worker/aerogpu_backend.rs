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

/// Boundary between emulator-side ring processing and host GPU execution.
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
        Self::default()
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

#[cfg(feature = "aerogpu-native")]
pub struct NativeAeroGpuBackend {
    exec: aero_gpu::AerogpuD3d9Executor,
    completed: VecDeque<AeroGpuBackendCompletion>,
}

#[cfg(feature = "aerogpu-native")]
impl NativeAeroGpuBackend {
    pub fn new_headless() -> Result<Self, String> {
        let exec = pollster::block_on(aero_gpu::AerogpuD3d9Executor::new_headless())
            .map_err(|e| format!("failed to initialize native AerogpuD3d9Executor backend: {e}"))?;
        Ok(Self {
            exec,
            completed: VecDeque::new(),
        })
    }
}

#[cfg(feature = "aerogpu-native")]
impl AeroGpuCommandBackend for NativeAeroGpuBackend {
    fn reset(&mut self) {
        self.exec.reset();
        self.completed.clear();
    }

    fn submit(
        &mut self,
        _mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        let result = self.exec.execute_cmd_stream(&submission.cmd_stream);

        // Block until GPU work is complete so guest fences match execution progress.
        self.exec.poll();

        // Never drop completions on error; fences must always make progress.
        self.completed.push_back(AeroGpuBackendCompletion {
            fence: submission.signal_fence,
            error: result.as_ref().err().map(|e| e.to_string()),
        });

        result.map_err(|e| e.to_string())
    }

    fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
        self.completed.drain(..).collect()
    }

    fn read_scanout_rgba8(&mut self, scanout_id: u32) -> Option<AeroGpuBackendScanout> {
        let (width, height, rgba8) =
            pollster::block_on(self.exec.read_presented_scanout_rgba8(scanout_id)).ok()??;
        Some(AeroGpuBackendScanout {
            width,
            height,
            rgba8,
        })
    }
}
