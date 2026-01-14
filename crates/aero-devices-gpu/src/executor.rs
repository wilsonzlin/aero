use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashSet, VecDeque};

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{SnapshotError, SnapshotResult};
use aero_protocol::aerogpu::aerogpu_cmd::{
    cmd_stream_has_vsync_present_bytes, cmd_stream_has_vsync_present_reader,
    decode_cmd_stream_header_le, AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
};
use memory::MemoryBus;

use crate::backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend, NullAeroGpuBackend,
};
use crate::regs::{irq_bits, ring_control, AeroGpuRegs, AerogpuErrorCode, FEATURE_VBLANK};
use crate::ring::{
    AeroGpuAllocEntry, AeroGpuAllocTableHeader, AeroGpuRingHeader, AeroGpuSubmitDesc,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES,
};
use crate::scanout::AeroGpuFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AeroGpuFenceCompletionMode {
    /// Legacy bring-up behavior: submissions complete inside the executor (optionally paced by
    /// vblank if the command stream contains a vsynced present).
    Immediate,
    /// Submissions remain "in flight" until `complete_fence` is called (out-of-order capable).
    Deferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingFenceKind {
    Immediate,
    Vblank,
}

#[derive(Clone, Copy, Debug)]
struct PendingFenceCompletion {
    fence: u64,
    wants_irq: bool,
    kind: PendingFenceKind,
}

#[derive(Clone, Debug)]
pub struct AeroGpuExecutorConfig {
    pub verbose: bool,
    pub keep_last_submissions: usize,
    pub fence_completion: AeroGpuFenceCompletionMode,
}

impl Default for AeroGpuExecutorConfig {
    fn default() -> Self {
        Self {
            verbose: false,
            keep_last_submissions: 64,
            fence_completion: AeroGpuFenceCompletionMode::Immediate,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AeroGpuSubmissionRecord {
    pub ring_head: u32,
    pub ring_tail: u32,
    pub submission: AeroGpuSubmission,
    pub decode_errors: Vec<AeroGpuSubmissionDecodeError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AeroGpuCmdStreamHeader {
    pub magic: u32,
    pub abi_version: u32,
    pub size_bytes: u32,
    pub flags: u32,
}

impl From<ProtocolCmdStreamHeader> for AeroGpuCmdStreamHeader {
    fn from(value: ProtocolCmdStreamHeader) -> Self {
        Self {
            magic: value.magic,
            abi_version: value.abi_version,
            size_bytes: value.size_bytes,
            flags: value.flags,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AeroGpuSubmission {
    pub desc: AeroGpuSubmitDesc,
    pub alloc_table_header: Option<AeroGpuAllocTableHeader>,
    pub allocs: Vec<AeroGpuAllocEntry>,
    pub cmd_stream_header: Option<AeroGpuCmdStreamHeader>,
    pub cmd_stream: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AeroGpuSubmissionDecodeError {
    AllocTable(AeroGpuAllocTableDecodeError),
    CmdStream(AeroGpuCmdStreamDecodeError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AeroGpuAllocTableDecodeError {
    InconsistentDescriptor,
    TooLarge,
    BadMagic,
    BadAbiVersion,
    SizeTooSmall,
    SizeExceedsDescriptor,
    BadEntryStride,
    EntriesOutOfBounds,
    InvalidEntry,
    DuplicateAllocId,
    AddressOverflow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AeroGpuCmdStreamDecodeError {
    InconsistentDescriptor,
    AddressOverflow,
    TooLarge,
    TooSmall,
    BadHeader,
    StreamSizeTooLarge,
}

pub struct AeroGpuExecutor {
    cfg: AeroGpuExecutorConfig,
    pub last_submissions: VecDeque<AeroGpuSubmissionRecord>,
    /// Submissions that have been decoded by `process_doorbell` but not yet executed by an
    /// in-process backend.
    ///
    /// This queue is only populated when `fence_completion == Deferred` **and** no backend has
    /// been configured via [`AeroGpuExecutor::set_backend`]. It exists as an integration hook for
    /// out-of-process/WASM backends (e.g. `aero-wasm` CPU worker decoding submissions and handing
    /// them off to `aero-gpu-wasm` for execution).
    ///
    /// Native in-process backends should call [`AeroGpuExecutor::set_backend`] which disables this
    /// queue and preserves the existing submit-to-backend behavior.
    pending_submissions: VecDeque<AeroGpuBackendSubmission>,
    pending_submissions_bytes: usize,
    pending_fences: VecDeque<PendingFenceCompletion>,
    in_flight: BTreeMap<u64, InFlightSubmission>,
    completed_before_submit: HashSet<u64>,
    backend_configured: bool,
    backend: Box<dyn AeroGpuCommandBackend>,
}

impl Clone for AeroGpuExecutor {
    fn clone(&self) -> Self {
        Self {
            cfg: self.cfg.clone(),
            last_submissions: self.last_submissions.clone(),
            pending_submissions: self.pending_submissions.clone(),
            pending_submissions_bytes: self.pending_submissions_bytes,
            pending_fences: self.pending_fences.clone(),
            in_flight: self.in_flight.clone(),
            completed_before_submit: self.completed_before_submit.clone(),
            backend_configured: false,
            backend: Box::new(NullAeroGpuBackend::new()),
        }
    }
}

impl AeroGpuExecutor {
    pub fn new(cfg: AeroGpuExecutorConfig) -> Self {
        Self {
            cfg,
            last_submissions: VecDeque::new(),
            pending_submissions: VecDeque::new(),
            pending_submissions_bytes: 0,
            pending_fences: VecDeque::new(),
            in_flight: BTreeMap::new(),
            completed_before_submit: HashSet::new(),
            backend_configured: false,
            backend: Box::new(NullAeroGpuBackend::new()),
        }
    }

    pub fn set_backend(&mut self, backend: Box<dyn AeroGpuCommandBackend>) {
        // Once a backend is configured, this executor is expected to submit work directly to it.
        // Clear any queued drain-mode submissions to avoid mixing execution models.
        self.pending_submissions.clear();
        self.pending_submissions_bytes = 0;
        self.backend_configured = true;
        self.backend = backend;
    }

    /// Drain newly-decoded submissions queued since the last call.
    ///
    /// This is only meaningful when:
    /// - `fence_completion == Deferred`, and
    /// - no backend has been configured (i.e. [`AeroGpuExecutor::set_backend`] has not been called).
    ///
    /// In that mode, `process_doorbell` will decode ring entries, mark their fences as in-flight,
    /// and queue [`AeroGpuBackendSubmission`] structs here for an external executor to run.
    ///
    /// Note: some guest drivers may emit submissions that reuse fence values (including fence
    /// values that have already completed). These submissions can still carry important side
    /// effects (e.g. shared-surface release) even if they do not advance the completed fence, so
    /// this queue is drained unconditionally.
    pub fn drain_pending_submissions(&mut self) -> Vec<AeroGpuBackendSubmission> {
        if self.pending_submissions.is_empty() {
            return Vec::new();
        }

        self.pending_submissions_bytes = 0;
        core::mem::take(&mut self.pending_submissions)
            .into_iter()
            .collect()
    }

    fn submission_payload_len_bytes(sub: &AeroGpuBackendSubmission) -> usize {
        sub.cmd_stream
            .len()
            .saturating_add(sub.alloc_table.as_ref().map(|b| b.len()).unwrap_or(0))
    }

    fn push_pending_submission(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) {
        let sub_bytes = Self::submission_payload_len_bytes(&submission);
        if sub_bytes > MAX_PENDING_SUBMISSIONS_TOTAL_BYTES {
            // A single submission is too large to queue safely. Treat as backend failure to avoid
            // unbounded allocations and guest deadlocks.
            let fence = submission.signal_fence;
            if fence != 0 && fence > regs.completed_fence {
                regs.stats.gpu_exec_errors = regs.stats.gpu_exec_errors.saturating_add(1);
                regs.record_error(AerogpuErrorCode::Backend, fence);
                if let Some(entry) = self.in_flight.get_mut(&fence) {
                    entry.vblank_ready = true;
                }
                self.complete_fence(regs, mem, fence);
            }
            return;
        }

        // Keep the pending submissions queue bounded so hostile/buggy guests can't cause
        // unbounded host allocations in the external-backend (WASM bridge) path.
        while self.pending_submissions.len() >= MAX_PENDING_SUBMISSIONS
            || self.pending_submissions_bytes.saturating_add(sub_bytes)
                > MAX_PENDING_SUBMISSIONS_TOTAL_BYTES
        {
            let Some(dropped) = self.pending_submissions.pop_front() else {
                self.pending_submissions_bytes = 0;
                break;
            };
            let dropped_bytes = Self::submission_payload_len_bytes(&dropped);
            self.pending_submissions_bytes =
                self.pending_submissions_bytes.saturating_sub(dropped_bytes);

            let fence = dropped.signal_fence;
            if fence != 0 && fence > regs.completed_fence {
                regs.stats.gpu_exec_errors = regs.stats.gpu_exec_errors.saturating_add(1);
                regs.record_error(AerogpuErrorCode::Backend, fence);
                if let Some(entry) = self.in_flight.get_mut(&fence) {
                    // If we dropped the submission, do not allow it to block on vblank pacing.
                    entry.vblank_ready = true;
                }
                self.complete_fence(regs, mem, fence);
            }
        }

        // `push_back` may allocate; reserve fallibly to avoid aborting on OOM.
        if self.pending_submissions.try_reserve(1).is_err() {
            let fence = submission.signal_fence;
            if fence != 0 && fence > regs.completed_fence {
                regs.stats.gpu_exec_errors = regs.stats.gpu_exec_errors.saturating_add(1);
                regs.record_error(AerogpuErrorCode::Backend, fence);
                if let Some(entry) = self.in_flight.get_mut(&fence) {
                    entry.vblank_ready = true;
                }
                self.complete_fence(regs, mem, fence);
            }
            return;
        }

        self.pending_submissions_bytes = self.pending_submissions_bytes.saturating_add(sub_bytes);
        self.pending_submissions.push_back(submission);
    }

    pub(crate) fn save_pending_submissions_snapshot_state(&self) -> Option<Vec<u8>> {
        if self.pending_submissions.is_empty() {
            return None;
        }

        let mut enc = Encoder::new().u32(self.pending_submissions.len() as u32);
        for sub in &self.pending_submissions {
            enc = enc
                .u32(sub.flags)
                .u32(sub.context_id)
                .u32(sub.engine_id)
                .u64(sub.signal_fence)
                .u32(sub.cmd_stream.len() as u32)
                .bytes(&sub.cmd_stream);
            match sub.alloc_table.as_ref().filter(|bytes| !bytes.is_empty()) {
                Some(bytes) => {
                    enc = enc.u32(bytes.len() as u32).bytes(bytes);
                }
                None => {
                    enc = enc.u32(0);
                }
            }
        }

        Some(enc.finish())
    }

    pub(crate) fn load_pending_submissions_snapshot_state(
        &mut self,
        bytes: &[u8],
    ) -> SnapshotResult<()> {
        // This queue may contain arbitrary guest command streams. Since snapshots can come from
        // untrusted sources, cap sizes to keep decode bounded.

        let mut d = Decoder::new(bytes);
        let count = d.u32()? as usize;
        if count > MAX_PENDING_SUBMISSIONS {
            return Err(SnapshotError::InvalidFieldEncoding("pending_submissions"));
        }

        let mut pending = VecDeque::new();
        pending
            .try_reserve_exact(count)
            .map_err(|_| SnapshotError::OutOfMemory)?;
        let mut total_bytes = 0usize;

        for _ in 0..count {
            let flags = d.u32()?;
            let context_id = d.u32()?;
            let engine_id = d.u32()?;
            let signal_fence = d.u64()?;

            let cmd_len = d.u32()? as usize;
            if cmd_len > MAX_CMD_STREAM_SIZE_BYTES as usize {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "pending_submissions.cmd_stream",
                ));
            }
            let cmd_stream = d.bytes_vec(cmd_len)?;
            total_bytes = total_bytes.saturating_add(cmd_stream.len());

            let alloc_len = d.u32()? as usize;
            if alloc_len > MAX_ALLOC_TABLE_SIZE_BYTES as usize {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "pending_submissions.alloc_table",
                ));
            }
            let alloc_table = if alloc_len == 0 {
                None
            } else {
                let bytes = d.bytes_vec(alloc_len)?;
                total_bytes = total_bytes.saturating_add(bytes.len());
                Some(bytes)
            };
            if total_bytes > MAX_PENDING_SUBMISSIONS_TOTAL_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "pending_submissions.total_bytes",
                ));
            }

            pending.push_back(AeroGpuBackendSubmission {
                flags,
                context_id,
                engine_id,
                signal_fence,
                cmd_stream,
                alloc_table,
            });
        }

        d.finish()?;

        self.pending_submissions = pending;
        self.pending_submissions_bytes = total_bytes;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.pending_submissions.clear();
        self.pending_submissions_bytes = 0;
        self.pending_fences.clear();
        self.in_flight.clear();
        self.completed_before_submit.clear();
        self.backend.reset();
    }

    /// Flush any fences that are blocked on vblank pacing.
    ///
    /// Callers should invoke this when vblank pacing is disabled (by configuration or by disabling
    /// scanout0) to prevent guests from waiting forever on a vblank that will never arrive.
    pub fn flush_pending_fences(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        if self.cfg.fence_completion != AeroGpuFenceCompletionMode::Immediate {
            // If vblank pacing is disabled, don't allow vsync-gated fences to remain blocked.
            for entry in self.in_flight.values_mut() {
                if entry.kind == PendingFenceKind::Vblank {
                    entry.vblank_ready = true;
                }
            }
            self.advance_completed_fence(regs, mem);
            return;
        }
        if self.pending_fences.is_empty() {
            return;
        }

        let mut advanced = false;
        let mut wants_irq = false;
        while let Some(entry) = self.pending_fences.pop_front() {
            if entry.fence > regs.completed_fence {
                regs.completed_fence = entry.fence;
                advanced = true;
                wants_irq |= entry.wants_irq;
            }
        }

        if advanced {
            self.write_fence_page(regs, mem);
            self.maybe_raise_fence_irq(regs, wants_irq);
        }
    }

    pub fn process_vblank_tick(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        // Only latch the vblank IRQ status bit while:
        // - vblank is actually active (feature enabled, scanout enabled), and
        // - the guest has the IRQ bit enabled.
        //
        // This prevents an immediate "stale" interrupt on re-enable.
        if (regs.features & FEATURE_VBLANK) != 0
            && regs.scanout0.enable
            && (regs.irq_enable & irq_bits::SCANOUT_VBLANK) != 0
        {
            regs.irq_status |= irq_bits::SCANOUT_VBLANK;
        }

        if self.cfg.fence_completion != AeroGpuFenceCompletionMode::Immediate {
            // Complete at most one vsync-delayed fence per vblank tick.
            if let Some((_, entry)) = self.in_flight.iter_mut().next() {
                if entry.kind == PendingFenceKind::Vblank
                    && entry.completed_backend
                    && !entry.vblank_ready
                {
                    entry.vblank_ready = true;
                }
            }
            self.advance_completed_fence(regs, mem);
            return;
        }
        // Complete at most one vsync-delayed fence per vblank tick.
        let mut to_complete = Vec::new();
        if matches!(
            self.pending_fences.front().map(|e| e.kind),
            Some(PendingFenceKind::Vblank)
        ) {
            to_complete.push(self.pending_fences.pop_front().unwrap());
        }

        // Any immediate submissions queued behind a vsync fence become eligible once the vsync
        // fence completes.
        while matches!(
            self.pending_fences.front().map(|e| e.kind),
            Some(PendingFenceKind::Immediate)
        ) {
            to_complete.push(self.pending_fences.pop_front().unwrap());
        }

        self.complete_fences(regs, mem, to_complete);
    }

    pub fn complete_fence(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus, fence: u64) {
        if fence <= regs.completed_fence {
            return;
        }

        if let Some(entry) = self.in_flight.get_mut(&fence) {
            entry.completed_backend = true;
        } else {
            // Allow completions to arrive before `process_doorbell` consumes the corresponding
            // descriptor. We'll apply this completion when the submit arrives.
            self.completed_before_submit.insert(fence);
            return;
        }

        self.advance_completed_fence(regs, mem);
    }

    pub fn poll_backend_completions(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        let completions = self.backend.poll_completions();
        if completions.is_empty() {
            return;
        }

        for AeroGpuBackendCompletion { fence, error } in completions {
            if error.is_some() {
                regs.stats.gpu_exec_errors = regs.stats.gpu_exec_errors.saturating_add(1);
                regs.record_error(AerogpuErrorCode::Backend, fence);
            }

            // Present writeback (deferred mode only): copy the last-presented scanout into the guest
            // framebuffer for scanout0 so host callers that render from guest memory see updates.
            if self.cfg.fence_completion == AeroGpuFenceCompletionMode::Deferred
                && fence > regs.completed_fence
                && matches!(
                    self.in_flight.get(&fence).map(|e| e.desc.flags & AeroGpuSubmitDesc::FLAG_PRESENT),
                    Some(flags) if flags != 0
                )
            {
                if let Some(scanout) = self.backend.read_scanout_rgba8(0) {
                    if let Err(err) = write_scanout0_rgba8(regs, mem, &scanout) {
                        if self.cfg.verbose {
                            eprintln!("aerogpu: scanout writeback failed: {err}");
                        }
                        regs.stats.gpu_exec_errors = regs.stats.gpu_exec_errors.saturating_add(1);
                        regs.record_error(AerogpuErrorCode::Backend, fence);
                    }
                }
            }

            if self.cfg.fence_completion == AeroGpuFenceCompletionMode::Deferred {
                self.complete_fence(regs, mem, fence);
            }
        }
    }

    pub fn read_presented_scanout_rgba8(
        &mut self,
        scanout_id: u32,
    ) -> Option<AeroGpuBackendScanout> {
        self.backend.read_scanout_rgba8(scanout_id)
    }

    fn advance_completed_fence(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        let mut advanced = false;
        let mut wants_irq = false;

        loop {
            let Some(next_fence) = self.in_flight.keys().next().copied() else {
                break;
            };

            // Defensive: drop stale entries if the guest ever reuses a fence value.
            if next_fence <= regs.completed_fence {
                self.in_flight.remove(&next_fence);
                continue;
            }

            let (ready, flags) = {
                let next = self
                    .in_flight
                    .get(&next_fence)
                    .expect("key came from in_flight");
                (next.is_ready(), next.desc.flags)
            };

            if !ready {
                break;
            }

            regs.completed_fence = next_fence;
            advanced = true;
            if flags & AeroGpuSubmitDesc::FLAG_NO_IRQ == 0 {
                wants_irq = true;
            }

            self.in_flight.remove(&next_fence);
        }

        if advanced {
            self.write_fence_page(regs, mem);
            self.maybe_raise_fence_irq(regs, wants_irq);
        }
    }

    pub fn process_doorbell(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        regs.stats.doorbells = regs.stats.doorbells.saturating_add(1);

        // If vblank pacing is not active, do not allow vsynced fences to remain queued forever.
        if (regs.features & FEATURE_VBLANK) == 0 || !regs.scanout0.enable {
            self.flush_pending_fences(regs, mem);
        }

        if regs.ring_control & ring_control::ENABLE == 0 {
            return;
        }
        if regs.ring_gpa == 0 || regs.ring_size_bytes == 0 {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.record_error(AerogpuErrorCode::CmdDecode, 0);
            return;
        }

        // Defensive: reject ring mappings that would wrap the u64 physical address space.
        //
        // Some `MemoryBus` implementations iterate bytewise using `wrapping_add`, so allowing a ring
        // GPA near `u64::MAX` could cause device reads/writes to silently wrap to low memory.
        if regs
            .ring_gpa
            .checked_add(AEROGPU_RING_HEADER_SIZE_BYTES)
            .is_none()
            || regs
                .ring_gpa
                .checked_add(u64::from(regs.ring_size_bytes))
                .is_none()
        {
            // If the ring mapping would wrap, the device cannot safely form descriptor GPAs.
            //
            // Drop any pending work by syncing head -> tail where possible so guests don't get
            // stuck repeatedly ringing the doorbell on an unrecoverable ring mapping. We avoid
            // reading the full ring header here because `ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES`
            // may wrap.
            //
            // These helpers use checked address arithmetic internally, so they will gracefully
            // skip memory accesses if even the head/tail fields cannot be addressed safely.
            let tail = AeroGpuRingHeader::read_tail(mem, regs.ring_gpa);
            AeroGpuRingHeader::write_head(mem, regs.ring_gpa, tail);
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.record_error(AerogpuErrorCode::Oob, 0);
            return;
        }

        let ring = AeroGpuRingHeader::read_from(mem, regs.ring_gpa);
        if !ring.is_valid(regs.ring_size_bytes) {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.record_error(AerogpuErrorCode::CmdDecode, 0);
            return;
        }

        let mut head = ring.head;
        let tail = ring.tail;
        let pending = tail.wrapping_sub(head);
        if pending == 0 {
            return;
        }
        if pending > ring.entry_count {
            // Driver and device are out of sync; drop all pending work to avoid looping.
            AeroGpuRingHeader::write_head(mem, regs.ring_gpa, tail);
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.record_error(AerogpuErrorCode::CmdDecode, 0);
            return;
        }

        let mut processed = 0u32;
        let max = ring.entry_count.min(pending);

        while head != tail && processed < max {
            let desc_gpa = match regs
                .ring_gpa
                .checked_add(AEROGPU_RING_HEADER_SIZE_BYTES)
                .and_then(|base| {
                    let slot = u64::from(ring.slot_index(head));
                    let stride = u64::from(ring.entry_stride_bytes);
                    slot.checked_mul(stride)
                        .and_then(|off| base.checked_add(off))
                }) {
                Some(v) => v,
                None => {
                    // Address overflow when forming the descriptor GPA. Drop all pending work (as if
                    // the ring were corrupted) to avoid looping forever and surface an OOB error.
                    AeroGpuRingHeader::write_head(mem, regs.ring_gpa, tail);
                    regs.stats.malformed_submissions =
                        regs.stats.malformed_submissions.saturating_add(1);
                    regs.record_error(AerogpuErrorCode::Oob, 0);
                    return;
                }
            };
            let desc = AeroGpuSubmitDesc::read_from(mem, desc_gpa);

            regs.stats.submissions = regs.stats.submissions.saturating_add(1);
            if desc.validate_prefix().is_err() || desc.desc_size_bytes > ring.entry_stride_bytes {
                regs.stats.malformed_submissions =
                    regs.stats.malformed_submissions.saturating_add(1);
            }

            let mut decode_errors = Vec::new();
            let (alloc_table_header, allocs) =
                decode_alloc_table(mem, regs.abi_version, &desc, &mut decode_errors);
            let capture_cmd_stream = self.cfg.keep_last_submissions > 0
                || self.cfg.fence_completion == AeroGpuFenceCompletionMode::Deferred;
            let (cmd_stream_header, mut cmd_stream) = decode_cmd_stream(
                mem,
                regs.abi_version,
                &desc,
                &mut decode_errors,
                capture_cmd_stream,
            );

            if !decode_errors.is_empty() {
                regs.stats.malformed_submissions =
                    regs.stats.malformed_submissions.saturating_add(1);
                let mut code = AerogpuErrorCode::CmdDecode;
                for err in &decode_errors {
                    match err {
                        AeroGpuSubmissionDecodeError::AllocTable(
                            AeroGpuAllocTableDecodeError::AddressOverflow
                            | AeroGpuAllocTableDecodeError::EntriesOutOfBounds,
                        )
                        | AeroGpuSubmissionDecodeError::CmdStream(
                            AeroGpuCmdStreamDecodeError::AddressOverflow
                            | AeroGpuCmdStreamDecodeError::StreamSizeTooLarge,
                        ) => {
                            code = AerogpuErrorCode::Oob;
                            break;
                        }
                        _ => {}
                    }
                }
                regs.record_error(code, desc.signal_fence);
            }

            let alloc_count = allocs.len();
            let cmd_header_size = cmd_stream_header
                .as_ref()
                .map(|h| h.size_bytes)
                .unwrap_or(0);
            let decode_error_count = decode_errors.len();

            match self.cfg.fence_completion {
                AeroGpuFenceCompletionMode::Immediate => {
                    let mut vsync_present = false;
                    // Determine whether this submission contains a vsynced PRESENT command.
                    //
                    // Do not rely solely on `AEROGPU_SUBMIT_FLAG_PRESENT`: the contract we need for
                    // Win7 DWM pacing is driven by the *command stream contents* (PRESENT with the
                    // VSYNC flag), and we must be robust even if the submit-level hint bit is
                    // missing.
                    let wants_present = (desc.flags & AeroGpuSubmitDesc::FLAG_PRESENT) != 0;
                    let cmd_stream_ok = desc.cmd_gpa != 0
                        && desc.cmd_size_bytes != 0
                        && cmd_stream_header.is_some()
                        && !decode_errors
                            .iter()
                            .any(|e| matches!(e, AeroGpuSubmissionDecodeError::CmdStream(_)));

                    if cmd_stream_ok {
                        let scan_result = if cmd_stream.is_empty() {
                            cmd_stream_has_vsync_present_reader(
                                |gpa, buf| mem.read_physical(gpa, buf),
                                desc.cmd_gpa,
                                desc.cmd_size_bytes,
                            )
                        } else {
                            cmd_stream_has_vsync_present_bytes(&cmd_stream)
                        };

                        match scan_result {
                            Ok(vsync) => vsync_present = vsync,
                            Err(_) => {
                                // Malformed streams still execute as "immediate" for pacing
                                // purposes (to avoid deadlocks) but are counted for diagnostics.
                                if decode_errors.is_empty() {
                                    regs.stats.malformed_submissions =
                                        regs.stats.malformed_submissions.saturating_add(1);
                                }
                            }
                        }
                    }

                    let mut alloc_table = None;
                    let mut submit_failed = false;
                    if desc.alloc_table_gpa != 0
                        && desc.alloc_table_size_bytes != 0
                        && alloc_table_header.is_some()
                        && !decode_errors
                            .iter()
                            .any(|e| matches!(e, AeroGpuSubmissionDecodeError::AllocTable(_)))
                    {
                        let size_bytes = alloc_table_header
                            .as_ref()
                            .map(|h| h.size_bytes)
                            .unwrap_or(0);
                        let size = size_bytes as usize;
                        if size != 0 {
                            let mut bytes = Vec::new();
                            if bytes.try_reserve_exact(size).is_err() {
                                submit_failed = true;
                            } else {
                                bytes.resize(size, 0u8);
                                mem.read_physical(desc.alloc_table_gpa, &mut bytes);
                                alloc_table = Some(bytes);
                            }
                        }
                    }

                    if cmd_stream_ok && !submit_failed {
                        let submit_cmd_stream = if cmd_stream.is_empty() {
                            // `decode_cmd_stream` may have only captured the header to avoid a
                            // potentially large copy. Backends require the full stream bytes, so
                            // read them now for execution.
                            let size = cmd_stream_header
                                .as_ref()
                                .map(|h| h.size_bytes)
                                .unwrap_or(desc.cmd_size_bytes)
                                .min(desc.cmd_size_bytes)
                                as usize;
                            let mut bytes = Vec::new();
                            if bytes.try_reserve_exact(size).is_err() {
                                submit_failed = true;
                                Vec::new()
                            } else {
                                bytes.resize(size, 0u8);
                                mem.read_physical(desc.cmd_gpa, &mut bytes);
                                bytes
                            }
                        } else {
                            // If we are retaining decoded submissions for debugging, avoid panicking
                            // on OOM when duplicating a potentially large stream. If the clone fails,
                            // fall back to moving the captured bytes into the submission (dropping
                            // the recorded copy).
                            let mut bytes = Vec::new();
                            if bytes.try_reserve_exact(cmd_stream.len()).is_err() {
                                core::mem::take(&mut cmd_stream)
                            } else {
                                bytes.extend_from_slice(&cmd_stream);
                                bytes
                            }
                        };

                        if !submit_failed {
                            let submit = AeroGpuBackendSubmission {
                                flags: desc.flags,
                                context_id: desc.context_id,
                                engine_id: desc.engine_id,
                                signal_fence: desc.signal_fence,
                                cmd_stream: submit_cmd_stream,
                                alloc_table,
                            };

                            if self.backend.submit(mem, submit).is_err() {
                                regs.stats.gpu_exec_errors =
                                    regs.stats.gpu_exec_errors.saturating_add(1);
                                regs.record_error(AerogpuErrorCode::Backend, desc.signal_fence);
                            }

                            if wants_present {
                                if let Some(scanout) = self.backend.read_scanout_rgba8(0) {
                                    if let Err(err) = write_scanout0_rgba8(regs, mem, &scanout) {
                                        if self.cfg.verbose {
                                            eprintln!("aerogpu: scanout writeback failed: {err}");
                                        }
                                        regs.stats.gpu_exec_errors =
                                            regs.stats.gpu_exec_errors.saturating_add(1);
                                        regs.record_error(
                                            AerogpuErrorCode::Backend,
                                            desc.signal_fence,
                                        );
                                    }
                                }
                            }
                        }
                    }

                    if submit_failed {
                        regs.stats.gpu_exec_errors = regs.stats.gpu_exec_errors.saturating_add(1);
                        regs.record_error(AerogpuErrorCode::Backend, desc.signal_fence);
                    }

                    let can_pace_vsync = vsync_present
                        && (regs.features & FEATURE_VBLANK) != 0
                        && regs.scanout0.enable;

                    let wants_irq = desc.flags & AeroGpuSubmitDesc::FLAG_NO_IRQ == 0;
                    let kind = if can_pace_vsync {
                        PendingFenceKind::Vblank
                    } else {
                        PendingFenceKind::Immediate
                    };

                    // Maintain a monotonically increasing fence schedule across queued
                    // (vsync-delayed) and immediate submissions.
                    let last_fence = self
                        .pending_fences
                        .back()
                        .map(|e| e.fence)
                        .unwrap_or(regs.completed_fence);
                    if desc.signal_fence > last_fence {
                        self.pending_fences.push_back(PendingFenceCompletion {
                            fence: desc.signal_fence,
                            wants_irq,
                            kind,
                        });
                    } else if desc.signal_fence == last_fence {
                        // Duplicate fence values can occur (e.g. Win7 KMD internal submissions
                        // reusing the most recently submitted fence). Preserve the most restrictive
                        // completion/IRQ semantics.
                        if let Some(back) = self.pending_fences.back_mut() {
                            back.wants_irq |= wants_irq;
                            if back.kind == PendingFenceKind::Immediate
                                && kind == PendingFenceKind::Vblank
                            {
                                back.kind = PendingFenceKind::Vblank;
                            }
                        }
                    }
                }
                AeroGpuFenceCompletionMode::Deferred => {
                    let mut vsync_present = false;
                    let cmd_stream_ok = desc.cmd_gpa != 0
                        && desc.cmd_size_bytes != 0
                        && cmd_stream_header.is_some()
                        && !decode_errors
                            .iter()
                            .any(|e| matches!(e, AeroGpuSubmissionDecodeError::CmdStream(_)));
                    if cmd_stream_ok {
                        vsync_present =
                            cmd_stream_has_vsync_present_bytes(&cmd_stream).unwrap_or(false);
                    }

                    let mut alloc_table = None;
                    let mut alloc_table_alloc_failed = false;
                    if desc.alloc_table_gpa != 0
                        && desc.alloc_table_size_bytes != 0
                        && alloc_table_header.is_some()
                        && !decode_errors
                            .iter()
                            .any(|e| matches!(e, AeroGpuSubmissionDecodeError::AllocTable(_)))
                    {
                        let size_bytes = alloc_table_header
                            .as_ref()
                            .map(|h| h.size_bytes)
                            .unwrap_or(0);
                        let size = size_bytes as usize;
                        if size != 0 {
                            let mut bytes = Vec::new();
                            if bytes.try_reserve_exact(size).is_err() {
                                alloc_table_alloc_failed = true;
                            } else {
                                bytes.resize(size, 0u8);
                                mem.read_physical(desc.alloc_table_gpa, &mut bytes);
                                alloc_table = Some(bytes);
                            }
                        }
                    }

                    let can_pace_vsync = vsync_present
                        && (regs.features & FEATURE_VBLANK) != 0
                        && regs.scanout0.enable;
                    let kind = if can_pace_vsync {
                        PendingFenceKind::Vblank
                    } else {
                        PendingFenceKind::Immediate
                    };

                    let submission_failed = !decode_errors.is_empty() || alloc_table_alloc_failed;
                    let mut inserted_new_fence = false;
                    if desc.signal_fence > regs.completed_fence {
                        let already_completed =
                            self.completed_before_submit.remove(&desc.signal_fence);
                        let incoming = InFlightSubmission {
                            desc: desc.clone(),
                            kind,
                            completed_backend: already_completed,
                            vblank_ready: kind == PendingFenceKind::Immediate,
                        };

                        match self.in_flight.entry(desc.signal_fence) {
                            Entry::Vacant(v) => {
                                v.insert(incoming);
                                inserted_new_fence = true;
                            }
                            Entry::Occupied(mut o) => {
                                // Some guest drivers may reuse fence values with NO_IRQ set for
                                // best-effort internal submissions. Avoid clobbering the original
                                // submission's metadata (PRESENT, wants_irq) by merging entries.
                                o.get_mut().merge(incoming);
                            }
                        }

                        // If the backend completed the fence before we processed the corresponding
                        // ring entry, we may be able to advance immediately.
                        if already_completed
                            && matches!(
                                self.in_flight.get(&desc.signal_fence).map(|e| e.kind),
                                Some(PendingFenceKind::Immediate)
                            )
                        {
                            self.advance_completed_fence(regs, mem);
                        }
                    }

                    // Malformed submissions must not deadlock the guest: treat them as failed and
                    // complete the fence immediately (deferred mode) rather than waiting for any
                    // backend/external executor. This mirrors the protocol contract and matches the
                    // canonical machine behavior.
                    //
                    // Note: only auto-complete on the first entry for a given fence. Duplicate
                    // fence values may accompany real work that must still wait for completion.
                    if submission_failed && inserted_new_fence {
                        // If we are failing the submission (malformed decode, or host could not
                        // stage required buffers), do not hold the fence behind a vblank gate.
                        if let Some(entry) = self.in_flight.get_mut(&desc.signal_fence) {
                            entry.vblank_ready = true;
                        }
                        self.complete_fence(regs, mem, desc.signal_fence);
                    }

                    if alloc_table_alloc_failed {
                        regs.stats.gpu_exec_errors = regs.stats.gpu_exec_errors.saturating_add(1);
                        regs.record_error(AerogpuErrorCode::Backend, desc.signal_fence);
                    }

                    // Do not forward malformed submissions into backends: they cannot be executed
                    // reliably and have already been treated as failed above.
                    if !submission_failed {
                        // Avoid cloning potentially large command streams unless we are also retaining
                        // a submission record for debugging (`keep_last_submissions`).
                        let submit_cmd_stream = if self.cfg.keep_last_submissions > 0 {
                            // Avoid panicking on OOM when duplicating a potentially large command
                            // stream. If the clone fails, move the captured bytes into the submission
                            // (dropping the recorded copy).
                            let mut bytes = Vec::new();
                            if bytes.try_reserve_exact(cmd_stream.len()).is_err() {
                                core::mem::take(&mut cmd_stream)
                            } else {
                                bytes.extend_from_slice(&cmd_stream);
                                bytes
                            }
                        } else {
                            core::mem::take(&mut cmd_stream)
                        };

                        let submit = AeroGpuBackendSubmission {
                            flags: desc.flags,
                            context_id: desc.context_id,
                            engine_id: desc.engine_id,
                            signal_fence: desc.signal_fence,
                            cmd_stream: submit_cmd_stream,
                            alloc_table,
                        };

                        if self.backend_configured {
                            if self.backend.submit(mem, submit).is_err() {
                                regs.stats.gpu_exec_errors =
                                    regs.stats.gpu_exec_errors.saturating_add(1);
                                regs.record_error(AerogpuErrorCode::Backend, desc.signal_fence);
                                // If the backend rejects the submission, still unblock the fence.
                                if let Some(entry) = self.in_flight.get_mut(&desc.signal_fence) {
                                    entry.vblank_ready = true;
                                }
                                self.complete_fence(regs, mem, desc.signal_fence);
                            }
                        } else {
                            // No in-process backend: surface the decoded submission to the caller
                            // (WASM bridge) so it can be executed externally and later completed via
                            // `complete_fence`.
                            //
                            // Some guest drivers may emit submissions with duplicate fences (including
                            // fence values that have already completed) for best-effort internal work.
                            // Even if such submissions do not advance the completed fence, they can
                            // carry important side effects (e.g. shared-surface release), so always
                            // queue them for external execution.
                            self.push_pending_submission(regs, mem, submit);
                        }
                    }
                }
            }

            if self.cfg.keep_last_submissions > 0 {
                if self.last_submissions.len() == self.cfg.keep_last_submissions {
                    self.last_submissions.pop_front();
                }
                self.last_submissions.push_back(AeroGpuSubmissionRecord {
                    ring_head: head,
                    ring_tail: tail,
                    submission: AeroGpuSubmission {
                        desc: desc.clone(),
                        alloc_table_header,
                        allocs,
                        cmd_stream_header,
                        cmd_stream,
                    },
                    decode_errors,
                });
            }

            if self.cfg.verbose {
                eprintln!(
                    "aerogpu: submit head={} tail={} fence={} flags=0x{:x} cmd_gpa=0x{:x} cmd_size={} allocs={} cmd_stream_size={} decode_errors={}",
                    head,
                    tail,
                    desc.signal_fence,
                    desc.flags,
                    desc.cmd_gpa,
                    desc.cmd_size_bytes,
                    alloc_count,
                    cmd_header_size,
                    decode_error_count,
                );
            }

            head = head.wrapping_add(1);
            processed += 1;
            AeroGpuRingHeader::write_head(mem, regs.ring_gpa, head);
        }

        if self.cfg.fence_completion == AeroGpuFenceCompletionMode::Immediate {
            // Complete any immediate fences that are not blocked behind a vsync fence.
            self.complete_immediate_fences(regs, mem);
            // Drain backend completions for error reporting and to avoid unbounded queueing in
            // synchronous backends. Fence advancement is handled separately in Immediate mode.
            self.poll_backend_completions(regs, mem);
        } else {
            self.poll_backend_completions(regs, mem);
        }
    }

    fn complete_immediate_fences(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        let mut to_complete = Vec::new();
        while matches!(
            self.pending_fences.front().map(|e| e.kind),
            Some(PendingFenceKind::Immediate)
        ) {
            to_complete.push(self.pending_fences.pop_front().unwrap());
        }
        self.complete_fences(regs, mem, to_complete);
    }

    fn complete_fences(
        &mut self,
        regs: &mut AeroGpuRegs,
        mem: &mut dyn MemoryBus,
        entries: Vec<PendingFenceCompletion>,
    ) {
        if entries.is_empty() {
            return;
        }

        let mut advanced = false;
        let mut wants_irq = false;
        for entry in entries {
            if entry.fence > regs.completed_fence {
                regs.completed_fence = entry.fence;
                advanced = true;
                wants_irq |= entry.wants_irq;
            }
        }

        if advanced {
            self.write_fence_page(regs, mem);
            self.maybe_raise_fence_irq(regs, wants_irq);
        }
    }

    fn write_fence_page(&self, regs: &AeroGpuRegs, mem: &mut dyn MemoryBus) {
        // Always make MMIO-completed-fence observable; the value is read from regs.completed_fence.
        if regs.fence_gpa == 0 {
            return;
        }

        crate::ring::write_fence_page(mem, regs.fence_gpa, regs.abi_version, regs.completed_fence);
    }

    fn maybe_raise_fence_irq(&self, regs: &mut AeroGpuRegs, wants_irq: bool) {
        if !wants_irq {
            return;
        }

        // Avoid latching "stale" interrupts: only set the status bit while unmasked.
        if (regs.irq_enable & irq_bits::FENCE) != 0 {
            regs.irq_status |= irq_bits::FENCE;
        }
    }

    pub(crate) fn save_snapshot_state(&self) -> Vec<u8> {
        let mut enc = Encoder::new();

        enc = enc.u32(self.pending_fences.len() as u32);
        for entry in &self.pending_fences {
            let kind = match entry.kind {
                PendingFenceKind::Immediate => 0u8,
                PendingFenceKind::Vblank => 1u8,
            };
            enc = enc.u64(entry.fence).bool(entry.wants_irq).u8(kind);
        }

        enc = enc.u32(self.in_flight.len() as u32);
        for entry in self.in_flight.values() {
            let desc = &entry.desc;
            let kind = match entry.kind {
                PendingFenceKind::Immediate => 0u8,
                PendingFenceKind::Vblank => 1u8,
            };
            enc = enc
                .u32(desc.desc_size_bytes)
                .u32(desc.flags)
                .u32(desc.context_id)
                .u32(desc.engine_id)
                .u64(desc.cmd_gpa)
                .u32(desc.cmd_size_bytes)
                .u64(desc.alloc_table_gpa)
                .u32(desc.alloc_table_size_bytes)
                .u64(desc.signal_fence)
                .u8(kind)
                .bool(entry.completed_backend)
                .bool(entry.vblank_ready);
        }

        // `HashSet` iteration order is nondeterministic; sort for canonical encoding.
        let mut completed: Vec<u64> = self.completed_before_submit.iter().copied().collect();
        completed.sort_unstable();
        enc = enc.u32(completed.len() as u32);
        for fence in completed {
            enc = enc.u64(fence);
        }

        enc.finish()
    }

    pub(crate) fn load_snapshot_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        // These structures are guest-controlled (via ring submissions) and can grow without
        // bound. Snapshots may come from untrusted sources, so cap sizes to keep decode bounded.
        const MAX_PENDING_FENCES: usize = 65_536;
        const MAX_IN_FLIGHT: usize = 65_536;
        const MAX_COMPLETED_BEFORE_SUBMIT: usize = 65_536;

        let mut d = Decoder::new(bytes);

        let pending_count = d.u32()? as usize;
        if pending_count > MAX_PENDING_FENCES {
            return Err(SnapshotError::InvalidFieldEncoding("pending_fences"));
        }
        let mut pending_fences = VecDeque::new();
        pending_fences
            .try_reserve_exact(pending_count)
            .map_err(|_| SnapshotError::OutOfMemory)?;
        for _ in 0..pending_count {
            let fence = d.u64()?;
            let wants_irq = d.bool()?;
            let kind = match d.u8()? {
                0 => PendingFenceKind::Immediate,
                1 => PendingFenceKind::Vblank,
                _ => return Err(SnapshotError::InvalidFieldEncoding("pending_fences.kind")),
            };
            pending_fences.push_back(PendingFenceCompletion {
                fence,
                wants_irq,
                kind,
            });
        }

        let in_flight_count = d.u32()? as usize;
        if in_flight_count > MAX_IN_FLIGHT {
            return Err(SnapshotError::InvalidFieldEncoding("in_flight"));
        }
        let mut in_flight = BTreeMap::new();
        for _ in 0..in_flight_count {
            let desc = AeroGpuSubmitDesc {
                desc_size_bytes: d.u32()?,
                flags: d.u32()?,
                context_id: d.u32()?,
                engine_id: d.u32()?,
                cmd_gpa: d.u64()?,
                cmd_size_bytes: d.u32()?,
                alloc_table_gpa: d.u64()?,
                alloc_table_size_bytes: d.u32()?,
                signal_fence: d.u64()?,
            };
            let kind = match d.u8()? {
                0 => PendingFenceKind::Immediate,
                1 => PendingFenceKind::Vblank,
                _ => return Err(SnapshotError::InvalidFieldEncoding("in_flight.kind")),
            };
            let completed_backend = d.bool()?;
            let vblank_ready = d.bool()?;

            if in_flight
                .insert(
                    desc.signal_fence,
                    InFlightSubmission {
                        desc,
                        kind,
                        completed_backend,
                        vblank_ready,
                    },
                )
                .is_some()
            {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "in_flight.duplicate_fence",
                ));
            }
        }

        let completed_count = d.u32()? as usize;
        if completed_count > MAX_COMPLETED_BEFORE_SUBMIT {
            return Err(SnapshotError::InvalidFieldEncoding(
                "completed_before_submit",
            ));
        }
        let mut completed_before_submit = HashSet::new();
        completed_before_submit
            .try_reserve(completed_count)
            .map_err(|_| SnapshotError::OutOfMemory)?;
        for _ in 0..completed_count {
            completed_before_submit.insert(d.u64()?);
        }

        d.finish()?;

        // Apply decoded state.
        self.last_submissions.clear();
        self.pending_fences = pending_fences;
        self.in_flight = in_flight;
        self.completed_before_submit = completed_before_submit;

        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Defensive caps (guest-driven scanout writeback)
// -----------------------------------------------------------------------------
//
// If the host backend supplies a presented scanout, the executor can write it back into guest
// memory (SCANOUT0_FB_GPA). The guest controls the scanout register values, so we must avoid
// allocating unbounded intermediate buffers or attempting writes that wrap the physical address
// space.
//
// Keep a tighter cap on wasm32 where heaps are more constrained.
#[cfg(target_arch = "wasm32")]
const MAX_SCANOUT_WRITEBACK_BYTES: usize = 32 * 1024 * 1024;
#[cfg(not(target_arch = "wasm32"))]
const MAX_SCANOUT_WRITEBACK_BYTES: usize = 64 * 1024 * 1024;

fn write_scanout0_rgba8(
    regs: &AeroGpuRegs,
    mem: &mut dyn MemoryBus,
    scanout: &AeroGpuBackendScanout,
) -> Result<(), String> {
    if !regs.scanout0.enable {
        return Ok(());
    }

    let dst_width = regs.scanout0.width as usize;
    let dst_height = regs.scanout0.height as usize;
    if dst_width == 0 || dst_height == 0 {
        return Ok(());
    }
    if regs.scanout0.fb_gpa == 0 {
        return Err("scanout0.fb_gpa is not set".into());
    }

    let src_width = scanout.width as usize;
    let src_height = scanout.height as usize;
    if src_width == 0 || src_height == 0 {
        return Ok(());
    }

    let copy_width = dst_width.min(src_width);
    let copy_height = dst_height.min(src_height);
    if copy_width == 0 || copy_height == 0 {
        return Ok(());
    }

    let dst_bpp = regs
        .scanout0
        .format
        .bytes_per_pixel()
        .ok_or_else(|| format!("unsupported scanout format {:?}", regs.scanout0.format))?;
    let pitch = regs.scanout0.pitch_bytes as usize;
    let row_bytes = copy_width
        .checked_mul(dst_bpp)
        .ok_or_else(|| "scanout row bytes overflow".to_string())?;
    if pitch < row_bytes {
        return Err(format!(
            "scanout pitch_bytes {pitch} is smaller than row_bytes {row_bytes}"
        ));
    }

    // Cap total bytes written and validate GPA arithmetic does not wrap.
    let total_write_bytes = row_bytes
        .checked_mul(copy_height)
        .ok_or_else(|| "scanout size overflow".to_string())?;
    if total_write_bytes > MAX_SCANOUT_WRITEBACK_BYTES {
        return Err(format!(
            "scanout writeback too large: {copy_width}x{copy_height} @ {dst_bpp}Bpp = {total_write_bytes} bytes (cap {MAX_SCANOUT_WRITEBACK_BYTES})"
        ));
    }

    let pitch_u64 = u64::from(regs.scanout0.pitch_bytes);
    let row_bytes_u64 =
        u64::try_from(row_bytes).map_err(|_| "scanout row bytes overflow".to_string())?;
    let last_row_gpa = regs
        .scanout0
        .fb_gpa
        .checked_add(
            (copy_height as u64)
                .checked_sub(1)
                .and_then(|rows| rows.checked_mul(pitch_u64))
                .ok_or_else(|| "scanout address overflow".to_string())?,
        )
        .ok_or_else(|| "scanout address overflow".to_string())?;
    last_row_gpa
        .checked_add(row_bytes_u64)
        .ok_or_else(|| "scanout address overflow".to_string())?;

    // Ensure the backend scanout buffer is large enough for the portion we will read.
    let required_src_len = src_width
        .checked_mul(copy_height)
        .and_then(|v| v.checked_mul(4))
        .ok_or_else(|| "scanout dimensions overflow".to_string())?;
    if scanout.rgba8.len() < required_src_len {
        return Err(format!(
            "scanout rgba8 buffer too small: need {required_src_len} bytes for {src_width}x{copy_height}, have {}",
            scanout.rgba8.len()
        ));
    }

    let mut row_buf = Vec::new();
    row_buf
        .try_reserve_exact(row_bytes)
        .map_err(|_| "scanout writeback allocation failed".to_string())?;
    row_buf.resize(row_bytes, 0u8);
    for y in 0..copy_height {
        let src_row_start = y * src_width * 4;
        let src_row = &scanout.rgba8[src_row_start..src_row_start + src_width * 4];

        match regs.scanout0.format {
            AeroGpuFormat::B8G8R8A8Unorm | AeroGpuFormat::B8G8R8A8UnormSrgb => {
                for x in 0..copy_width {
                    let src = &src_row[x * 4..x * 4 + 4];
                    let dst = &mut row_buf[x * 4..x * 4 + 4];
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                    dst[3] = src[3];
                }
            }
            AeroGpuFormat::B8G8R8X8Unorm | AeroGpuFormat::B8G8R8X8UnormSrgb => {
                for x in 0..copy_width {
                    let src = &src_row[x * 4..x * 4 + 4];
                    let dst = &mut row_buf[x * 4..x * 4 + 4];
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                    dst[3] = 0xff;
                }
            }
            AeroGpuFormat::R8G8B8A8Unorm | AeroGpuFormat::R8G8B8A8UnormSrgb => {
                row_buf[..copy_width * 4].copy_from_slice(&src_row[..copy_width * 4]);
            }
            AeroGpuFormat::R8G8B8X8Unorm | AeroGpuFormat::R8G8B8X8UnormSrgb => {
                for x in 0..copy_width {
                    let src = &src_row[x * 4..x * 4 + 4];
                    let dst = &mut row_buf[x * 4..x * 4 + 4];
                    dst[0] = src[0];
                    dst[1] = src[1];
                    dst[2] = src[2];
                    dst[3] = 0xff;
                }
            }
            AeroGpuFormat::B5G6R5Unorm => {
                for x in 0..copy_width {
                    let src = &src_row[x * 4..x * 4 + 4];
                    let r = (src[0] >> 3) as u16;
                    let g = (src[1] >> 2) as u16;
                    let b = (src[2] >> 3) as u16;
                    let pix = (r << 11) | (g << 5) | b;
                    let dst = &mut row_buf[x * 2..x * 2 + 2];
                    dst.copy_from_slice(&pix.to_le_bytes());
                }
            }
            AeroGpuFormat::B5G5R5A1Unorm => {
                for x in 0..copy_width {
                    let src = &src_row[x * 4..x * 4 + 4];
                    let r = (src[0] >> 3) as u16;
                    let g = (src[1] >> 3) as u16;
                    let b = (src[2] >> 3) as u16;
                    let a = if src[3] >= 0x80 { 1u16 } else { 0u16 };
                    let pix = (a << 15) | (r << 10) | (g << 5) | b;
                    let dst = &mut row_buf[x * 2..x * 2 + 2];
                    dst.copy_from_slice(&pix.to_le_bytes());
                }
            }
            _ => {
                return Err(format!(
                    "unsupported scanout format {:?}",
                    regs.scanout0.format
                ));
            }
        }

        let row_gpa = regs.scanout0.fb_gpa + (y as u64) * pitch_u64;
        mem.write_physical(row_gpa, &row_buf);
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Defensive caps (guest-driven allocations)
// -----------------------------------------------------------------------------
//
// The ring submission format allows the guest to provide pointers to:
// - an optional allocation table (list of referenced GPU resources), and
// - a command stream.
//
// Device-side decoding will copy these buffers into host-owned `Vec<u8>` values. Since those sizes
// are guest-controlled, cap them defensively to avoid unbounded allocations.
//
// Keep tighter limits on wasm32: browser/test environments can have smaller heaps, and very large
// command streams are not expected in that configuration.
#[cfg(target_arch = "wasm32")]
const MAX_ALLOC_TABLE_SIZE_BYTES: u32 = 4 * 1024 * 1024;
#[cfg(not(target_arch = "wasm32"))]
const MAX_ALLOC_TABLE_SIZE_BYTES: u32 = 16 * 1024 * 1024;

#[cfg(target_arch = "wasm32")]
const MAX_CMD_STREAM_SIZE_BYTES: u32 = 16 * 1024 * 1024;
#[cfg(not(target_arch = "wasm32"))]
const MAX_CMD_STREAM_SIZE_BYTES: u32 = 64 * 1024 * 1024;

// -----------------------------------------------------------------------------
// Defensive caps (external-backend submission queue)
// -----------------------------------------------------------------------------
//
// When the executor is in Deferred mode and no in-process backend has been configured, decoded
// submissions are queued in `pending_submissions` for an external executor (e.g. the browser GPU
// worker) to execute. Those submissions include guest-provided command streams and optional
// allocation tables, so the queue must be bounded to avoid unbounded host allocations.
//
// If the queue overflows, we drop the oldest submissions and treat their fences as failed
// (backend error) so the guest cannot deadlock waiting on work that will never execute.
const MAX_PENDING_SUBMISSIONS: usize = 256;
// Total memory cap (in bytes) for queued `AeroGpuBackendSubmission` payloads.
//
// Keep a tighter limit under `cfg(test)` so unit tests can exercise byte-based overflow handling
// without allocating large buffers.
#[cfg(test)]
const MAX_PENDING_SUBMISSIONS_TOTAL_BYTES: usize = 4 * 1024;
#[cfg(not(test))]
const MAX_PENDING_SUBMISSIONS_TOTAL_BYTES: usize = 128 * 1024 * 1024; // 128MiB

fn decode_alloc_table(
    mem: &mut dyn MemoryBus,
    device_abi_version: u32,
    desc: &AeroGpuSubmitDesc,
    decode_errors: &mut Vec<AeroGpuSubmissionDecodeError>,
) -> (Option<AeroGpuAllocTableHeader>, Vec<AeroGpuAllocEntry>) {
    if desc.alloc_table_gpa == 0 && desc.alloc_table_size_bytes == 0 {
        return (None, Vec::new());
    }
    if desc.alloc_table_gpa == 0 || desc.alloc_table_size_bytes == 0 {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::InconsistentDescriptor,
        ));
        return (None, Vec::new());
    }
    if desc
        .alloc_table_gpa
        .checked_add(u64::from(desc.alloc_table_size_bytes))
        .is_none()
    {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::AddressOverflow,
        ));
        return (None, Vec::new());
    }

    if desc.alloc_table_size_bytes < AeroGpuAllocTableHeader::SIZE_BYTES {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::SizeTooSmall,
        ));
        return (None, Vec::new());
    }
    if desc
        .alloc_table_gpa
        .checked_add(u64::from(AeroGpuAllocTableHeader::SIZE_BYTES))
        .is_none()
        || desc
            .alloc_table_gpa
            .checked_add(u64::from(desc.alloc_table_size_bytes))
            .is_none()
    {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::AddressOverflow,
        ));
        return (None, Vec::new());
    }

    let header = AeroGpuAllocTableHeader::read_from(mem, desc.alloc_table_gpa);

    if header.magic != AEROGPU_ALLOC_TABLE_MAGIC {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::BadMagic,
        ));
        return (Some(header), Vec::new());
    }
    if (header.abi_version >> 16) != (device_abi_version >> 16) {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::BadAbiVersion,
        ));
        return (Some(header), Vec::new());
    }
    if header.size_bytes < AeroGpuAllocTableHeader::SIZE_BYTES {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::SizeTooSmall,
        ));
        return (Some(header), Vec::new());
    }
    if header.size_bytes > desc.alloc_table_size_bytes {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::SizeExceedsDescriptor,
        ));
        return (Some(header), Vec::new());
    }
    if header.size_bytes > MAX_ALLOC_TABLE_SIZE_BYTES {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::TooLarge,
        ));
        return (Some(header), Vec::new());
    }
    // Forward-compat: newer guests may extend `aerogpu_alloc_entry` by increasing the declared
    // stride. We only require the entry prefix we understand.
    if header.entry_stride_bytes < AeroGpuAllocEntry::SIZE_BYTES {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::BadEntryStride,
        ));
        return (Some(header), Vec::new());
    }

    // Confirm the table contains the declared entries before walking them.
    if header.validate_prefix().is_err() {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::EntriesOutOfBounds,
        ));
        return (Some(header), Vec::new());
    }

    let mut allocs = Vec::new();
    let Ok(entry_count) = usize::try_from(header.entry_count) else {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::AddressOverflow,
        ));
        return (Some(header), Vec::new());
    };
    if allocs.try_reserve_exact(entry_count).is_err() {
        decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
            AeroGpuAllocTableDecodeError::TooLarge,
        ));
        return (Some(header), Vec::new());
    }

    let mut seen = HashSet::new();
    for idx in 0..header.entry_count {
        let Some(entry_offset) = u64::from(idx).checked_mul(u64::from(header.entry_stride_bytes))
        else {
            decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::AddressOverflow,
            ));
            break;
        };
        let Some(offset) = u64::from(AeroGpuAllocTableHeader::SIZE_BYTES).checked_add(entry_offset)
        else {
            decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::AddressOverflow,
            ));
            break;
        };
        let Some(entry_gpa) = desc.alloc_table_gpa.checked_add(offset) else {
            decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::AddressOverflow,
            ));
            break;
        };

        let entry = AeroGpuAllocEntry::read_from(mem, entry_gpa);
        if entry.alloc_id == 0 || entry.size_bytes == 0 {
            decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::InvalidEntry,
            ));
            break;
        }
        if entry.gpa.checked_add(entry.size_bytes).is_none() {
            decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::AddressOverflow,
            ));
            break;
        }
        if !seen.insert(entry.alloc_id) {
            decode_errors.push(AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::DuplicateAllocId,
            ));
            break;
        }
        allocs.push(entry);
    }

    (Some(header), allocs)
}

