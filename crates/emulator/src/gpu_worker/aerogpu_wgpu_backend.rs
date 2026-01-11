#![cfg(feature = "aerogpu-exec")]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::{GuestMemory, GuestMemoryError};
use aero_protocol::aerogpu::aerogpu_ring::{
    decode_alloc_table_le, AerogpuAllocEntry, AerogpuAllocTableDecodeError,
};
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
    mem: RefCell<&'a mut dyn MemoryBus>,
}

impl<'a> MemoryBusGuestMemory<'a> {
    fn new(mem: &'a mut dyn MemoryBus) -> Self {
        Self {
            mem: RefCell::new(mem),
        }
    }
}

impl GuestMemory for MemoryBusGuestMemory<'_> {
    fn read(&self, gpa: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        let len = dst.len();
        let _end = gpa
            .checked_add(len as u64)
            .ok_or(GuestMemoryError { gpa, len })?;
        // `MemoryBus` reads are infallible; unmapped accesses yield 0xFF.
        self.mem.borrow_mut().read_physical(gpa, dst);
        Ok(())
    }

    fn write(&self, gpa: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        let len = src.len();
        let _end = gpa
            .checked_add(len as u64)
            .ok_or(GuestMemoryError { gpa, len })?;
        self.mem.borrow_mut().write_physical(gpa, src);
        Ok(())
    }
}

fn decode_alloc_table_bytes(bytes: &[u8]) -> Result<Vec<AerogpuAllocEntry>> {
    match decode_alloc_table_le(bytes) {
        Ok(view) => Ok(view.entries.to_vec()),
        Err(AerogpuAllocTableDecodeError::Misaligned) => {
            // `decode_alloc_table_le` requires the entry array to be aligned. The guest allocation
            // itself is naturally aligned, but a `Vec<u8>` copy is not required to preserve that
            // alignment. Retry using an 8-byte aligned buffer.
            let words = (bytes.len() + 7) / 8;
            let mut aligned = vec![0u64; words];
            let aligned_bytes = unsafe {
                core::slice::from_raw_parts_mut(aligned.as_mut_ptr() as *mut u8, aligned.len() * 8)
            };
            aligned_bytes[..bytes.len()].copy_from_slice(bytes);

            decode_alloc_table_le(&aligned_bytes[..bytes.len()])
                .map(|view| view.entries.to_vec())
                .map_err(|e| anyhow!("alloc table decode failed: {e:?}"))
        }
        Err(err) => Err(anyhow!("alloc table decode failed: {err:?}")),
    }
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

            let guest_mem = MemoryBusGuestMemory::new(mem);
            let allocs = submission
                .alloc_table
                .as_deref()
                .map(decode_alloc_table_bytes)
                .transpose()?;

            let report = self.exec.execute_cmd_stream(
                &submission.cmd_stream,
                allocs.as_deref(),
                &guest_mem,
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

        let result = exec_result.map_err(|e| e.to_string());

        // Never drop completions on error; fences must always make progress.
        self.completions.push_back(AeroGpuBackendCompletion {
            fence: submission.signal_fence,
            error: result.as_ref().err().cloned(),
        });

        result
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
    use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocTableHeader, AEROGPU_ALLOC_TABLE_MAGIC};

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
}
