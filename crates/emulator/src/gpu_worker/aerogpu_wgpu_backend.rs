#![cfg(feature = "aerogpu-exec")]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::{GuestMemory, GuestMemoryError};
use aero_protocol::aerogpu::aerogpu_ring::{
    decode_alloc_table_le, AerogpuAllocEntry, AerogpuAllocTableDecodeError, AerogpuAllocTableHeader,
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
        Err(AerogpuAllocTableDecodeError::Misaligned) => decode_alloc_table_bytes_unaligned(bytes),
        Err(err) => Err(anyhow!("alloc table decode failed: {err:?}")),
    }
}

fn decode_alloc_table_bytes_unaligned(bytes: &[u8]) -> Result<Vec<AerogpuAllocEntry>> {
    // `decode_alloc_table_le` requires the entry array to be aligned so it can cast the entry
    // array directly. When the allocation table bytes originate from an unaligned `Vec<u8>` copy,
    // fall back to decoding entries individually.
    let header = AerogpuAllocTableHeader::decode_from_le_bytes(bytes)
        .map_err(|e| anyhow!("alloc table header decode failed: {e:?}"))?;
    header
        .validate_prefix()
        .map_err(|e| anyhow!("alloc table header validation failed: {e:?}"))?;

    let size_bytes = usize::try_from(header.size_bytes)
        .map_err(|_| anyhow!("alloc table header size_bytes does not fit in usize"))?;
    if size_bytes > bytes.len() {
        return Err(anyhow!(
            "alloc table size_bytes={} exceeds buffer length={}",
            size_bytes,
            bytes.len()
        ));
    }

    let expected_stride = AerogpuAllocEntry::SIZE_BYTES as u32;
    if header.entry_stride_bytes != expected_stride {
        return Err(anyhow!(
            "alloc table entry_stride_bytes={} does not match expected_stride={expected_stride}",
            header.entry_stride_bytes,
        ));
    }

    let entry_count = usize::try_from(header.entry_count)
        .map_err(|_| anyhow!("alloc table header entry_count does not fit in usize"))?;
    let entries_size_bytes = entry_count
        .checked_mul(AerogpuAllocEntry::SIZE_BYTES)
        .ok_or_else(|| anyhow!("alloc table entry_count overflows"))?;

    let header_size_bytes = AerogpuAllocTableHeader::SIZE_BYTES;
    let required_bytes = header_size_bytes
        .checked_add(entries_size_bytes)
        .ok_or_else(|| anyhow!("alloc table size computation overflows"))?;
    if required_bytes > size_bytes {
        return Err(anyhow!(
            "alloc table entries out of bounds: required_bytes={required_bytes} size_bytes={size_bytes}"
        ));
    }

    let mut entries = Vec::with_capacity(entry_count);
    for idx in 0..entry_count {
        let off = header_size_bytes
            .checked_add(
                idx.checked_mul(AerogpuAllocEntry::SIZE_BYTES)
                    .ok_or_else(|| anyhow!("alloc table entry offset overflow"))?,
            )
            .ok_or_else(|| anyhow!("alloc table entry offset overflow"))?;
        let end = off
            .checked_add(AerogpuAllocEntry::SIZE_BYTES)
            .ok_or_else(|| anyhow!("alloc table entry offset overflow"))?;
        entries.push(
            AerogpuAllocEntry::decode_from_le_bytes(&bytes[off..end])
                .map_err(|e| anyhow!("alloc table entry {idx} decode failed: {e:?}"))?,
        );
    }

    Ok(entries)
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
