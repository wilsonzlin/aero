use std::collections::VecDeque;

use memory::MemoryBus;

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AerogpuAllocTableHeader};

/// Submission payload forwarded from the device-side ring executor into a host command backend.
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

impl AeroGpuBackendSubmission {
    /// Total payload size in bytes (command stream + optional allocation table).
    ///
    /// This is used by device models to enforce bounded queues when bridging submissions to an
    /// external executor (e.g. a browser GPU worker).
    #[inline]
    pub fn payload_len_bytes(&self) -> usize {
        self.cmd_stream.len().saturating_add(
            self.alloc_table
                .as_ref()
                .map(|bytes| bytes.len())
                .unwrap_or(0),
        )
    }
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

/// Boundary between device-side ring processing and host GPU execution.
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

/// wgpu-based native backend wrapper (D3D9 executor) that blocks until execution completes.
///
/// This backend is feature-gated so `aero-devices-gpu` can remain lightweight (and WASM-friendly)
/// by default.
#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
pub struct NativeAeroGpuBackend {
    exec: aero_gpu::AerogpuD3d9Executor,
    completed: VecDeque<AeroGpuBackendCompletion>,
}

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
struct MemoryBusGuestMemory<'a> {
    mem: &'a mut dyn MemoryBus,
}

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
impl<'a> MemoryBusGuestMemory<'a> {
    fn new(mem: &'a mut dyn MemoryBus) -> Self {
        Self { mem }
    }
}

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
fn decode_alloc_table(bytes: &[u8]) -> Result<AllocTable, String> {
    let header = AerogpuAllocTableHeader::decode_from_le_bytes(bytes)
        .map_err(|err| format!("failed to decode alloc table header: {err:?}"))?;
    header
        .validate_prefix()
        .map_err(|err| format!("invalid alloc table header: {err:?}"))?;

    let table_size = header.size_bytes as usize;
    if table_size > bytes.len() {
        return Err(format!(
            "alloc table header size_bytes={} exceeds buffer len={}",
            header.size_bytes,
            bytes.len()
        ));
    }

    let stride = header.entry_stride_bytes as usize;
    // Forward-compat: newer guests may extend `aerogpu_alloc_entry` by increasing the stride. The
    // native backend only requires the entry prefix we understand.
    if stride < AerogpuAllocEntry::SIZE_BYTES {
        return Err(format!(
            "alloc table entry_stride_bytes={} is smaller than expected {}",
            header.entry_stride_bytes,
            AerogpuAllocEntry::SIZE_BYTES
        ));
    }

    let mut out = std::collections::HashMap::<u32, AllocEntry>::new();
    for idx in 0..header.entry_count {
        let idx_u64 = idx as u64;
        let entry_offset = idx_u64
            .checked_mul(stride as u64)
            .ok_or_else(|| "alloc table entry offset overflow".to_string())?;
        let start = AerogpuAllocTableHeader::SIZE_BYTES as u64 + entry_offset;
        let start =
            usize::try_from(start).map_err(|_| "alloc table entry offset overflow".to_string())?;
        let end = start + AerogpuAllocEntry::SIZE_BYTES;
        if end > table_size {
            return Err(format!(
                "alloc table entry {idx} out of bounds (end={end}, size_bytes={})",
                header.size_bytes
            ));
        }

        let entry = AerogpuAllocEntry::decode_from_le_bytes(&bytes[start..end])
            .map_err(|err| format!("failed to decode alloc table entry {idx}: {err:?}"))?;
        if entry.alloc_id == 0 {
            return Err(format!("alloc table entry {idx} has alloc_id=0"));
        }
        if entry.size_bytes == 0 {
            return Err(format!(
                "alloc table entry {idx} has size_bytes=0 (alloc_id={})",
                entry.alloc_id
            ));
        }
        if entry.gpa.checked_add(entry.size_bytes).is_none() {
            return Err(format!(
                "alloc table entry {idx} gpa+size overflows (gpa=0x{:x}, size=0x{:x})",
                entry.gpa, entry.size_bytes
            ));
        }
        if let Some(existing) = out.get(&entry.alloc_id) {
            return Err(format!(
                "alloc table contains duplicate alloc_id={} (gpa0=0x{:x} size0={} gpa1=0x{:x} size1={})",
                entry.alloc_id,
                existing.gpa,
                existing.size_bytes,
                entry.gpa,
                entry.size_bytes,
            ));
        }
        out.insert(
            entry.alloc_id,
            AllocEntry {
                flags: entry.flags,
                gpa: entry.gpa,
                size_bytes: entry.size_bytes,
            },
        );
    }

    AllocTable::new(out).map_err(|err| format!("invalid alloc table: {err}"))
}

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
impl aero_gpu::GuestMemory for MemoryBusGuestMemory<'_> {
    fn read(&mut self, gpa: u64, dst: &mut [u8]) -> Result<(), aero_gpu::GuestMemoryError> {
        let len = dst.len();
        let _end = gpa
            .checked_add(len as u64)
            .ok_or(aero_gpu::GuestMemoryError { gpa, len })?;
        // `MemoryBus` reads are infallible; unmapped accesses yield 0xFF.
        self.mem.read_physical(gpa, dst);
        Ok(())
    }

    fn write(&mut self, gpa: u64, src: &[u8]) -> Result<(), aero_gpu::GuestMemoryError> {
        let len = src.len();
        let _end = gpa
            .checked_add(len as u64)
            .ok_or(aero_gpu::GuestMemoryError { gpa, len })?;
        self.mem.write_physical(gpa, src);
        Ok(())
    }
}

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
impl NativeAeroGpuBackend {
    pub fn new_headless() -> Result<Self, aero_gpu::AerogpuD3d9Error> {
        let exec = pollster::block_on(aero_gpu::AerogpuD3d9Executor::new_headless())?;
        Ok(Self {
            exec,
            completed: VecDeque::new(),
        })
    }
}

