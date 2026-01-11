#![cfg(feature = "aerogpu-exec")]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::{GuestMemory, GuestMemoryError};
use aero_protocol::aerogpu::aerogpu_ring::{AerogpuAllocEntry, AerogpuAllocTableHeader};
use anyhow::{anyhow, bail, Result};
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
    let header = AerogpuAllocTableHeader::decode_from_le_bytes(bytes)
        .map_err(|e| anyhow!("alloc table header decode failed: {e:?}"))?;
    header
        .validate_prefix()
        .map_err(|e| anyhow!("alloc table header validation failed: {e:?}"))?;

    let size_bytes = header.size_bytes as usize;
    if size_bytes > bytes.len() {
        bail!(
            "alloc table size_bytes={} exceeds buffer length={}",
            size_bytes,
            bytes.len()
        );
    }

    let entry_count = header.entry_count as usize;
    let stride = header.entry_stride_bytes as usize;
    let header_size = AerogpuAllocTableHeader::SIZE_BYTES;

    let mut entries = Vec::with_capacity(entry_count);
    for idx in 0..entry_count {
        let off = header_size
            .checked_add(
                idx.checked_mul(stride)
                    .ok_or_else(|| anyhow!("alloc table index overflow"))?,
            )
            .ok_or_else(|| anyhow!("alloc table index overflow"))?;
        let end = off
            .checked_add(AerogpuAllocEntry::SIZE_BYTES)
            .ok_or_else(|| anyhow!("alloc table entry overflow"))?;
        if end > size_bytes {
            bail!(
                "alloc table entry {} out of bounds: entry_end={} size_bytes={}",
                idx,
                end,
                size_bytes
            );
        }
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
