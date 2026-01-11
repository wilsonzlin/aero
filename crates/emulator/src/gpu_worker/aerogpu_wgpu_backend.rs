#![cfg(feature = "aerogpu-exec")]

use std::collections::{HashMap, VecDeque};

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::{GuestMemory, GuestMemoryError};
use aero_protocol::aerogpu::aerogpu_ring::{decode_alloc_table_le, AerogpuAllocEntry};
use anyhow::{anyhow, Result};
use memory::MemoryBus;

use crate::gpu_worker::aerogpu_backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend,
};

pub struct AerogpuWgpuBackend {
    exec: AerogpuD3d11Executor,
    completions: VecDeque<AeroGpuBackendCompletion>,
    presented_scanouts: HashMap<u32, AeroGpuBackendScanout>,
}

impl AerogpuWgpuBackend {
    pub fn new() -> Result<Self> {
        let exec = pollster::block_on(AerogpuD3d11Executor::new_for_tests())?;
        Ok(Self {
            exec,
            completions: VecDeque::new(),
            presented_scanouts: HashMap::new(),
        })
    }
}

struct MemoryBusGuestMemory<'a> {
    mem: &'a mut dyn MemoryBus,
}

impl<'a> MemoryBusGuestMemory<'a> {
    fn new(mem: &'a mut dyn MemoryBus) -> Self {
        Self { mem }
    }
}

impl GuestMemory for MemoryBusGuestMemory<'_> {
    fn read(&mut self, gpa: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        let len = dst.len();
        let _end = gpa
            .checked_add(len as u64)
            .ok_or(GuestMemoryError { gpa, len })?;
        // `MemoryBus` reads are infallible; unmapped accesses yield 0xFF.
        self.mem.read_physical(gpa, dst);
        Ok(())
    }

    fn write(&mut self, gpa: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        let len = src.len();
        let _end = gpa
            .checked_add(len as u64)
            .ok_or(GuestMemoryError { gpa, len })?;
        self.mem.write_physical(gpa, src);
        Ok(())
    }
}

fn decode_alloc_table_bytes(bytes: &[u8]) -> Result<Vec<AerogpuAllocEntry>> {
    let view = decode_alloc_table_le(bytes)
        .map_err(|err| anyhow!("alloc table decode failed: {err:?}"))?;
    Ok(view.entries.into_owned())
}

impl AeroGpuCommandBackend for AerogpuWgpuBackend {
    fn reset(&mut self) {
        self.exec.reset();
        self.completions.clear();
        self.presented_scanouts.clear();
    }

    fn submit(
        &mut self,
        mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        let exec_result: Result<()> = (|| {
            if submission.cmd_stream.is_empty() {
                return Ok(());
            }

            let mut guest_mem = MemoryBusGuestMemory::new(mem);
            let allocs = submission
                .alloc_table
                .as_deref()
                .map(decode_alloc_table_bytes)
                .transpose()?;

            let report = self.exec.execute_cmd_stream(
                &submission.cmd_stream,
                allocs.as_deref(),
                &mut guest_mem,
            )?;

            for present in report.presents {
                let Some(tex_id) = present.presented_render_target else {
                    continue;
                };

                let (width, height) = self.exec.texture_size(tex_id)?;
                let rgba8 = pollster::block_on(self.exec.read_texture_rgba8(tex_id))?;

                self.presented_scanouts.insert(
                    present.scanout_id,
                    AeroGpuBackendScanout {
                        width,
                        height,
                        rgba8,
                    },
                );
            }

            Ok(())
        })();

        // P0 fence semantics: ensure GPU work has completed before signaling the fence.
        self.exec.poll_wait();

        let error = exec_result.as_ref().err().map(|e| e.to_string());

        // Never drop completions on error; fences must always make progress.
        self.completions.push_back(AeroGpuBackendCompletion {
            fence: submission.signal_fence,
            error,
        });

        // Always accept the submission; execution failures are reported via `completion.error`.
        Ok(())
    }

    fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
        self.completions.drain(..).collect()
    }

    fn read_scanout_rgba8(&mut self, scanout_id: u32) -> Option<AeroGpuBackendScanout> {
        self.presented_scanouts.get(&scanout_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
    use aero_protocol::aerogpu::aerogpu_ring::{
        AerogpuAllocTableHeader, AEROGPU_ALLOC_TABLE_MAGIC,
    };
    use memory::Bus;

    fn require_webgpu() -> bool {
        let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
            return false;
        };

        let v = raw.trim();
        v == "1"
            || v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes")
            || v.eq_ignore_ascii_case("on")
    }

    fn skip_or_panic(test_name: &str, reason: &str) {
        if require_webgpu() {
            panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
        }
        eprintln!("skipping {test_name}: {reason}");
    }

    #[test]
    fn decode_alloc_table_bytes_recovers_from_misaligned_buffers() {
        let header_size = AerogpuAllocTableHeader::SIZE_BYTES;
        let entry_size = AerogpuAllocEntry::SIZE_BYTES;
        let size_bytes = u32::try_from(header_size + entry_size).unwrap();

        let mut table = Vec::new();
        table.extend_from_slice(&AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
        table.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        table.extend_from_slice(&size_bytes.to_le_bytes());
        table.extend_from_slice(&1u32.to_le_bytes()); // entry_count
        table.extend_from_slice(&(entry_size as u32).to_le_bytes()); // entry_stride_bytes
        table.extend_from_slice(&0u32.to_le_bytes()); // reserved0

        // One entry.
        table.extend_from_slice(&1u32.to_le_bytes()); // alloc_id
        table.extend_from_slice(&0u32.to_le_bytes()); // flags
        table.extend_from_slice(&0x1000u64.to_le_bytes()); // gpa
        table.extend_from_slice(&0x2000u64.to_le_bytes()); // size_bytes
        table.extend_from_slice(&0u64.to_le_bytes()); // reserved0

        // Force misalignment by offsetting the slice by one byte.
        let mut storage = vec![0u8; table.len() + 1];
        storage[1..].copy_from_slice(&table);

        let entries = decode_alloc_table_bytes(&storage[1..]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alloc_id, 1);
        assert_eq!(entries[0].gpa, 0x1000);
        assert_eq!(entries[0].size_bytes, 0x2000);
    }

    #[test]
    fn submit_surfaces_execution_errors_via_completion() {
        let mut backend = match AerogpuWgpuBackend::new() {
            Ok(backend) => backend,
            Err(err) => {
                skip_or_panic(
                    concat!(
                        module_path!(),
                        "::submit_surfaces_execution_errors_via_completion"
                    ),
                    &format!("wgpu unavailable ({err:#})"),
                );
                return;
            }
        };
        let mut mem = Bus::new(0x1000);

        // Intentionally malformed command stream: too small to contain even the header.
        let submission = AeroGpuBackendSubmission {
            flags: 0,
            context_id: 0,
            engine_id: 0,
            signal_fence: 42,
            cmd_stream: vec![0u8],
            alloc_table: None,
        };

        assert!(backend.submit(&mut mem, submission).is_ok());

        let completions = backend.poll_completions();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].fence, 42);
        assert!(
            completions[0].error.is_some(),
            "expected execution error to be reported via completion"
        );
    }

    #[test]
    fn decode_alloc_table_bytes_accepts_extended_entry_stride() {
        let header_size = AerogpuAllocTableHeader::SIZE_BYTES;
        let entry_size = AerogpuAllocEntry::SIZE_BYTES;
        let entry_stride = entry_size + 16;
        let size_bytes = u32::try_from(header_size + entry_stride).unwrap();

        let mut table = Vec::new();
        table.extend_from_slice(&AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
        table.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        table.extend_from_slice(&size_bytes.to_le_bytes());
        table.extend_from_slice(&1u32.to_le_bytes()); // entry_count
        table.extend_from_slice(&(entry_stride as u32).to_le_bytes()); // entry_stride_bytes
        table.extend_from_slice(&0u32.to_le_bytes()); // reserved0

        // One entry (prefix), then padding to match the stride.
        table.extend_from_slice(&1u32.to_le_bytes()); // alloc_id
        table.extend_from_slice(&0u32.to_le_bytes()); // flags
        table.extend_from_slice(&0x1000u64.to_le_bytes()); // gpa
        table.extend_from_slice(&0x2000u64.to_le_bytes()); // size_bytes
        table.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        table.resize(header_size + entry_stride, 0);

        let entries = decode_alloc_table_bytes(&table).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alloc_id, 1);
        assert_eq!(entries[0].gpa, 0x1000);
        assert_eq!(entries[0].size_bytes, 0x2000);
    }
}
