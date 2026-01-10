use std::collections::VecDeque;

use memory::MemoryBus;

use crate::devices::aerogpu_regs::{irq_bits, ring_control, AeroGpuRegs};
use crate::devices::aerogpu_ring::{
    AeroGpuRingHeader, AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_FENCE_PAGE_SIZE_BYTES,
    AEROGPU_RING_HEADER_SIZE_BYTES,
};

#[derive(Clone, Debug)]
pub struct AeroGpuExecutorConfig {
    pub verbose: bool,
    pub keep_last_submissions: usize,
}

impl Default for AeroGpuExecutorConfig {
    fn default() -> Self {
        Self {
            verbose: false,
            keep_last_submissions: 64,
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
}

impl AeroGpuExecutor {
    pub fn new(cfg: AeroGpuExecutorConfig) -> Self {
        Self {
            cfg,
            last_submissions: VecDeque::new(),
        }
    }

    pub fn process_doorbell(&mut self, regs: &mut AeroGpuRegs, mem: &mut dyn MemoryBus) {
        regs.stats.doorbells = regs.stats.doorbells.saturating_add(1);
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
        let mut fence_advanced = false;
        let mut fence_irq = false;

        while head != tail && processed < max {
            let desc_gpa = regs.ring_gpa
                + AEROGPU_RING_HEADER_SIZE_BYTES
                + (u64::from(ring.slot_index(head)) * u64::from(ring.entry_stride_bytes));
            let desc = AeroGpuSubmitDesc::read_from(mem, desc_gpa);

            regs.stats.submissions = regs.stats.submissions.saturating_add(1);
            if desc.desc_size_bytes != AeroGpuSubmitDesc::SIZE_BYTES {
                regs.stats.malformed_submissions = regs.stats.malformed_submissions.saturating_add(1);
            }

            if desc.signal_fence > regs.completed_fence {
                regs.completed_fence = desc.signal_fence;
                fence_advanced = true;

                if desc.flags & AeroGpuSubmitDesc::FLAG_NO_IRQ == 0 {
                    fence_irq = true;
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

        if fence_advanced {
            // Always make MMIO-completed-fence observable; the value is read from regs.completed_fence.
            if regs.fence_gpa != 0 {
                // Initialize header (idempotent) and update completed value.
                mem.write_u32(regs.fence_gpa + 0, AEROGPU_FENCE_PAGE_MAGIC);
                mem.write_u32(regs.fence_gpa + 4, regs.abi_version);
                mem.write_u64(regs.fence_gpa + 8, regs.completed_fence);

                // Keep writes within the defined struct size; do not touch the rest of the page.
                let _ = AEROGPU_FENCE_PAGE_SIZE_BYTES;
            }

            if fence_irq {
                regs.irq_status |= irq_bits::FENCE;
            }
        }
    }
}