fn decode_cmd_stream(
    mem: &mut dyn MemoryBus,
    _device_abi_version: u32,
    desc: &AeroGpuSubmitDesc,
    decode_errors: &mut Vec<AeroGpuSubmissionDecodeError>,
    capture_bytes: bool,
) -> (Option<AeroGpuCmdStreamHeader>, Vec<u8>) {
    if desc.cmd_gpa == 0 && desc.cmd_size_bytes == 0 {
        return (None, Vec::new());
    }
    if desc.cmd_gpa == 0 || desc.cmd_size_bytes == 0 {
        decode_errors.push(AeroGpuSubmissionDecodeError::CmdStream(
            AeroGpuCmdStreamDecodeError::InconsistentDescriptor,
        ));
        return (None, Vec::new());
    }

    if desc
        .cmd_gpa
        .checked_add(u64::from(desc.cmd_size_bytes))
        .is_none()
    {
        decode_errors.push(AeroGpuSubmissionDecodeError::CmdStream(
            AeroGpuCmdStreamDecodeError::AddressOverflow,
        ));
        return (None, Vec::new());
    }

    // Forward-compat: `cmd_size_bytes` is the buffer capacity, while the command stream header's
    // `size_bytes` is the number of bytes used by the stream. Guests may provide a backing buffer
    // that is larger than `cmd_stream_header.size_bytes` (page rounding / reuse); only copy the
    // used prefix.
    if desc.cmd_size_bytes < ProtocolCmdStreamHeader::SIZE_BYTES as u32 {
        decode_errors.push(AeroGpuSubmissionDecodeError::CmdStream(
            AeroGpuCmdStreamDecodeError::TooSmall,
        ));
        if !capture_bytes {
            return (None, Vec::new());
        }
        let cmd_size = desc.cmd_size_bytes as usize;
        let mut cmd_stream = vec![0u8; cmd_size];
        mem.read_physical(desc.cmd_gpa, &mut cmd_stream);
        return (None, cmd_stream);
    }

    let mut prefix = [0u8; ProtocolCmdStreamHeader::SIZE_BYTES];
    mem.read_physical(desc.cmd_gpa, &mut prefix);
    let header = match decode_cmd_stream_header_le(&prefix) {
        Ok(header) => AeroGpuCmdStreamHeader::from(header),
        Err(_) => {
            decode_errors.push(AeroGpuSubmissionDecodeError::CmdStream(
                AeroGpuCmdStreamDecodeError::BadHeader,
            ));
            if capture_bytes {
                return (None, prefix.to_vec());
            }
            return (None, Vec::new());
        }
    };

    if header.size_bytes > desc.cmd_size_bytes {
        decode_errors.push(AeroGpuSubmissionDecodeError::CmdStream(
            AeroGpuCmdStreamDecodeError::StreamSizeTooLarge,
        ));
        if capture_bytes {
            return (Some(header), prefix.to_vec());
        }
        return (Some(header), Vec::new());
    }

    if header.size_bytes > MAX_CMD_STREAM_SIZE_BYTES {
        decode_errors.push(AeroGpuSubmissionDecodeError::CmdStream(
            AeroGpuCmdStreamDecodeError::TooLarge,
        ));
        if capture_bytes {
            return (Some(header), prefix.to_vec());
        }
        return (Some(header), Vec::new());
    }

    if !capture_bytes {
        return (Some(header), Vec::new());
    }

    let cmd_size = header.size_bytes as usize;
    let mut cmd_stream = Vec::new();
    if cmd_stream.try_reserve_exact(cmd_size).is_err() {
        decode_errors.push(AeroGpuSubmissionDecodeError::CmdStream(
            AeroGpuCmdStreamDecodeError::TooLarge,
        ));
        return (Some(header), Vec::new());
    }
    cmd_stream.resize(cmd_size, 0u8);
    mem.read_physical(desc.cmd_gpa, &mut cmd_stream);

    (Some(header), cmd_stream)
}

