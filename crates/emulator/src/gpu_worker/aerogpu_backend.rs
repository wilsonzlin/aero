#[cfg(feature = "aerogpu-native")]
use std::cell::RefCell;
use std::collections::VecDeque;

use memory::MemoryBus;

#[cfg(feature = "aerogpu-native")]
use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};

#[cfg(feature = "aerogpu-native")]
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AerogpuAllocTableHeader};

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
struct MemoryBusGuestMemory<'a> {
    mem: RefCell<&'a mut dyn MemoryBus>,
}

#[cfg(feature = "aerogpu-native")]
impl<'a> MemoryBusGuestMemory<'a> {
    fn new(mem: &'a mut dyn MemoryBus) -> Self {
        Self {
            mem: RefCell::new(mem),
        }
    }
}

#[cfg(feature = "aerogpu-native")]
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
    if stride < AerogpuAllocEntry::SIZE_BYTES {
        return Err(format!(
            "alloc table entry_stride_bytes={} too small (min {})",
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
        let start = AerogpuAllocTableHeader::SIZE_BYTES as u64
            + entry_offset;
        let start = usize::try_from(start)
            .map_err(|_| "alloc table entry offset overflow".to_string())?;
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
        if out.contains_key(&entry.alloc_id) {
            return Err(format!(
                "alloc table contains duplicate alloc_id={}",
                entry.alloc_id
            ));
        }
        out.insert(
            entry.alloc_id,
            AllocEntry {
                gpa: entry.gpa,
                size_bytes: entry.size_bytes,
            },
        );
    }

    Ok(AllocTable::new(out))
}

#[cfg(feature = "aerogpu-native")]
impl aero_gpu::GuestMemory for MemoryBusGuestMemory<'_> {
    fn read(&self, gpa: u64, dst: &mut [u8]) -> Result<(), aero_gpu::GuestMemoryError> {
        let len = dst.len();
        let _end = gpa
            .checked_add(len as u64)
            .ok_or(aero_gpu::GuestMemoryError { gpa, len })?;
        // `MemoryBus` reads are infallible; unmapped accesses yield 0xFF.
        self.mem.borrow_mut().read_physical(gpa, dst);
        Ok(())
    }
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
        mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        let guest_mem = MemoryBusGuestMemory::new(mem);
        let alloc_table = submission
            .alloc_table
            .as_deref()
            .map(decode_alloc_table)
            .transpose()?;

        let result = self.exec.execute_cmd_stream_with_guest_memory(
            &submission.cmd_stream,
            &guest_mem,
            alloc_table.as_ref(),
        );

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
