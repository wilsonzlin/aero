//! Guest RAM write logging for browser-tiered execution.
//!
//! The CPU/JIT runtime (`aero_cpu_core::jit::runtime::JitRuntime`) tracks a monotonically
//! increasing version per 4KiB guest page. Compiled blocks capture a snapshot of the versions for
//! the code pages they cover; the runtime rejects / evicts blocks when the snapshot is stale.
//!
//! In native embeddings, the MMU/bus layer can call `jit.on_guest_write(paddr, len)` whenever a
//! guest write hits RAM. In the browser tiered VM, Tier-0 interpreter writes occur inside the WASM
//! module and are not automatically forwarded to the JIT runtime unless we explicitly plumb them.
//!
//! This module provides a small, bounded log that a `CpuBus` implementation can fill during a
//! block/batch. The embedding is expected to drain the log at a safe boundary (e.g. after each
//! interpreted block or after a `run_blocks` batch) and forward the writes to `JitRuntime`.

/// Maximum number of write ranges to record before falling back to a coarse invalidation.
///
/// The log is drained at interpreter block boundaries, so typical workloads should stay well below
/// this limit. If we overflow, we conservatively invalidate the entire guest RAM region (so stale
/// code cannot run).
const WRITE_LOG_CAP: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuestWrite {
    pub(crate) paddr: u64,
    pub(crate) len: u32,
}

#[derive(Debug)]
pub(crate) struct GuestWriteLog {
    entries: Vec<GuestWrite>,
    overflowed: bool,
}

impl GuestWriteLog {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::with_capacity(WRITE_LOG_CAP),
            overflowed: false,
        }
    }

    /// Record a RAM write at `paddr..paddr+len`.
    pub(crate) fn record(&mut self, paddr: u64, len: usize) {
        if len == 0 {
            return;
        }

        if self.overflowed {
            return;
        }

        // Treat ranges as half-open intervals: `[start, end)`.
        let mut start = paddr;
        let mut end = paddr.saturating_add(len as u64);

        // Merge with any existing range that overlaps or is directly adjacent.
        //
        // Note: we intentionally allow merging out-of-order writes (e.g. stack writes interleaved
        // with other stores) so we do not blow through the fixed log cap with redundant entries.
        let mut i = 0usize;
        while i < self.entries.len() {
            let entry = self.entries[i];
            let entry_start = entry.paddr;
            let entry_end = entry_start.saturating_add(u64::from(entry.len));
            let overlaps_or_adjacent = start <= entry_end && end >= entry_start;
            if overlaps_or_adjacent {
                start = start.min(entry_start);
                end = end.max(entry_end);
                // Remove this entry; continue scanning at the same index.
                self.entries.swap_remove(i);
                continue;
            }
            i += 1;
        }

        let merged_len_u64 = end.saturating_sub(start);
        let Ok(len_u32) = u32::try_from(merged_len_u64) else {
            // Length doesn't fit in u32 (shouldn't happen for wasm32 guest RAM). Fall back to a
            // coarse invalidation on drain.
            self.entries.clear();
            self.overflowed = true;
            return;
        };

        if self.entries.len() >= WRITE_LOG_CAP {
            // Overflow: drop fine-grained detail and fall back to invalidating the full guest RAM
            // region when drained.
            self.entries.clear();
            self.overflowed = true;
            return;
        }

        self.entries.push(GuestWrite {
            paddr: start,
            len: len_u32,
        });
    }

    /// Drain the log into `f`.
    ///
    /// If the log overflowed, calls `f(0, guest_size)` once (clamped to `usize`).
    pub(crate) fn drain_to(&mut self, guest_size: u64, mut f: impl FnMut(u64, usize)) {
        if self.overflowed {
            self.overflowed = false;
            self.entries.clear();
            // When `guest_size` exceeds `usize::MAX` (possible with Q35 high-RAM remap where the
            // guest-physical end can exceed 4GiB even on wasm32), split the coarse invalidation into
            // multiple chunks so we still cover the full guest-physical address space.
            let mut start = 0u64;
            let mut remaining = guest_size;
            while remaining != 0 {
                let chunk_len_u64 = remaining.min(usize::MAX as u64);
                let chunk_len = chunk_len_u64 as usize;
                if chunk_len == 0 {
                    break;
                }
                f(start, chunk_len);
                start = start.saturating_add(chunk_len_u64);
                remaining = remaining.saturating_sub(chunk_len_u64);
            }
            return;
        }

        for entry in self.entries.drain(..) {
            if entry.len == 0 {
                continue;
            }
            f(entry.paddr, entry.len as usize);
        }
    }
}

