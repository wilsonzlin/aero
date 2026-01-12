use std::collections::{BTreeMap, HashSet, VecDeque};

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_hdr_le, decode_cmd_stream_header_le, AerogpuCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuCmdStreamIter,
    AEROGPU_PRESENT_FLAG_VSYNC,
};
use memory::MemoryBus;

use crate::devices::aerogpu_regs::{irq_bits, ring_control, AeroGpuRegs, FEATURE_VBLANK};
use crate::devices::aerogpu_ring::{
    AeroGpuAllocEntry, AeroGpuAllocTableHeader, AeroGpuRingHeader, AeroGpuSubmitDesc,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES,
};
use crate::devices::aerogpu_scanout::AeroGpuFormat;
use crate::gpu_worker::aerogpu_backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend, NullAeroGpuBackend,
};
use crate::gpu_worker::aerogpu_software::AeroGpuSoftwareExecutor;

#[cfg(feature = "aerogpu-trace")]
use aero_gpu_trace::{
    AerogpuMemoryRangeCapture, AerogpuSubmissionCapture, TraceMeta, TraceWriteError, TraceWriter,
};

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
    pending_fences: VecDeque<PendingFenceCompletion>,
    in_flight: BTreeMap<u64, InFlightSubmission>,
    completed_before_submit: HashSet<u64>,
    backend: Box<dyn AeroGpuCommandBackend>,
    software: AeroGpuSoftwareExecutor,
    #[cfg(feature = "aerogpu-trace")]
    trace: Option<AerogpuSubmissionTrace>,
}

impl Clone for AeroGpuExecutor {
    fn clone(&self) -> Self {
        Self {
            cfg: self.cfg.clone(),
            last_submissions: self.last_submissions.clone(),
            pending_fences: self.pending_fences.clone(),
            in_flight: self.in_flight.clone(),
            completed_before_submit: self.completed_before_submit.clone(),
            backend: Box::new(NullAeroGpuBackend::new()),
            software: self.software.clone(),
            #[cfg(feature = "aerogpu-trace")]
            trace: None,
        }
    }
}

impl AeroGpuExecutor {
    pub fn new(cfg: AeroGpuExecutorConfig) -> Self {
        Self {
            cfg,
            last_submissions: VecDeque::new(),
            pending_fences: VecDeque::new(),
            in_flight: BTreeMap::new(),
            completed_before_submit: HashSet::new(),
            backend: Box::new(NullAeroGpuBackend::new()),
            software: AeroGpuSoftwareExecutor::new(),
            #[cfg(feature = "aerogpu-trace")]
            trace: None,
        }
    }

    pub fn set_backend(&mut self, backend: Box<dyn AeroGpuCommandBackend>) {
        self.backend = backend;
    }

    pub fn reset(&mut self) {
        self.pending_fences.clear();
        self.in_flight.clear();
        self.completed_before_submit.clear();
        self.backend.reset();
        self.software.reset();
    }

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
                regs.irq_status |= irq_bits::ERROR;
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
                        regs.irq_status |= irq_bits::ERROR;
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

    #[cfg(feature = "aerogpu-trace")]
    pub fn start_trace_in_memory(
        &mut self,
        emulator_version: impl Into<String>,
    ) -> Result<(), TraceWriteError> {
        self.trace = Some(AerogpuSubmissionTrace::new_in_memory(emulator_version)?);
        Ok(())
    }

    #[cfg(feature = "aerogpu-trace")]
    pub fn finish_trace(&mut self) -> Result<Option<Vec<u8>>, TraceWriteError> {
        let Some(trace) = self.trace.take() else {
            return Ok(None);
        };
        Ok(Some(trace.finish()?))
    }