#[derive(Clone, Debug)]
struct InFlightSubmission {
    desc: AeroGpuSubmitDesc,
    kind: PendingFenceKind,
    completed_backend: bool,
    vblank_ready: bool,
}

impl InFlightSubmission {
    fn merge(&mut self, other: InFlightSubmission) {
        // Preserve forward-compat bits with OR semantics, but treat NO_IRQ specially: a fence
        // should raise an IRQ if *any* submission with that fence wants one.
        let no_irq = (self.desc.flags & other.desc.flags) & AeroGpuSubmitDesc::FLAG_NO_IRQ;
        let other_bits = (self.desc.flags | other.desc.flags) & !AeroGpuSubmitDesc::FLAG_NO_IRQ;
        self.desc.flags = other_bits | no_irq;

        // Prefer the more restrictive completion kind: if any submission wants vblank pacing,
        // the fence must remain gated on vblank.
        let new_kind =
            if self.kind == PendingFenceKind::Vblank || other.kind == PendingFenceKind::Vblank {
                PendingFenceKind::Vblank
            } else {
                PendingFenceKind::Immediate
            };

        if self.kind == PendingFenceKind::Immediate && new_kind == PendingFenceKind::Vblank {
            // Upgrading from immediate to vblank-gated: force the fence to wait for the next vblank
            // tick (even if the immediate entry previously treated it as ready).
            self.vblank_ready = false;
        }
        self.kind = new_kind;

        self.completed_backend |= other.completed_backend;

        if self.kind == PendingFenceKind::Immediate {
            self.vblank_ready = true;
        }
    }

