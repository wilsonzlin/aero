use std::collections::{BTreeMap, HashSet, VecDeque};

use aero_protocol::aerogpu::aerogpu_cmd::{
    decode_cmd_hdr_le, decode_cmd_stream_header_le, AerogpuCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader, AEROGPU_PRESENT_FLAG_VSYNC,
};
use memory::MemoryBus;

use crate::devices::aerogpu_regs::{irq_bits, ring_control, AeroGpuRegs, FEATURE_VBLANK};
use crate::devices::aerogpu_ring::{
    AeroGpuRingHeader, AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_FENCE_PAGE_SIZE_BYTES,
    AEROGPU_RING_HEADER_SIZE_BYTES,
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
    pub desc: AeroGpuSubmitDesc,
}

#[derive(Clone, Debug)]
pub struct AeroGpuExecutor {
    cfg: AeroGpuExecutorConfig,
    pub last_submissions: VecDeque<AeroGpuSubmissionRecord>,
    pending_fences: VecDeque<PendingFenceCompletion>,
    in_flight: BTreeMap<u64, InFlightSubmission>,
    completed_before_submit: HashSet<u64>,
}

impl AeroGpuExecutor {
    pub fn new(cfg: AeroGpuExecutorConfig) -> Self {
        Self {
            cfg,
            last_submissions: VecDeque::new(),
            pending_fences: VecDeque::new(),
            in_flight: BTreeMap::new(),
            completed_before_submit: HashSet::new(),
        }
    }

    pub fn reset(&mut self) {
        self.pending_fences.clear();
        self.in_flight.clear();
        self.completed_before_submit.clear();
    }