    pub fn process_doorbell(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        regs.stats.doorbells = regs.stats.doorbells.saturating_add(1);

        // If vblank pacing is not active, do not allow vsynced fences to remain queued forever.
        if self.cfg.fence_completion == AeroGpuFenceCompletionMode::Immediate
            && ((regs.features & FEATURE_VBLANK) == 0 || !regs.scanout0.enable)
        {
            self.flush_pending_fences(regs, mem);
        }

        if regs.ring_control & ring_control::ENABLE == 0 {
            return;
        }
        if regs.ring_gpa == 0 || regs.ring_size_bytes == 0 {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.irq_status |= irq_bits::ERROR;
            return;
        }

        let ring = AeroGpuRingHeader::read_from(mem, regs.ring_gpa);
        if !ring.is_valid(regs.ring_size_bytes) {
            regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            regs.irq_status |= irq_bits::ERROR;
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
            return;
        }

        let mut processed = 0u32;
        let max = ring.entry_count.min(pending);

        while head != tail && processed < max {
            let desc_gpa = regs.ring_gpa
                + AEROGPU_RING_HEADER_SIZE_BYTES
                + (u64::from(ring.slot_index(head)) * u64::from(ring.entry_stride_bytes));
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
            let (cmd_stream_header, cmd_stream) = decode_cmd_stream(
                mem,
                regs.abi_version,
                &desc,
                &mut decode_errors,
                capture_cmd_stream,
            );

            if !decode_errors.is_empty() {
                regs.stats.malformed_submissions =
                    regs.stats.malformed_submissions.saturating_add(1);
                regs.irq_status |= irq_bits::ERROR;
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
                    // Most submissions are regular render work; avoid scanning the command stream
                    // unless the KMD marked this submission as containing a PRESENT.
                    let wants_present = (desc.flags & AeroGpuSubmitDesc::FLAG_PRESENT) != 0;
                    let cmd_stream_ok = desc.cmd_gpa != 0
                        && desc.cmd_size_bytes != 0
                        && cmd_stream_header.is_some()
                        && !decode_errors
                            .iter()
                            .any(|e| matches!(e, AeroGpuSubmissionDecodeError::CmdStream(_)));
                    if decode_errors.is_empty() && cmd_stream_ok {
                        self.software.execute_submission(regs, mem, &desc);
                    }
                    if wants_present && cmd_stream_ok {
                        let scan_result = if cmd_stream.is_empty() {
                            cmd_stream_has_vsync_present(mem, desc.cmd_gpa, desc.cmd_size_bytes)
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

                    let alloc_table = if desc.alloc_table_gpa != 0
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
                        if size == 0 {
                            None
                        } else {
                            let mut bytes = vec![0u8; size];
                            mem.read_physical(desc.alloc_table_gpa, &mut bytes);
                            Some(bytes)
                        }
                    } else {
                        None
                    };

                    if cmd_stream_ok {
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
                            let mut bytes = vec![0u8; size];
                            mem.read_physical(desc.cmd_gpa, &mut bytes);
                            bytes
                        } else {
                            cmd_stream.clone()
                        };

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
                            regs.irq_status |= irq_bits::ERROR;
                        }

                        if wants_present {
                            if let Some(scanout) = self.backend.read_scanout_rgba8(0) {
                                if let Err(err) = write_scanout0_rgba8(regs, mem, &scanout) {
                                    if self.cfg.verbose {
                                        eprintln!("aerogpu: scanout writeback failed: {err}");
                                    }
                                    regs.stats.gpu_exec_errors =
                                        regs.stats.gpu_exec_errors.saturating_add(1);
                                    regs.irq_status |= irq_bits::ERROR;
                                }
                            }
                        }
                    }

                    let can_pace_vsync = vsync_present
                        && (regs.features & FEATURE_VBLANK) != 0
                        && regs.scanout0.enable;

                    let wants_irq = desc.flags & AeroGpuSubmitDesc::FLAG_NO_IRQ == 0;

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
                            kind: if can_pace_vsync {
                                PendingFenceKind::Vblank
                            } else {
                                PendingFenceKind::Immediate
                            },
                        });
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

