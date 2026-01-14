#[cfg(feature = "aerogpu-native")]
use std::collections::VecDeque;

#[cfg(feature = "aerogpu-native")]
use memory::MemoryBus;

pub use aero_devices_gpu::backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission, AeroGpuCommandBackend,
    ImmediateAeroGpuBackend, NullAeroGpuBackend,
};

#[cfg(feature = "aerogpu-native")]
use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};

#[cfg(feature = "aerogpu-native")]
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AerogpuAllocTableHeader};

#[cfg(feature = "aerogpu-native")]
pub struct NativeAeroGpuBackend {
    exec: aero_gpu::AerogpuD3d9Executor,
    completed: VecDeque<AeroGpuBackendCompletion>,
}

#[cfg(feature = "aerogpu-native")]
struct MemoryBusGuestMemory<'a> {
    mem: &'a mut dyn MemoryBus,
}

#[cfg(feature = "aerogpu-native")]
impl<'a> MemoryBusGuestMemory<'a> {
    fn new(mem: &'a mut dyn MemoryBus) -> Self {
        Self { mem }
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

#[cfg(feature = "aerogpu-native")]
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

#[cfg(feature = "aerogpu-native")]
impl NativeAeroGpuBackend {
    pub fn new_headless() -> Result<Self, aero_gpu::AerogpuD3d9Error> {
        let exec = pollster::block_on(aero_gpu::AerogpuD3d9Executor::new_headless())?;
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
                // completion record (the executor will raise ERROR IRQ / gpu_exec_errors from that),
                // but still accept the submission so it is not double-counted.
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