    pub fn flush_pending_fences(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        if self.cfg.fence_completion != AeroGpuFenceCompletionMode::Immediate {
            self.pending_fences.clear();
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
            entry.completed = true;
        } else {
            // Allow completions to arrive before `process_doorbell` consumes the corresponding
            // descriptor. We'll apply this completion when the submit arrives.
            self.completed_before_submit.insert(fence);
            return;
        }

        self.advance_completed_fence(regs, mem);
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

            let (completed, flags) = {
                let next = self
                    .in_flight
                    .get(&next_fence)
                    .expect("key came from in_flight");
                (next.completed, next.desc.flags)
            };

            if !completed {
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
            if desc.desc_size_bytes < AeroGpuSubmitDesc::SIZE_BYTES
                || desc.desc_size_bytes > ring.entry_stride_bytes
            {
                regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            }

            match self.cfg.fence_completion {
                AeroGpuFenceCompletionMode::Immediate => {
                    let mut vsync_present = false;
                    if desc.cmd_size_bytes != 0 {
                        match cmd_stream_has_vsync_present(mem, desc.cmd_gpa, desc.cmd_size_bytes) {
                            Ok(vsync) => vsync_present = vsync,
                            Err(_) => {
                                // Malformed streams still execute as "immediate" for pacing
                                // purposes (to avoid deadlocks) but are counted for diagnostics.
                                regs.stats.malformed_submissions =
                                    regs.stats.malformed_submissions.saturating_add(1);
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
                    if desc.signal_fence > regs.completed_fence {
                        let already_completed =
                            self.completed_before_submit.remove(&desc.signal_fence);
                        self.in_flight.insert(
                            desc.signal_fence,
                            InFlightSubmission {
                                desc: desc.clone(),
                                completed: already_completed,
                            },
                        );

                        if already_completed {
                            self.advance_completed_fence(regs, mem);
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
                    desc: desc.clone(),
                });
            }

            if self.cfg.verbose {
                eprintln!(
                    "aerogpu: submit head={} tail={} fence={} flags=0x{:x} cmd_gpa=0x{:x} cmd_size={}",
                    head, tail, desc.signal_fence, desc.flags, desc.cmd_gpa, desc.cmd_size_bytes
                );
            }

            head = head.wrapping_add(1);
            processed += 1;
            AeroGpuRingHeader::write_head(mem, regs.ring_gpa, head);
        }

        if self.cfg.fence_completion == AeroGpuFenceCompletionMode::Immediate {
            // Complete any immediate fences that are not blocked behind a vsync fence.
            self.complete_immediate_fences(regs, mem);
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

        // Initialize header (idempotent) and update completed value.
        mem.write_u32(regs.fence_gpa + 0, AEROGPU_FENCE_PAGE_MAGIC);
        mem.write_u32(regs.fence_gpa + 4, regs.abi_version);
        mem.write_u64(regs.fence_gpa + 8, regs.completed_fence);

        // Keep writes within the defined struct size; do not touch the rest of the page.
        let _ = AEROGPU_FENCE_PAGE_SIZE_BYTES;
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

fn cmd_stream_has_vsync_present(
    mem: &mut dyn MemoryBus,
    cmd_gpa: u64,
    cmd_size_bytes: u32,
) -> Result<bool, ()> {
    let cmd_size = usize::try_from(cmd_size_bytes).map_err(|_| ())?;
    if cmd_size < AerogpuCmdStreamHeader::SIZE_BYTES {
        return Err(());
    }

    let mut stream_hdr_bytes = [0u8; AerogpuCmdStreamHeader::SIZE_BYTES];
    mem.read_physical(cmd_gpa, &mut stream_hdr_bytes);
    let stream_hdr = decode_cmd_stream_header_le(&stream_hdr_bytes).map_err(|_| ())?;

    let declared_size = stream_hdr.size_bytes as usize;
    if declared_size > cmd_size {
        return Err(());
    }

    let mut offset = AerogpuCmdStreamHeader::SIZE_BYTES;
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
    completed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::devices::aerogpu_ring::AEROGPU_RING_MAGIC;
    use memory::Bus;

    fn write_ring_header(mem: &mut dyn MemoryBus, gpa: u64, entry_count: u32, head: u32, tail: u32) {
        let stride = AeroGpuSubmitDesc::SIZE_BYTES;
        let size_bytes =
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * u64::from(stride))
                .unwrap();

        mem.write_u32(gpa + 0, AEROGPU_RING_MAGIC);
        mem.write_u32(gpa + 4, AeroGpuRegs::default().abi_version);
        mem.write_u32(gpa + 8, size_bytes);
        mem.write_u32(gpa + 12, entry_count);
        mem.write_u32(gpa + 16, stride);
        mem.write_u32(gpa + 20, 0);
        mem.write_u32(gpa + 24, head);
        mem.write_u32(gpa + 28, tail);
    }

    fn write_submit_desc(mem: &mut dyn MemoryBus, gpa: u64, fence: u64, flags: u32) {
        mem.write_u32(gpa + 0, AeroGpuSubmitDesc::SIZE_BYTES);
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
            u32::try_from(AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * stride).unwrap();

        let mut regs = AeroGpuRegs::default();
        regs.ring_gpa = ring_gpa;
        regs.ring_size_bytes = ring_size_bytes;
        regs.ring_control = ring_control::ENABLE;
        regs.fence_gpa = fence_gpa;
        regs.irq_enable = irq_bits::FENCE;

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
        assert_eq!(mem.read_u32(fence_gpa + 0), 0);

        // Completing fence 1 should allow advancement up to 2 (since fence 2 is already complete).
        exec.complete_fence(&mut regs, &mut mem, 1);
        assert_eq!(regs.completed_fence, 2);
        assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
        assert_eq!(mem.read_u32(fence_gpa + 0), AEROGPU_FENCE_PAGE_MAGIC);
        assert_eq!(mem.read_u64(fence_gpa + 8), 2);

        // Ack the IRQ bit and ensure completing fence 3 raises again.
        regs.irq_status = 0;
        exec.complete_fence(&mut regs, &mut mem, 3);
        assert_eq!(regs.completed_fence, 3);
        assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
        assert_eq!(mem.read_u64(fence_gpa + 8), 3);

        // Duplicate completion should not retrigger IRQ or advance.
        regs.irq_status = 0;
        exec.complete_fence(&mut regs, &mut mem, 3);
        assert_eq!(regs.completed_fence, 3);
        assert_eq!(regs.irq_status & irq_bits::FENCE, 0);
    }
}