                    let alloc_table = if desc.alloc_table_gpa != 0
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
                        if size == 0 {
                            None
                        } else {
                            let mut bytes = vec![0u8; size];
                            mem.read_physical(desc.alloc_table_gpa, &mut bytes);
                            Some(bytes)
                        }
                    } else {
                        None
                    };

                    let can_pace_vsync = vsync_present
                        && (regs.features & FEATURE_VBLANK) != 0
                        && regs.scanout0.enable;
                    let kind = if can_pace_vsync {
                        PendingFenceKind::Vblank
                    } else {
                        PendingFenceKind::Immediate
                    };

                    if desc.signal_fence > regs.completed_fence {
                        let already_completed =
                            self.completed_before_submit.remove(&desc.signal_fence);
                        self.in_flight.insert(
                            desc.signal_fence,
                            InFlightSubmission {
                                desc: desc.clone(),
                                kind,
                                completed_backend: already_completed,
                                vblank_ready: kind == PendingFenceKind::Immediate,
                            },
                        );

                        if already_completed && kind == PendingFenceKind::Immediate {
                            self.advance_completed_fence(regs, mem);
                        }
                    }

                    let submit = AeroGpuBackendSubmission {
                        flags: desc.flags,
                        context_id: desc.context_id,
                        engine_id: desc.engine_id,
                        signal_fence: desc.signal_fence,
                        cmd_stream: cmd_stream.clone(),
                        alloc_table,
                    };

                    if self.backend.submit(mem, submit).is_err() {
                        regs.stats.gpu_exec_errors = regs.stats.gpu_exec_errors.saturating_add(1);
                        regs.irq_status |= irq_bits::ERROR;
                        // If the backend rejects the submission, still unblock the fence.
                        if let Some(entry) = self.in_flight.get_mut(&desc.signal_fence) {
                            entry.vblank_ready = true;
                        }
                        self.complete_fence(regs, mem, desc.signal_fence);
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
                    head, tail, desc.signal_fence, desc.flags, desc.cmd_gpa, desc.cmd_size_bytes
                    , alloc_count, cmd_header_size, decode_error_count,
                );
            }

            #[cfg(feature = "aerogpu-trace")]
            if let Some(trace) = self.trace.as_mut() {
                if let Err(err) = trace.record_submission(mem, &desc) {
                    eprintln!("aerogpu trace: disabling recorder due to error: {err:?}");
                    self.trace = None;
                }
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

        crate::devices::aerogpu_ring::write_fence_page(
            mem,
            regs.fence_gpa,
            regs.abi_version,
            regs.completed_fence,
        );
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
}
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
    let expected_src_len = src_width
        .checked_mul(src_height)
        .and_then(|v| v.checked_mul(4))
        .ok_or_else(|| "scanout dimensions overflow".to_string())?;
    if scanout.rgba8.len() < expected_src_len {
        return Err(format!(
            "scanout rgba8 buffer too small: need {expected_src_len} bytes for {src_width}x{src_height}, have {}",
            scanout.rgba8.len()
        ));
    }

    let copy_width = dst_width.min(src_width);
    let copy_height = dst_height.min(src_height);

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

    let mut row_buf = vec![0u8; row_bytes];
    for y in 0..copy_height {
        let src_row_start = y * src_width * 4;
        let src_row = &scanout.rgba8[src_row_start..src_row_start + src_width * 4];

        match regs.scanout0.format {
            AeroGpuFormat::B8G8R8A8Unorm => {
                for x in 0..copy_width {
                    let src = &src_row[x * 4..x * 4 + 4];
                    let dst = &mut row_buf[x * 4..x * 4 + 4];
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                    dst[3] = src[3];
                }
            }
            AeroGpuFormat::B8G8R8X8Unorm => {
                for x in 0..copy_width {
                    let src = &src_row[x * 4..x * 4 + 4];
                    let dst = &mut row_buf[x * 4..x * 4 + 4];
                    dst[0] = src[2];
                    dst[1] = src[1];
                    dst[2] = src[0];
                    dst[3] = 0xff;
                }
            }
            AeroGpuFormat::R8G8B8A8Unorm => {
                row_buf[..copy_width * 4].copy_from_slice(&src_row[..copy_width * 4]);
            }
            AeroGpuFormat::R8G8B8X8Unorm => {
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
            AeroGpuFormat::Invalid
            | AeroGpuFormat::D24UnormS8Uint
            | AeroGpuFormat::D32Float
            | AeroGpuFormat::Bc1Unorm
            | AeroGpuFormat::Bc2Unorm
            | AeroGpuFormat::Bc3Unorm
            | AeroGpuFormat::Bc7Unorm => {
                return Err(format!(
                    "unsupported scanout format {:?}",
                    regs.scanout0.format
                ));
            }
        }

        let row_gpa = regs.scanout0.fb_gpa + (y as u64) * (regs.scanout0.pitch_bytes as u64);
        mem.write_physical(row_gpa, &row_buf);
    }

    Ok(())
}
const MAX_ALLOC_TABLE_SIZE_BYTES: u32 = 16 * 1024 * 1024;
const MAX_CMD_STREAM_SIZE_BYTES: u32 = 64 * 1024 * 1024;

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