#[cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]
impl AeroGpuCommandBackend for NativeAeroGpuBackend {
    fn reset(&mut self) {
        self.exec.reset();
        self.completed.clear();
    }

    fn submit(
        &mut self,
        mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        if submission.cmd_stream.is_empty() {
            self.completed.push_back(AeroGpuBackendCompletion {
                fence: submission.signal_fence,
                error: None,
            });
            return Ok(());
        }

        let mut guest_mem = MemoryBusGuestMemory::new(mem);
        let alloc_table = match submission
            .alloc_table
            .as_deref()
            .map(decode_alloc_table)
            .transpose()
        {
            Ok(table) => table,
            Err(err) => {
                self.completed.push_back(AeroGpuBackendCompletion {
                    fence: submission.signal_fence,
                    error: Some(err.clone()),
                });
                // Backends must never block fence progress on errors; surface the failure via the
                // completion record, but still accept the submission so it is not double-counted.
                return Ok(());
            }
        };

        let result = self.exec.execute_cmd_stream_with_guest_memory_for_context(
            submission.context_id,
            &submission.cmd_stream,
            &mut guest_mem,
            alloc_table.as_ref(),
        );

        // Block until GPU work is complete so guest fences match execution progress.
        self.exec.poll();

        // Never drop completions on error; fences must always make progress.
        self.completed.push_back(AeroGpuBackendCompletion {
            fence: submission.signal_fence,
            error: result.as_ref().err().map(|e| e.to_string()),
        });

        // Always accept the submission; execution failures are reported via `completion.error`.
        Ok(())
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
    fn immediate_backend_completes_in_submission_order() {
        let mut backend = ImmediateAeroGpuBackend::new();
        let mut mem = ZeroMem;

        for fence in [10, 11, 12] {
            backend
                .submit(
                    &mut mem,
                    AeroGpuBackendSubmission {
                        flags: 0,
                        context_id: 1,
                        engine_id: 2,
                        signal_fence: fence,
                        cmd_stream: Vec::new(),
                        alloc_table: None,
                    },
                )
                .unwrap();
        }

        let fences: Vec<u64> = backend
            .poll_completions()
            .into_iter()
            .map(|completion| completion.fence)
            .collect();

        assert_eq!(fences, vec![10, 11, 12]);
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