    fn is_ready(&self) -> bool {
        match self.kind {
            PendingFenceKind::Immediate => self.completed_backend,
            PendingFenceKind::Vblank => self.completed_backend && self.vblank_ready,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ring::{
        AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC, RING_HEAD_OFFSET, RING_TAIL_OFFSET,
    };
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuCmdOpcode, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_PRESENT_FLAG_VSYNC,
    };
    use memory::{Bus, MemoryBus};

    fn write_ring_header(
        mem: &mut dyn MemoryBus,
        gpa: u64,
        entry_count: u32,
        head: u32,
        tail: u32,
    ) {
        let stride = AeroGpuSubmitDesc::SIZE_BYTES;
        let size_bytes = u32::try_from(
            AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * u64::from(stride),
        )
        .unwrap();

        mem.write_u32(gpa, AEROGPU_RING_MAGIC);
        mem.write_u32(gpa + 4, AeroGpuRegs::default().abi_version);
        mem.write_u32(gpa + 8, size_bytes);
        mem.write_u32(gpa + 12, entry_count);
        mem.write_u32(gpa + 16, stride);
        mem.write_u32(gpa + 20, 0);
        mem.write_u32(gpa + RING_HEAD_OFFSET, head);
        mem.write_u32(gpa + RING_TAIL_OFFSET, tail);
    }

    fn write_submit_desc(mem: &mut dyn MemoryBus, gpa: u64, fence: u64, flags: u32) {
        mem.write_u32(gpa, AeroGpuSubmitDesc::SIZE_BYTES);
        mem.write_u32(gpa + 4, flags);
        mem.write_u32(gpa + 8, 0);
        mem.write_u32(gpa + 12, 0);
        mem.write_u64(gpa + 16, 0);
        mem.write_u32(gpa + 24, 0);
        mem.write_u64(gpa + 32, 0);
        mem.write_u32(gpa + 40, 0);
        mem.write_u64(gpa + 48, fence);
    }

    fn write_submit_desc_with_cmd(
        mem: &mut dyn MemoryBus,
        gpa: u64,
        fence: u64,
        flags: u32,
        cmd_gpa: u64,
        cmd_size_bytes: u32,
    ) {
        mem.write_u32(gpa, AeroGpuSubmitDesc::SIZE_BYTES);
        mem.write_u32(gpa + 4, flags);
        mem.write_u32(gpa + 8, 0);
        mem.write_u32(gpa + 12, 0);
        mem.write_u64(gpa + 16, cmd_gpa);
        mem.write_u32(gpa + 24, cmd_size_bytes);
        mem.write_u64(gpa + 32, 0);
        mem.write_u32(gpa + 40, 0);
        mem.write_u64(gpa + 48, fence);
    }

    fn write_vsync_present_cmd_stream(mem: &mut dyn MemoryBus, gpa: u64) -> u32 {
        // Minimal command stream:
        // - stream header (24 bytes)
        // - PRESENT packet (16 bytes): cmd hdr (8) + payload {scanout_id:u32, flags:u32} (8)
        let size_bytes = 40u32;

        mem.write_u32(gpa, AEROGPU_CMD_STREAM_MAGIC);
        mem.write_u32(gpa + 4, AeroGpuRegs::default().abi_version);
        mem.write_u32(gpa + 8, size_bytes);
        mem.write_u32(gpa + 12, 0);
        mem.write_u32(gpa + 16, 0);
        mem.write_u32(gpa + 20, 0);

        // Packet header.
        mem.write_u32(gpa + 24, AerogpuCmdOpcode::Present as u32);
        mem.write_u32(gpa + 28, 16);

        // Payload (scanout_id, flags).
        mem.write_u32(gpa + 32, 0);
        mem.write_u32(gpa + 36, AEROGPU_PRESENT_FLAG_VSYNC);

        size_bytes
    }

    fn write_invalid_cmd_stream_header(mem: &mut dyn MemoryBus, gpa: u64) -> u32 {
        let size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32;
        // Wrong magic triggers `AeroGpuCmdStreamDecodeError::BadHeader`.
        mem.write_u32(gpa, AEROGPU_CMD_STREAM_MAGIC ^ 1);
        mem.write_u32(gpa + 4, AeroGpuRegs::default().abi_version);
        mem.write_u32(gpa + 8, size_bytes);
        mem.write_u32(gpa + 12, 0);
        mem.write_u32(gpa + 16, 0);
        mem.write_u32(gpa + 20, 0);
        size_bytes
    }

    #[test]
    fn duplicate_fence_entries_preserve_irq_request() {
        let mut mem = Bus::new(0x4000);
        let ring_gpa = 0x1000u64;

        let entry_count = 8u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, 2);

        let fence = 7u64;

        // Two descriptors reuse the same fence:
        // - First wants an IRQ (normal submission).
        // - Second suppresses IRQ delivery (Win7 KMD internal submission behavior).
        let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
        write_submit_desc(&mut mem, desc0_gpa, fence, 0);
        let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + stride;
        write_submit_desc(&mut mem, desc1_gpa, fence, AeroGpuSubmitDesc::FLAG_NO_IRQ);

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();

        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            irq_enable: irq_bits::FENCE,
            ..Default::default()
        };

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        });

        exec.process_doorbell(&mut regs, &mut mem);
        assert_eq!(regs.completed_fence, 0);
        assert_eq!(regs.irq_status & irq_bits::FENCE, 0);

        exec.complete_fence(&mut regs, &mut mem, fence);
        assert_eq!(regs.completed_fence, fence);
        assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
    }

    #[test]
    fn deferred_mode_drains_submission_even_if_fence_already_completed() {
        let mut mem = Bus::new(0x8000);
        let ring_gpa = 0x1000u64;
        let cmd_gpa = 0x3000u64;

        let entry_count = 8u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, 1);

        // Simulate a driver emitting a submission that reuses an already-completed fence (e.g. a
        // best-effort internal submission). Even though this does not advance the fence, the host
        // still needs to execute it for side effects.
        let fence = 7u64;

        let cmd_size_bytes = write_vsync_present_cmd_stream(&mut mem, cmd_gpa);
        let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
        write_submit_desc_with_cmd(
            &mut mem,
            desc_gpa,
            fence,
            AeroGpuSubmitDesc::FLAG_NO_IRQ,
            cmd_gpa,
            cmd_size_bytes,
        );

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();

        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            completed_fence: fence,
            ..Default::default()
        };

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        });

        exec.process_doorbell(&mut regs, &mut mem);
        assert_eq!(regs.completed_fence, fence);

        let drained = exec.drain_pending_submissions();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].signal_fence, fence);
        assert_eq!(
            drained[0].cmd_stream.get(0..4),
            Some(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes()[..])
        );
    }

    #[test]
    fn deferred_mode_auto_completes_malformed_submissions() {
        let mut mem = Bus::new(0x8000);
        let ring_gpa = 0x1000u64;
        let cmd_gpa = 0x3000u64;

        let entry_count = 8u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, 1);

        let fence = 7u64;
        let cmd_size_bytes = write_invalid_cmd_stream_header(&mut mem, cmd_gpa);
        let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
        write_submit_desc_with_cmd(&mut mem, desc_gpa, fence, 0, cmd_gpa, cmd_size_bytes);

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();

        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            irq_enable: irq_bits::FENCE | irq_bits::ERROR,
            ..Default::default()
        };

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        });

        exec.process_doorbell(&mut regs, &mut mem);

        // Malformed submission should not wedge deferred mode: fence is completed immediately.
        assert_eq!(regs.completed_fence, fence);
        assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
        assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

        // Failed submissions are not forwarded for external execution.
        assert!(exec.drain_pending_submissions().is_empty());
    }

    #[test]
    fn deferred_mode_pending_submission_queue_is_bounded() {
        let mut mem = Bus::new(0x20000);
        let ring_gpa = 0x1000u64;

        let entry_count = 512u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);

        let tail = (MAX_PENDING_SUBMISSIONS as u32) + 1;
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, tail);

        for i in 0..tail {
            let fence = u64::from(i) + 1;
            let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(i) * stride;
            // Use empty (0-byte) submissions so this test focuses on the queue-length cap rather
            // than the byte cap, which is intentionally small under `cfg(test)`.
            write_submit_desc(&mut mem, desc_gpa, fence, 0);
        }

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();
        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            irq_enable: irq_bits::FENCE | irq_bits::ERROR,
            ..Default::default()
        };

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        });

        exec.process_doorbell(&mut regs, &mut mem);

        // The queue should be capped; overflowing entries are dropped and their fences completed.
        assert_eq!(regs.completed_fence, 1);
        assert_eq!(regs.error_code, AerogpuErrorCode::Backend as u32);
        assert_eq!(regs.error_fence, 1);

        let drained = exec.drain_pending_submissions();
        assert_eq!(drained.len(), MAX_PENDING_SUBMISSIONS);
        assert_eq!(drained.first().map(|s| s.signal_fence), Some(2));
        assert_eq!(
            drained.last().map(|s| s.signal_fence),
            Some(u64::from(tail))
        );
    }

    #[test]
    fn deferred_mode_pending_submission_queue_is_bounded_by_bytes() {
        let mut mem = Bus::new(0x20000);
        let ring_gpa = 0x1000u64;
        let cmd_gpa = 0x18000u64;

        let entry_count = 512u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);

        let cmd_size_bytes = write_vsync_present_cmd_stream(&mut mem, cmd_gpa);
        let max_by_bytes =
            u32::try_from(MAX_PENDING_SUBMISSIONS_TOTAL_BYTES / cmd_size_bytes as usize).unwrap();
        assert!(
            max_by_bytes > 0,
            "test requires MAX_PENDING_SUBMISSIONS_TOTAL_BYTES to be >= one submission"
        );
        assert!(
            max_by_bytes < MAX_PENDING_SUBMISSIONS as u32,
            "test assumes byte cap is reached before the queue-length cap"
        );

        let tail = max_by_bytes + 1;
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, tail);

        for i in 0..tail {
            let fence = u64::from(i) + 1;
            let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(i) * stride;
            write_submit_desc_with_cmd(&mut mem, desc_gpa, fence, 0, cmd_gpa, cmd_size_bytes);
        }

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();
        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            irq_enable: irq_bits::FENCE | irq_bits::ERROR,
            features: FEATURE_VBLANK,
            ..Default::default()
        };
        regs.scanout0.enable = true;

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        });

        exec.process_doorbell(&mut regs, &mut mem);

        // Overflowing the byte cap should drop the oldest submission and complete its fence. Since
        // the command stream is a vsynced PRESENT and vblank pacing is active, the in-flight entry
        // is vblank-gated; dropping must force it ready to avoid deadlocking fence progress.
        assert_eq!(regs.completed_fence, 1);
        assert_eq!(regs.error_code, AerogpuErrorCode::Backend as u32);
        assert_eq!(regs.error_fence, 1);

        let drained = exec.drain_pending_submissions();
        assert_eq!(drained.len(), max_by_bytes as usize);
        assert_eq!(drained.first().map(|s| s.signal_fence), Some(2));
        assert_eq!(
            drained.last().map(|s| s.signal_fence),
            Some(u64::from(tail))
        );
    }

    #[test]
    fn immediate_duplicate_fence_entries_preserve_irq_request() {
        let mut mem = Bus::new(0x4000);
        let ring_gpa = 0x1000u64;

        let entry_count = 8u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, 2);

        let fence = 7u64;

        // Reverse the flags order: first entry suppresses IRQ, second wants one. Even though the
        // fence value is duplicated, we must preserve the IRQ request.
        let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
        write_submit_desc(&mut mem, desc0_gpa, fence, AeroGpuSubmitDesc::FLAG_NO_IRQ);
        let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + stride;
        write_submit_desc(&mut mem, desc1_gpa, fence, 0);

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();

        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            irq_enable: irq_bits::FENCE,
            ..Default::default()
        };

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Immediate,
        });

        exec.process_doorbell(&mut regs, &mut mem);

        assert_eq!(regs.completed_fence, fence);
        assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
    }

    #[test]
    fn immediate_duplicate_fence_entries_upgrade_to_vblank_gating() {
        let mut mem = Bus::new(0x5000);
        let ring_gpa = 0x1000u64;
        let cmd_gpa = 0x3000u64;

        let entry_count = 8u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, 2);

        let fence = 7u64;

        // First entry does not contain a vsynced present (no cmd stream), so it would normally be
        // scheduled as immediate. Second entry reuses the same fence but contains a vsynced present
        // command, so the fence must be gated on vblank.
        let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
        write_submit_desc(&mut mem, desc0_gpa, fence, 0);

        let cmd_size_bytes = write_vsync_present_cmd_stream(&mut mem, cmd_gpa);
        let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + stride;
        write_submit_desc_with_cmd(&mut mem, desc1_gpa, fence, 0, cmd_gpa, cmd_size_bytes);

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();

        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            irq_enable: irq_bits::FENCE,
            ..Default::default()
        };
        regs.scanout0.enable = true;

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Immediate,
        });

        exec.process_doorbell(&mut regs, &mut mem);
        assert_eq!(
            regs.completed_fence, 0,
            "fence must be gated on vblank once any submission with that fence contains a vsynced present"
        );
        assert_eq!(regs.irq_status & irq_bits::FENCE, 0);

        exec.process_vblank_tick(&mut regs, &mut mem);
        assert_eq!(regs.completed_fence, fence);
        assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
    }

    #[test]
    fn process_doorbell_rejects_overflowing_ring_gpa_without_touching_memory() {
        struct PanicBus;

        impl MemoryBus for PanicBus {
            fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
                panic!("unexpected guest memory read");
            }

            fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
                panic!("unexpected guest memory write");
            }
        }

        let mut mem = PanicBus;
        let mut regs = AeroGpuRegs {
            ring_control: ring_control::ENABLE,
            ring_gpa: u64::MAX - 1,
            ring_size_bytes: 0x1000,
            ..Default::default()
        };

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig::default());
        exec.process_doorbell(&mut regs, &mut mem);

        assert_eq!(regs.error_code, AerogpuErrorCode::Oob as u32);
        assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
    }

    #[test]
    fn duplicate_fence_entries_preserve_present_writeback_metadata() {
        #[derive(Debug)]
        struct CompletingBackend {
            completions: Vec<AeroGpuBackendCompletion>,
            scanout: AeroGpuBackendScanout,
        }

        impl AeroGpuCommandBackend for CompletingBackend {
            fn reset(&mut self) {}

            fn submit(
                &mut self,
                _mem: &mut dyn MemoryBus,
                _submission: AeroGpuBackendSubmission,
            ) -> Result<(), String> {
                Ok(())
            }

            fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
                core::mem::take(&mut self.completions)
            }

            fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
                Some(self.scanout.clone())
            }
        }

        let mut mem = Bus::new(0x8000);
        let ring_gpa = 0x1000u64;
        let fb_gpa = 0x3000u64;

        let entry_count = 8u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, 2);

        let fence = 7u64;

        let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
        write_submit_desc(&mut mem, desc0_gpa, fence, AeroGpuSubmitDesc::FLAG_PRESENT);
        let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + stride;
        write_submit_desc(&mut mem, desc1_gpa, fence, AeroGpuSubmitDesc::FLAG_NO_IRQ);

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();

        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            irq_enable: irq_bits::FENCE,
            ..Default::default()
        };
        regs.scanout0.enable = true;
        regs.scanout0.width = 1;
        regs.scanout0.height = 1;
        regs.scanout0.pitch_bytes = 4;
        regs.scanout0.format = AeroGpuFormat::B8G8R8A8Unorm;
        regs.scanout0.fb_gpa = fb_gpa;

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        });
        exec.set_backend(Box::new(CompletingBackend {
            completions: vec![AeroGpuBackendCompletion { fence, error: None }],
            scanout: AeroGpuBackendScanout {
                width: 1,
                height: 1,
                rgba8: vec![1, 2, 3, 4],
            },
        }));

        exec.process_doorbell(&mut regs, &mut mem);

        assert_eq!(mem.read_u32(fb_gpa), u32::from_le_bytes([3, 2, 1, 4]));
        assert_eq!(regs.completed_fence, fence);
    }

    #[test]
    fn scanout_writeback_rejects_oversized_dimensions_before_touching_buffer() {
        let mut mem = Bus::new(0x1000);
        let mut regs = AeroGpuRegs::default();
        regs.scanout0.enable = true;
        regs.scanout0.width = 1;
        regs.scanout0.height = u32::try_from(MAX_SCANOUT_WRITEBACK_BYTES / 4 + 1).unwrap();
        regs.scanout0.format = AeroGpuFormat::R8G8B8A8Unorm;
        regs.scanout0.pitch_bytes = 4;
        regs.scanout0.fb_gpa = 0x100;

        // Do not allocate a huge RGBA buffer; the writeback path should reject on size alone.
        let scanout = AeroGpuBackendScanout {
            width: 1,
            height: regs.scanout0.height,
            rgba8: Vec::new(),
        };

        let err = write_scanout0_rgba8(&regs, &mut mem, &scanout).unwrap_err();
        assert!(err.contains("too large"), "expected size error, got: {err}");
    }

    #[test]
    fn scanout_writeback_rejects_address_overflow() {
        let mut mem = Bus::new(0x1000);
        let mut regs = AeroGpuRegs::default();
        regs.scanout0.enable = true;
        regs.scanout0.width = 1;
        regs.scanout0.height = 2;
        regs.scanout0.format = AeroGpuFormat::R8G8B8A8Unorm;
        regs.scanout0.pitch_bytes = 4;
        // Force wrap when computing `fb_gpa + pitch * (height - 1)`.
        regs.scanout0.fb_gpa = u64::MAX - 1;

        let scanout = AeroGpuBackendScanout {
            width: 1,
            height: 2,
            rgba8: Vec::new(),
        };

        let err = write_scanout0_rgba8(&regs, &mut mem, &scanout).unwrap_err();
        assert!(
            err.contains("address overflow"),
            "expected address overflow error, got: {err}"
        );
    }
}