fn cmd_stream_has_vsync_present_bytes(bytes: &[u8]) -> Result<bool, ()> {
    let iter = AerogpuCmdStreamIter::new(bytes).map_err(|_| ())?;
    for packet in iter {
        let packet = packet.map_err(|_| ())?;
        if matches!(
            packet.opcode,
            Some(AerogpuCmdOpcode::Present) | Some(AerogpuCmdOpcode::PresentEx)
        ) {
            // flags is always after the scanout_id field.
            if packet.payload.len() < 8 {
                return Err(());
            }
            let flags = u32::from_le_bytes(packet.payload[4..8].try_into().unwrap());
            if (flags & AEROGPU_PRESENT_FLAG_VSYNC) != 0 {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn cmd_stream_has_vsync_present(
    mem: &mut dyn MemoryBus,
    cmd_gpa: u64,
    cmd_size_bytes: u32,
) -> Result<bool, ()> {
    let cmd_size = usize::try_from(cmd_size_bytes).map_err(|_| ())?;
    if cmd_size < ProtocolCmdStreamHeader::SIZE_BYTES {
        return Err(());
    }

    let mut stream_hdr_bytes = [0u8; ProtocolCmdStreamHeader::SIZE_BYTES];
    mem.read_physical(cmd_gpa, &mut stream_hdr_bytes);
    let stream_hdr = decode_cmd_stream_header_le(&stream_hdr_bytes).map_err(|_| ())?;

    let declared_size = stream_hdr.size_bytes as usize;
    if declared_size > cmd_size {
        return Err(());
    }

    let mut offset = ProtocolCmdStreamHeader::SIZE_BYTES;
    while offset < declared_size {
        let rem = declared_size - offset;
        if rem < AerogpuCmdHdr::SIZE_BYTES {
            return Err(());
        }

        let cmd_hdr_gpa = cmd_gpa.checked_add(offset as u64).ok_or(())?;
        let mut cmd_hdr_bytes = [0u8; AerogpuCmdHdr::SIZE_BYTES];
        mem.read_physical(cmd_hdr_gpa, &mut cmd_hdr_bytes);
        let cmd_hdr = decode_cmd_hdr_le(&cmd_hdr_bytes).map_err(|_| ())?;

        let cmd_size = cmd_hdr.size_bytes as usize;
        let end = offset.checked_add(cmd_size).ok_or(())?;
        if end > declared_size {
            return Err(());
        }

        if cmd_hdr.opcode == AerogpuCmdOpcode::Present as u32
            || cmd_hdr.opcode == AerogpuCmdOpcode::PresentEx as u32
        {
            // flags is always at offset 12 (hdr + scanout_id).
            if cmd_size < 16 {
                return Err(());
            }
            let flags_gpa = cmd_hdr_gpa.checked_add(12).ok_or(())?;
            let flags = mem.read_u32(flags_gpa);
            if (flags & AEROGPU_PRESENT_FLAG_VSYNC) != 0 {
                return Ok(true);
            }
        }

        offset += cmd_size;
    }

    Ok(false)
}

#[derive(Clone, Debug)]
struct InFlightSubmission {
    desc: AeroGpuSubmitDesc,
    kind: PendingFenceKind,
    completed_backend: bool,
    vblank_ready: bool,
}

impl InFlightSubmission {
    fn is_ready(&self) -> bool {
        match self.kind {
            PendingFenceKind::Immediate => self.completed_backend,
            PendingFenceKind::Vblank => self.completed_backend && self.vblank_ready,
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    use crate::devices::aerogpu_ring::{
        AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_MAGIC, FENCE_PAGE_COMPLETED_FENCE_OFFSET,
        FENCE_PAGE_MAGIC_OFFSET, RING_HEAD_OFFSET, RING_TAIL_OFFSET,
    };
    use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC;
    use memory::Bus;

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

    #[test]
    fn fence_completions_advance_monotonically_and_raise_irq_on_advances() {
        let mut mem = Bus::new(0x4000);
        let ring_gpa = 0x1000u64;
        let fence_gpa = 0x2000u64;

        let entry_count = 8u32;
        let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
        write_ring_header(&mut mem, ring_gpa, entry_count, 0, 3);

        for (slot, fence) in [1u64, 2, 3].into_iter().enumerate() {
            let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + (slot as u64) * stride;
            write_submit_desc(&mut mem, desc_gpa, fence, 0);
        }

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride)
                .unwrap();

        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            fence_gpa,
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

        let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
        assert_eq!(ring.head, 3);
        assert_eq!(ring.tail, 3);

        // Completion arrives out-of-order: fence 2 first should not advance completed_fence.
        exec.complete_fence(&mut regs, &mut mem, 2);
        assert_eq!(regs.completed_fence, 0);
        assert_eq!(regs.irq_status & irq_bits::FENCE, 0);
        assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

        // Completing fence 1 should allow advancement up to 2 (since fence 2 is already complete).
        exec.complete_fence(&mut regs, &mut mem, 1);
        assert_eq!(regs.completed_fence, 2);
        assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
        assert_eq!(
            mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
            AEROGPU_FENCE_PAGE_MAGIC
        );
        assert_eq!(
            mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
            2
        );

        // Ack the IRQ bit and ensure completing fence 3 raises again.
        regs.irq_status = 0;
        exec.complete_fence(&mut regs, &mut mem, 3);
        assert_eq!(regs.completed_fence, 3);
        assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
        assert_eq!(
            mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
            3
        );

        // Duplicate completion should not retrigger IRQ or advance.
        regs.irq_status = 0;
        exec.complete_fence(&mut regs, &mut mem, 3);
        assert_eq!(regs.completed_fence, 3);
        assert_eq!(regs.irq_status & irq_bits::FENCE, 0);
    }

    #[test]
    fn scanout_writeback_converts_rgba_to_16bpp_formats() {
        let mut mem = Bus::new(0x4000);
        let fb_gpa = 0x1000u64;

        let scanout = AeroGpuBackendScanout {
            width: 1,
            height: 1,
            rgba8: vec![255, 0, 0, 255], // opaque red
        };

        let mut regs = AeroGpuRegs::default();
        regs.scanout0.enable = true;
        regs.scanout0.width = 1;
        regs.scanout0.height = 1;
        regs.scanout0.fb_gpa = fb_gpa;
        regs.scanout0.pitch_bytes = 2;

        // RGB565: R=31 -> 0xF800.
        regs.scanout0.format = AeroGpuFormat::B5G6R5Unorm;
        write_scanout0_rgba8(&regs, &mut mem, &scanout).unwrap();
        assert_eq!(mem.read_u16(fb_gpa), 0xF800);

        // BGRA5551: A=1, R=31 -> 0xFC00.
        regs.scanout0.format = AeroGpuFormat::B5G5R5A1Unorm;
        write_scanout0_rgba8(&regs, &mut mem, &scanout).unwrap();
        assert_eq!(mem.read_u16(fb_gpa), 0xFC00);
    }

    #[test]
    fn alloc_table_decode_accepts_extended_entry_stride() {
        let mut mem = Bus::new(0x4000);
        let alloc_table_gpa = 0x1000u64;
        let abi_version = AeroGpuRegs::default().abi_version;

        let entry_count = 2u32;
        let entry_stride_bytes = AeroGpuAllocEntry::SIZE_BYTES + 16;
        let size_bytes = AeroGpuAllocTableHeader::SIZE_BYTES + entry_count * entry_stride_bytes;

        // Write the allocation table header.
        mem.write_u32(alloc_table_gpa, AEROGPU_ALLOC_TABLE_MAGIC);
        mem.write_u32(alloc_table_gpa + 4, abi_version);
        mem.write_u32(alloc_table_gpa + 8, size_bytes);
        mem.write_u32(alloc_table_gpa + 12, entry_count);
        mem.write_u32(alloc_table_gpa + 16, entry_stride_bytes);
        mem.write_u32(alloc_table_gpa + 20, 0);

        // Write entries at the declared stride, leaving the extra bytes as zero padding.
        let header_size = u64::from(AeroGpuAllocTableHeader::SIZE_BYTES);
        for i in 0..entry_count {
            let entry_gpa =
                alloc_table_gpa + header_size + u64::from(i) * u64::from(entry_stride_bytes);
            let alloc_id = i + 1;
            mem.write_u32(entry_gpa, alloc_id);
            mem.write_u32(entry_gpa + 4, 0);
            mem.write_u64(entry_gpa + 8, 0x2000u64 + u64::from(alloc_id) * 0x1000);
            mem.write_u64(entry_gpa + 16, 0x100);
            mem.write_u64(entry_gpa + 24, 0);
        }

        let desc = AeroGpuSubmitDesc {
            desc_size_bytes: AeroGpuSubmitDesc::SIZE_BYTES,
            flags: 0,
            context_id: 0,
            engine_id: 0,
            cmd_gpa: 0,
            cmd_size_bytes: 0,
            alloc_table_gpa,
            alloc_table_size_bytes: size_bytes,
            signal_fence: 0,
        };

        let mut decode_errors = Vec::new();
        let (_hdr, allocs) = decode_alloc_table(&mut mem, abi_version, &desc, &mut decode_errors);
        assert!(decode_errors.is_empty());
        assert_eq!(allocs.len(), entry_count as usize);
        assert_eq!(allocs[0].alloc_id, 1);
        assert_eq!(allocs[1].alloc_id, 2);
    }

    #[derive(Debug, Default)]
    struct RejectingBackend;

    impl AeroGpuCommandBackend for RejectingBackend {
        fn reset(&mut self) {}

        fn submit(
            &mut self,
            _mem: &mut dyn MemoryBus,
            _submission: AeroGpuBackendSubmission,
        ) -> Result<(), String> {
            Err("backend rejected submission".to_string())
        }

        fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
            Vec::new()
        }

        fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
            None
        }
    }

    #[test]
    fn executor_completes_fence_when_backend_rejects_submission() {
        let mut mem = Bus::new(0x8000);
        let ring_gpa = 0x1000u64;
        let fence_gpa = 0x2000u64;
        let cmd_gpa = 0x3000u64;

        // Minimal command stream: header only (no packets).
        let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32;
        mem.write_u32(cmd_gpa, AEROGPU_CMD_STREAM_MAGIC);
        mem.write_u32(cmd_gpa + 4, AeroGpuRegs::default().abi_version);
        mem.write_u32(cmd_gpa + 8, cmd_size_bytes);
        mem.write_u32(cmd_gpa + 12, 0); // flags
        mem.write_u32(cmd_gpa + 16, 0); // reserved0
        mem.write_u32(cmd_gpa + 20, 0); // reserved1

        // One ring entry with a fence.
        write_ring_header(&mut mem, ring_gpa, 1, 0, 1);
        let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
        mem.write_u32(desc_gpa, AeroGpuSubmitDesc::SIZE_BYTES);
        mem.write_u32(desc_gpa + 4, 0); // flags
        mem.write_u32(desc_gpa + 8, 0); // context_id
        mem.write_u32(desc_gpa + 12, 0); // engine_id
        mem.write_u64(desc_gpa + 16, cmd_gpa);
        mem.write_u32(desc_gpa + 24, cmd_size_bytes);
        mem.write_u64(desc_gpa + 32, 0); // alloc_table_gpa
        mem.write_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
        mem.write_u64(desc_gpa + 48, 7); // signal_fence

        let ring_size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + AeroGpuSubmitDesc::SIZE_BYTES as u64)
                .unwrap();

        let mut regs = AeroGpuRegs {
            ring_gpa,
            ring_size_bytes,
            ring_control: ring_control::ENABLE,
            fence_gpa,
            irq_enable: irq_bits::FENCE,
            ..Default::default()
        };

        let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
            verbose: false,
            keep_last_submissions: 0,
            fence_completion: AeroGpuFenceCompletionMode::Deferred,
        });
        exec.set_backend(Box::new(RejectingBackend));

        exec.process_doorbell(&mut regs, &mut mem);

        // Backend submission failures should still allow fences to advance so the guest makes progress.
        assert_eq!(regs.completed_fence, 7);
        assert_eq!(regs.stats.gpu_exec_errors, 1);
        assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

        let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
        assert_eq!(ring.head, 1);
        assert_eq!(ring.tail, 1);

        assert_eq!(
            mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
            AEROGPU_FENCE_PAGE_MAGIC
        );
        assert_eq!(
            mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
            7
        );
    }
}