impl Default for GuestWriteLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use aero_cpu_core::jit::cache::CompiledBlockHandle;
    use aero_cpu_core::jit::runtime::{
        CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
    };

    use super::GuestWriteLog;

    #[test]
    fn write_log_coalesces_out_of_order_overlaps() {
        let mut log = GuestWriteLog::new();

        // Record disjoint ranges, then an overlapping range that intersects an earlier (non-last)
        // entry. The log should coalesce the overlap rather than leaving overlapping entries.
        log.record(100, 10); // [100,110)
        log.record(0, 10); // [0,10)
        log.record(105, 10); // [105,115) overlaps first entry.

        let mut drained: Vec<(u64, usize)> = Vec::new();
        log.drain_to(1_000, |paddr, len| drained.push((paddr, len)));
        drained.sort_by_key(|(paddr, _)| *paddr);

        assert_eq!(drained, vec![(0, 10), (100, 15)]);
    }

    #[test]
    fn write_log_coalesces_bridge_ranges() {
        let mut log = GuestWriteLog::new();

        // Two disjoint ranges with a gap, plus a third range that bridges them (adjacent to both).
        // The final log should contain one merged range.
        log.record(0, 10); // [0,10)
        log.record(20, 10); // [20,30)
        log.record(10, 10); // [10,20) bridges/adjacent to both.

        let mut drained: Vec<(u64, usize)> = Vec::new();
        log.drain_to(1_000, |paddr, len| drained.push((paddr, len)));
        drained.sort_by_key(|(paddr, _)| *paddr);

        assert_eq!(drained, vec![(0, 30)]);
    }

    #[test]
    fn write_log_overflow_falls_back_to_full_invalidation() {
        let mut log = GuestWriteLog::new();

        // Force an overflow by recording many disjoint 1-byte ranges.
        for i in 0..(super::WRITE_LOG_CAP + 1) {
            log.record((i as u64) * 2, 1);
        }

        let mut drained: Vec<(u64, usize)> = Vec::new();
        log.drain_to(0x10_000, |paddr, len| drained.push((paddr, len)));

        assert_eq!(drained, vec![(0, 0x10_000)]);

        // The overflow flag should clear after draining, so we can log again.
        log.record(0x1234, 1);
        drained.clear();
        log.drain_to(0x10_000, |paddr, len| drained.push((paddr, len)));
        assert_eq!(drained, vec![(0x1234, 1)]);
    }

    #[derive(Default)]
    struct NeverExecBackend;

    impl JitBackend for NeverExecBackend {
        type Cpu = ();

        fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
            panic!("JIT backend should not execute in this test");
        }
    }

    #[derive(Clone, Default)]
    struct RecordingCompileSink(Rc<RefCell<Vec<u64>>>);

    impl RecordingCompileSink {
        fn snapshot(&self) -> Vec<u64> {
            self.0.borrow().clone()
        }
    }

    impl CompileRequestSink for RecordingCompileSink {
        fn request_compile(&mut self, entry_rip: u64) {
            self.0.borrow_mut().push(entry_rip);
        }
    }

    #[test]
    fn write_log_drained_to_jit_invalidates_blocks() {
        let entry_rip = 0x1000u64;
        let code_len = 16u32;
        let guest_size = 0x10_000u64;

        let cfg = JitConfig {
            enabled: true,
            hot_threshold: 1_000_000,
            cache_max_blocks: 16,
            cache_max_bytes: 0,
        };
        let compile = RecordingCompileSink::default();
        let mut jit = JitRuntime::new(cfg, NeverExecBackend, compile.clone());

        // Install a fake compiled block with a meta snapshot.
        let meta = jit.snapshot_meta(entry_rip, code_len);
        jit.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index: 0,
            meta,
        });
        assert!(jit.is_compiled(entry_rip));
        assert!(jit.prepare_block(entry_rip).is_some());
        assert!(compile.snapshot().is_empty());

        // Simulate an interpreter write to the code page and flush it into the JIT runtime.
        let mut log = GuestWriteLog::new();
        log.record(entry_rip + 4, 1);
        log.drain_to(guest_size, |paddr, len| jit.on_guest_write(paddr, len));

        // Next probe should invalidate + request recompilation.
        assert!(jit.prepare_block(entry_rip).is_none());
        assert!(!jit.is_compiled(entry_rip));
        assert_eq!(compile.snapshot(), vec![entry_rip]);
    }
}