#[cfg(feature = "aerogpu-trace")]
struct AerogpuSubmissionTrace {
    writer: TraceWriter<Vec<u8>>,
    frame_index: u32,
    frame_open: bool,
}

#[cfg(feature = "aerogpu-trace")]
impl core::fmt::Debug for AerogpuSubmissionTrace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AerogpuSubmissionTrace")
            .field("frame_index", &self.frame_index)
            .field("frame_open", &self.frame_open)
            .finish()
    }
}

#[cfg(feature = "aerogpu-trace")]
impl AerogpuSubmissionTrace {
    fn new_in_memory(emulator_version: impl Into<String>) -> Result<Self, TraceWriteError> {
        let mut meta = TraceMeta::new(
            emulator_version,
            crate::devices::aerogpu_regs::AEROGPU_ABI_VERSION_U32,
        );
        meta.notes = Some("raw aerogpu_ring submissions + aerogpu_cmd stream bytes".to_string());
        let writer = TraceWriter::new_v2(Vec::<u8>::new(), &meta)?;
        Ok(Self {
            writer,
            frame_index: 0,
            frame_open: false,
        })
    }

    fn ensure_frame_open(&mut self) -> Result<(), TraceWriteError> {
        if !self.frame_open {
            self.writer.begin_frame(self.frame_index)?;
            self.frame_open = true;
        }
        Ok(())
    }

    fn record_submission(
        &mut self,
        mem: &mut dyn MemoryBus,
        desc: &AeroGpuSubmitDesc,
    ) -> Result<(), TraceWriteError> {
        const MAX_CAPTURE_BYTES: u32 = 64 * 1024 * 1024;

        self.ensure_frame_open()?;

        // Only capture the command stream if the descriptor is consistent. Malformed descriptors
        // are still recorded (with an empty cmd stream) so tracing can't be disabled by guests
        // that set `cmd_size_bytes` without a valid GPA.
        let cmd_stream_bytes = if desc.cmd_gpa != 0
            && desc.cmd_size_bytes != 0
            && desc
                .cmd_gpa
                .checked_add(u64::from(desc.cmd_size_bytes))
                .is_some()
        {
            // Capture only the used bytes (`cmd_stream_header.size_bytes`), not the backing buffer
            // capacity (`desc.cmd_size_bytes`).
            if desc.cmd_size_bytes > MAX_CAPTURE_BYTES {
                return Err(TraceWriteError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "aerogpu trace: cmd buffer too large",
                )));
            }
            let mut capture_len = desc.cmd_size_bytes;
            if desc.cmd_size_bytes >= ProtocolCmdStreamHeader::SIZE_BYTES as u32 {
                let mut prefix = [0u8; ProtocolCmdStreamHeader::SIZE_BYTES];
                mem.read_physical(desc.cmd_gpa, &mut prefix);
                if let Ok(hdr) = decode_cmd_stream_header_le(&prefix) {
                    if hdr.size_bytes >= ProtocolCmdStreamHeader::SIZE_BYTES as u32
                        && hdr.size_bytes <= desc.cmd_size_bytes
                    {
                        capture_len = hdr.size_bytes;
                    }
                }
            }
            let cmd_size = capture_len as usize;
            let mut cmd_stream_bytes = vec![0u8; cmd_size];
            mem.read_physical(desc.cmd_gpa, &mut cmd_stream_bytes);
            cmd_stream_bytes
        } else {
            Vec::new()
        };

        let alloc_table_bytes = if desc.alloc_table_gpa != 0
            && desc.alloc_table_size_bytes != 0
            && desc
                .alloc_table_gpa
                .checked_add(u64::from(desc.alloc_table_size_bytes))
                .is_some()
        {
            // Capture only the used bytes (`alloc_table_header.size_bytes`), not the backing buffer
            // capacity (`desc.alloc_table_size_bytes`).
            if desc.alloc_table_size_bytes > MAX_CAPTURE_BYTES {
                return Err(TraceWriteError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "aerogpu trace: alloc table buffer too large",
                )));
            }

            let mut capture_len = desc.alloc_table_size_bytes;
            if desc.alloc_table_size_bytes >= AeroGpuAllocTableHeader::SIZE_BYTES {
                let hdr = AeroGpuAllocTableHeader::read_from(mem, desc.alloc_table_gpa);
                if hdr.size_bytes >= AeroGpuAllocTableHeader::SIZE_BYTES
                    && hdr.size_bytes <= desc.alloc_table_size_bytes
                {
                    capture_len = hdr.size_bytes;
                }
            }

            let alloc_size = capture_len as usize;
            let mut bytes = vec![0u8; alloc_size];
            mem.read_physical(desc.alloc_table_gpa, &mut bytes);
            Some(bytes)
        } else {
            None
        };

        struct CapturedRange {
            alloc_id: u32,
            flags: u32,
            gpa: u64,
            size_bytes: u64,
            bytes: Vec<u8>,
        }

        let mut ranges_owned = Vec::<CapturedRange>::new();

        if let Some(table) = &alloc_table_bytes {
            // Best-effort parse of the allocation table; invalid tables are still recorded as raw bytes.
            if table.len() >= AeroGpuAllocTableHeader::SIZE_BYTES as usize {
                let magic = u32::from_le_bytes(table[0..4].try_into().unwrap());
                let size_bytes = u32::from_le_bytes(table[8..12].try_into().unwrap());
                let entry_count = u32::from_le_bytes(table[12..16].try_into().unwrap());
                let entry_stride = u32::from_le_bytes(table[16..20].try_into().unwrap());

                if magic == AEROGPU_ALLOC_TABLE_MAGIC
                    && size_bytes as usize <= table.len()
                    && entry_stride >= AeroGpuAllocEntry::SIZE_BYTES
                {
                    let count = entry_count as usize;
                    let entry_stride = entry_stride as usize;
                    let mut off = AeroGpuAllocTableHeader::SIZE_BYTES as usize;
                    for _ in 0..count {
                        if off + AeroGpuAllocEntry::SIZE_BYTES as usize > table.len() {
                            break;
                        }
                        let alloc_id = u32::from_le_bytes(table[off..off + 4].try_into().unwrap());
                        let flags = u32::from_le_bytes(table[off + 4..off + 8].try_into().unwrap());
                        let gpa = u64::from_le_bytes(table[off + 8..off + 16].try_into().unwrap());
                        let size =
                            u64::from_le_bytes(table[off + 16..off + 24].try_into().unwrap());

                        if alloc_id != 0 && size != 0 && gpa.checked_add(size).is_some() {
                            let size_u32 = u32::try_from(size).unwrap_or(u32::MAX);
                            if size_u32 <= MAX_CAPTURE_BYTES {
                                let mut bytes = vec![0u8; size as usize];
                                mem.read_physical(gpa, &mut bytes);
                                ranges_owned.push(CapturedRange {
                                    alloc_id,
                                    flags,
                                    gpa,
                                    size_bytes: size,
                                    bytes,
                                });
                            }
                        }

                        let Some(next) = off.checked_add(entry_stride) else {
                            break;
                        };
                        off = next;
                    }
                }
            }
        }

        let mut ranges = Vec::with_capacity(ranges_owned.len());
        for range in &ranges_owned {
            ranges.push(AerogpuMemoryRangeCapture {
                alloc_id: range.alloc_id,
                flags: range.flags,
                gpa: range.gpa,
                size_bytes: range.size_bytes,
                bytes: &range.bytes,
            });
        }

        self.writer
            .write_aerogpu_submission(AerogpuSubmissionCapture {
                submit_flags: desc.flags,
                context_id: desc.context_id,
                engine_id: desc.engine_id,
                signal_fence: desc.signal_fence,
                cmd_stream_bytes: &cmd_stream_bytes,
                alloc_table_bytes: alloc_table_bytes.as_deref(),
                memory_ranges: &ranges,
            })?;

        if (desc.flags & AeroGpuSubmitDesc::FLAG_PRESENT) != 0 {
            self.writer.present(self.frame_index)?;
            self.frame_open = false;
            self.frame_index = self.frame_index.wrapping_add(1);
        }

        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>, TraceWriteError> {
        if self.frame_open {
            // Close the last frame even if the guest didn't submit a present.
            self.writer.present(self.frame_index)?;
        }
        self.writer.finish()
    }
}
