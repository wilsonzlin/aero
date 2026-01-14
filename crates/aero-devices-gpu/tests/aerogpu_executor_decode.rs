use aero_devices_gpu::backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend, NullAeroGpuBackend,
};
use aero_devices_gpu::cmd::{
    CMD_HDR_OPCODE_OFFSET, CMD_HDR_SIZE_BYTES_OFFSET, CMD_PRESENT_FLAGS_OFFSET,
    CMD_PRESENT_SCANOUT_ID_OFFSET, CMD_PRESENT_SIZE_BYTES, CMD_STREAM_ABI_VERSION_OFFSET,
    CMD_STREAM_FLAGS_OFFSET, CMD_STREAM_HEADER_SIZE_BYTES, CMD_STREAM_MAGIC_OFFSET,
    CMD_STREAM_RESERVED0_OFFSET, CMD_STREAM_RESERVED1_OFFSET, CMD_STREAM_SIZE_BYTES_OFFSET,
};
use aero_devices_gpu::executor::{
    AeroGpuAllocTableDecodeError, AeroGpuCmdStreamDecodeError, AeroGpuExecutor,
    AeroGpuExecutorConfig, AeroGpuFenceCompletionMode, AeroGpuSubmissionDecodeError,
};
use aero_devices_gpu::regs::{
    irq_bits, ring_control, AeroGpuRegs, AerogpuErrorCode, FEATURE_VBLANK,
};
use aero_devices_gpu::ring::{
    AeroGpuAllocEntry, AeroGpuSubmitDesc, AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC,
    ALLOC_ENTRY_ALLOC_ID_OFFSET, ALLOC_ENTRY_FLAGS_OFFSET, ALLOC_ENTRY_GPA_OFFSET,
    ALLOC_ENTRY_RESERVED0_OFFSET, ALLOC_ENTRY_SIZE_BYTES_OFFSET, ALLOC_TABLE_ABI_VERSION_OFFSET,
    ALLOC_TABLE_ENTRY_COUNT_OFFSET, ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET,
    ALLOC_TABLE_MAGIC_OFFSET, ALLOC_TABLE_RESERVED0_OFFSET, ALLOC_TABLE_SIZE_BYTES_OFFSET,
    FENCE_PAGE_COMPLETED_FENCE_OFFSET, RING_ABI_VERSION_OFFSET, RING_ENTRY_COUNT_OFFSET,
    RING_ENTRY_STRIDE_BYTES_OFFSET, RING_FLAGS_OFFSET, RING_HEAD_OFFSET, RING_MAGIC_OFFSET,
    RING_SIZE_BYTES_OFFSET, RING_TAIL_OFFSET, SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET,
    SUBMIT_DESC_ALLOC_TABLE_RESERVED0_OFFSET, SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET,
    SUBMIT_DESC_CMD_GPA_OFFSET, SUBMIT_DESC_CMD_RESERVED0_OFFSET,
    SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, SUBMIT_DESC_CONTEXT_ID_OFFSET, SUBMIT_DESC_ENGINE_ID_OFFSET,
    SUBMIT_DESC_FLAGS_OFFSET, SUBMIT_DESC_RESERVED0_OFFSET, SUBMIT_DESC_SIGNAL_FENCE_OFFSET,
    SUBMIT_DESC_SIZE_BYTES_OFFSET,
};
use aero_devices_gpu::scanout::AeroGpuFormat;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdOpcode, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_PRESENT_FLAG_VSYNC,
};
use memory::MemoryBus;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let range = self.range(paddr, buf.len());
        buf.copy_from_slice(&self.data[range]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let range = self.range(paddr, buf.len());
        self.data[range].copy_from_slice(buf);
    }
}

#[derive(Clone, Debug, Default)]
struct SparseMemory {
    bytes: BTreeMap<u64, u8>,
}

impl SparseMemory {
    fn new() -> Self {
        Self::default()
    }
}

impl MemoryBus for SparseMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        for (idx, dst) in buf.iter_mut().enumerate() {
            let addr = match paddr.checked_add(idx as u64) {
                Some(v) => v,
                None => {
                    *dst = 0;
                    continue;
                }
            };
            *dst = *self.bytes.get(&addr).unwrap_or(&0);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (idx, src) in buf.iter().copied().enumerate() {
            let Some(addr) = paddr.checked_add(idx as u64) else {
                continue;
            };
            self.bytes.insert(addr, src);
        }
    }
}

fn write_ring(
    mem: &mut VecMemory,
    ring_gpa: u64,
    ring_size: u32,
    entry_count: u32,
    head: u32,
    tail: u32,
    abi_version: u32,
) {
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(
        ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0); // flags
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, head);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, tail);
}

fn write_submit_desc(
    mem: &mut VecMemory,
    desc_gpa: u64,
    cmd_gpa: u64,
    cmd_size_bytes: u32,
    alloc_table_gpa: u64,
    alloc_table_size_bytes: u32,
    signal_fence: u64,
) {
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0);
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_RESERVED0_OFFSET, 0);
    mem.write_u64(
        desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET,
        alloc_table_gpa,
    );
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET,
        alloc_table_size_bytes,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_RESERVED0_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, signal_fence);
    mem.write_u64(desc_gpa + SUBMIT_DESC_RESERVED0_OFFSET, 0);
}

fn write_alloc_table_entries(
    mem: &mut VecMemory,
    gpa: u64,
    abi_version: u32,
    magic: u32,
    entries: &[(u32, u32, u64, u64)],
) -> u32 {
    let header_size = AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES;
    let entry_stride = AeroGpuAllocEntry::SIZE_BYTES;
    let entry_count = u32::try_from(entries.len()).expect("entry_count overflow");
    let size_bytes = header_size + entry_count * entry_stride;

    mem.write_u32(gpa + ALLOC_TABLE_MAGIC_OFFSET, magic);
    mem.write_u32(gpa + ALLOC_TABLE_ABI_VERSION_OFFSET, abi_version);
    mem.write_u32(gpa + ALLOC_TABLE_SIZE_BYTES_OFFSET, size_bytes);
    mem.write_u32(gpa + ALLOC_TABLE_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(gpa + ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(gpa + ALLOC_TABLE_RESERVED0_OFFSET, 0);

    for (idx, (alloc_id, flags, entry_gpa, entry_size_bytes)) in entries.iter().enumerate() {
        let entry_base = gpa + u64::from(header_size) + (idx as u64) * u64::from(entry_stride);
        mem.write_u32(entry_base + ALLOC_ENTRY_ALLOC_ID_OFFSET, *alloc_id);
        mem.write_u32(entry_base + ALLOC_ENTRY_FLAGS_OFFSET, *flags);
        mem.write_u64(entry_base + ALLOC_ENTRY_GPA_OFFSET, *entry_gpa);
        mem.write_u64(
            entry_base + ALLOC_ENTRY_SIZE_BYTES_OFFSET,
            *entry_size_bytes,
        );
        mem.write_u64(entry_base + ALLOC_ENTRY_RESERVED0_OFFSET, 0);
    }

    size_bytes
}

fn write_alloc_table(mem: &mut VecMemory, gpa: u64, abi_version: u32, magic: u32) -> u32 {
    write_alloc_table_entries(mem, gpa, abi_version, magic, &[(1, 0, 0x9000, 0x1000)])
}

fn write_cmd_stream_header(
    mem: &mut VecMemory,
    gpa: u64,
    abi_version: u32,
    size_bytes: u32,
    magic: u32,
) -> u32 {
    mem.write_u32(gpa + CMD_STREAM_MAGIC_OFFSET, magic);
    mem.write_u32(gpa + CMD_STREAM_ABI_VERSION_OFFSET, abi_version);
    mem.write_u32(gpa + CMD_STREAM_SIZE_BYTES_OFFSET, size_bytes);
    mem.write_u32(gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(gpa + CMD_STREAM_RESERVED1_OFFSET, 0);
    CMD_STREAM_HEADER_SIZE_BYTES
}

fn write_vsync_present_cmd_stream(mem: &mut VecMemory, gpa: u64, abi_version: u32) -> u32 {
    let stream_size = CMD_STREAM_HEADER_SIZE_BYTES + CMD_PRESENT_SIZE_BYTES;

    mem.write_u32(gpa + CMD_STREAM_MAGIC_OFFSET, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(gpa + CMD_STREAM_ABI_VERSION_OFFSET, abi_version);
    mem.write_u32(gpa + CMD_STREAM_SIZE_BYTES_OFFSET, stream_size);
    mem.write_u32(gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    let present_gpa = gpa + u64::from(CMD_STREAM_HEADER_SIZE_BYTES);
    mem.write_u32(
        present_gpa + CMD_HDR_OPCODE_OFFSET,
        AerogpuCmdOpcode::Present as u32,
    );
    mem.write_u32(
        present_gpa + CMD_HDR_SIZE_BYTES_OFFSET,
        CMD_PRESENT_SIZE_BYTES,
    );
    mem.write_u32(present_gpa + CMD_PRESENT_SCANOUT_ID_OFFSET, 0);
    mem.write_u32(
        present_gpa + CMD_PRESENT_FLAGS_OFFSET,
        AEROGPU_PRESENT_FLAG_VSYNC,
    );

    stream_size
}

#[derive(Default)]
struct CompletingBackend {
    completed: Vec<AeroGpuBackendCompletion>,
}

impl AeroGpuCommandBackend for CompletingBackend {
    fn reset(&mut self) {
        self.completed.clear();
    }

    fn submit(
        &mut self,
        _mem: &mut dyn MemoryBus,
        submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        // Complete immediately; vblank gating should still block fence advancement.
        self.completed.push(AeroGpuBackendCompletion {
            fence: submission.signal_fence,
            error: None,
        });
        Ok(())
    }

    fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
        self.completed.drain(..).collect()
    }

    fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
        None
    }
}

#[test]
fn decodes_alloc_table_and_cmd_stream_header() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let alloc_table_gpa = 0x5000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        AEROGPU_ALLOC_TABLE_MAGIC,
    );

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        42,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 42);
    assert_eq!(regs.stats.malformed_submissions, 0);
    assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record.decode_errors.is_empty(),
        "unexpected decode errors: {:?}",
        record.decode_errors
    );
    assert_eq!(record.submission.allocs.len(), 1);
    let header = record
        .submission
        .cmd_stream_header
        .as_ref()
        .expect("missing cmd stream header");

    let magic = header.magic;
    assert_eq!(magic, AEROGPU_CMD_STREAM_MAGIC);

    let size_bytes = header.size_bytes;
    assert_eq!(size_bytes, 24);
    assert_eq!(record.submission.cmd_stream.len(), cmd_size_bytes as usize);
}

#[test]
fn ring_pending_exceeds_entry_count_advances_head_to_tail() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    // Corrupt ring state: pending = tail - head = 9 (> entry_count=8).
    // The executor should clamp by advancing head to tail and returning early.
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 9, regs.abi_version);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 9);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_eq!(regs.completed_fence, 0);
    assert_eq!(regs.error_code, AerogpuErrorCode::CmdDecode as u32);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn cmd_buffer_can_exceed_cmd_stream_header_size_bytes() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let alloc_table_gpa = 0x5000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        AEROGPU_ALLOC_TABLE_MAGIC,
    );

    // Forward-compat: `cmd_size_bytes` is the backing buffer size, while the stream header's
    // `size_bytes` is the number of bytes used.
    let cmd_gpa = 0x6000u64;
    write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        CMD_STREAM_HEADER_SIZE_BYTES,
        AEROGPU_CMD_STREAM_MAGIC,
    );
    let cmd_buffer_size_bytes = 4096u32;

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_buffer_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        42,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record.decode_errors.is_empty(),
        "unexpected decode errors: {:?}",
        record.decode_errors
    );
    let header = record
        .submission
        .cmd_stream_header
        .as_ref()
        .expect("missing cmd stream header");
    assert_eq!(header.size_bytes, CMD_STREAM_HEADER_SIZE_BYTES);
    assert_eq!(
        record.submission.cmd_stream.len(),
        CMD_STREAM_HEADER_SIZE_BYTES as usize
    );
}

#[test]
fn alloc_table_entry_with_zero_gpa_decodes() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let alloc_table_gpa = 0x5000u64;
    let alloc_table_size_bytes = write_alloc_table_entries(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        AEROGPU_ALLOC_TABLE_MAGIC,
        &[(1, 0, 0, 0x1000)],
    );

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        55,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 55);
    assert_eq!(regs.stats.malformed_submissions, 0);
    assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record.decode_errors.is_empty(),
        "unexpected decode errors: {:?}",
        record.decode_errors
    );
    assert_eq!(record.submission.allocs.len(), 1);
    assert_eq!(record.submission.allocs[0].gpa, 0);
}

#[test]
fn ring_descriptor_gpa_overflow_sets_error_irq_and_advances_head() {
    let mut mem = SparseMemory::new();
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    // Choose a ring GPA such that `ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES` would overflow.
    let ring_gpa = u64::MAX - (AEROGPU_RING_HEADER_SIZE_BYTES - 1);
    let ring_size = 0x1000u32;

    // Minimal valid ring header.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, 8);
    mem.write_u32(
        ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    // Pending work should be dropped by advancing head to tail.
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
    assert_eq!(regs.error_code, AerogpuErrorCode::Oob as u32);
    assert_eq!(regs.error_count, 1);
    assert_eq!(exec.last_submissions.len(), 0);
}

#[test]
fn accepts_unknown_minor_versions_for_submission_headers() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    // Use an unknown minor version while keeping the same major to validate forward-compat rules.
    let newer_minor = (regs.abi_version & 0xffff_0000) | 999;

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, newer_minor);

    let alloc_table_gpa = 0x5000u64;
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        newer_minor,
        AEROGPU_ALLOC_TABLE_MAGIC,
    );

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes =
        write_cmd_stream_header(&mut mem, cmd_gpa, newer_minor, 24, AEROGPU_CMD_STREAM_MAGIC);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        42,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 42);
    assert_eq!(regs.stats.malformed_submissions, 0);
    assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record.decode_errors.is_empty(),
        "unexpected decode errors: {:?}",
        record.decode_errors
    );
    let header = record
        .submission
        .cmd_stream_header
        .as_ref()
        .expect("missing cmd stream header");
    assert_eq!(header.abi_version, newer_minor);
}

#[test]
fn malformed_alloc_table_sets_error_irq_and_advances_head() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let alloc_table_gpa = 0x5000u64;
    let alloc_table_size_bytes = write_alloc_table(&mut mem, alloc_table_gpa, regs.abi_version, 0);

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        1,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 1);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::BadMagic,
            )),
        "expected BadMagic error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn malformed_alloc_table_abi_version_sets_error_irq_and_records_decode_error() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    // Use an unsupported major version while keeping the rest of the table well-formed.
    let alloc_table_gpa = 0x5000u64;
    let wrong_major = regs.abi_version.wrapping_add(1 << 16);
    let alloc_table_size_bytes = write_alloc_table(
        &mut mem,
        alloc_table_gpa,
        wrong_major,
        AEROGPU_ALLOC_TABLE_MAGIC,
    );

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        2,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 2);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::BadAbiVersion,
            )),
        "expected BadAbiVersion error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn malformed_alloc_table_bad_entry_stride_sets_error_irq_and_records_decode_error() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    // Allocation table header with an entry stride smaller than the required entry prefix.
    let alloc_table_gpa = 0x5000u64;
    let header_size = AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES;
    let bad_stride = AeroGpuAllocEntry::SIZE_BYTES - 1;
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_MAGIC_OFFSET,
        AEROGPU_ALLOC_TABLE_MAGIC,
    );
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_ABI_VERSION_OFFSET,
        regs.abi_version,
    );
    mem.write_u32(alloc_table_gpa + ALLOC_TABLE_SIZE_BYTES_OFFSET, header_size);
    mem.write_u32(alloc_table_gpa + ALLOC_TABLE_ENTRY_COUNT_OFFSET, 0);
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET,
        bad_stride,
    );
    mem.write_u32(alloc_table_gpa + ALLOC_TABLE_RESERVED0_OFFSET, 0);

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        header_size,
        3,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 3);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::BadEntryStride,
            )),
        "expected BadEntryStride error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn malformed_alloc_table_entries_out_of_bounds_sets_error_irq_and_records_decode_error() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    // Declare one entry but provide a header size that does not cover it.
    let alloc_table_gpa = 0x5000u64;
    let header_size = AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES;
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_MAGIC_OFFSET,
        AEROGPU_ALLOC_TABLE_MAGIC,
    );
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_ABI_VERSION_OFFSET,
        regs.abi_version,
    );
    mem.write_u32(alloc_table_gpa + ALLOC_TABLE_SIZE_BYTES_OFFSET, header_size);
    mem.write_u32(alloc_table_gpa + ALLOC_TABLE_ENTRY_COUNT_OFFSET, 1);
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET,
        AeroGpuAllocEntry::SIZE_BYTES,
    );
    mem.write_u32(alloc_table_gpa + ALLOC_TABLE_RESERVED0_OFFSET, 0);

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        header_size,
        4,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 4);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::EntriesOutOfBounds,
            )),
        "expected EntriesOutOfBounds error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn malformed_alloc_table_invalid_entry_sets_error_irq_and_records_decode_error() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    // Entry with alloc_id == 0 is invalid.
    let alloc_table_gpa = 0x5000u64;
    let alloc_table_size_bytes = write_alloc_table_entries(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        AEROGPU_ALLOC_TABLE_MAGIC,
        &[(0, 0, 0x9000, 0x1000)],
    );

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        5,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 5);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::InvalidEntry,
            )),
        "expected InvalidEntry error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn duplicate_alloc_id_in_alloc_table_sets_error_irq() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let alloc_table_gpa = 0x5000u64;
    let alloc_table_size_bytes = write_alloc_table_entries(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        AEROGPU_ALLOC_TABLE_MAGIC,
        &[
            (1, 0, 0x9000, 0x1000),
            // Duplicate alloc_id should be rejected even if other fields differ.
            (1, 0, 0xA000, 0x1000),
        ],
    );

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        7,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 7);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::DuplicateAllocId,
            )),
        "expected DuplicateAllocId error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn alloc_table_entry_address_overflow_sets_error_irq() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let alloc_table_gpa = 0x5000u64;
    let alloc_table_size_bytes = write_alloc_table_entries(
        &mut mem,
        alloc_table_gpa,
        regs.abi_version,
        AEROGPU_ALLOC_TABLE_MAGIC,
        &[(1, 0, u64::MAX - 0x10, 0x100)],
    );

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        alloc_table_gpa,
        alloc_table_size_bytes,
        8,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 8);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::AddressOverflow,
            )),
        "expected AddressOverflow error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn malformed_cmd_stream_size_sets_error_irq() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        0x1000, // header claims more bytes than the desc allows
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, cmd_gpa, cmd_size_bytes, 0, 0, 5);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 5);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::CmdStream(
                AeroGpuCmdStreamDecodeError::StreamSizeTooLarge,
            )),
        "expected StreamSizeTooLarge error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn malformed_cmd_stream_too_small_sets_error_irq_and_records_decode_error() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = CMD_STREAM_HEADER_SIZE_BYTES - 1;

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, cmd_gpa, cmd_size_bytes, 0, 0, 6);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 6);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::CmdStream(
                AeroGpuCmdStreamDecodeError::TooSmall,
            )),
        "expected TooSmall error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn malformed_cmd_stream_bad_header_sets_error_irq_and_records_decode_error() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        0, /* bad magic */
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, cmd_gpa, cmd_size_bytes, 0, 0, 7);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 7);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::CmdStream(
                AeroGpuCmdStreamDecodeError::BadHeader,
            )),
        "expected BadHeader error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn cmd_stream_too_large_sets_error_irq_and_advances_head() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    // Declare a backing buffer size and cmd stream header size that exceed the executor's
    // MAX_CMD_STREAM_SIZE_BYTES guard (64 MiB), without requiring the test to allocate that much.
    let cmd_gpa = 0x6000u64;
    let huge_size_bytes = 64u32 * 1024 * 1024 + 4;
    write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        huge_size_bytes,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, cmd_gpa, huge_size_bytes, 0, 0, 77);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 77);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::CmdStream(
                AeroGpuCmdStreamDecodeError::TooLarge,
            )),
        "expected TooLarge error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn alloc_table_too_large_sets_error_irq_and_advances_head() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    // Declare an alloc table size that exceeds the executor's MAX_ALLOC_TABLE_SIZE_BYTES guard
    // (16 MiB), without requiring the test to allocate that much.
    let alloc_table_gpa = 0x5000u64;
    let huge_size_bytes = 16u32 * 1024 * 1024 + 4;
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_MAGIC_OFFSET,
        AEROGPU_ALLOC_TABLE_MAGIC,
    );
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_ABI_VERSION_OFFSET,
        regs.abi_version,
    );
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_SIZE_BYTES_OFFSET,
        huge_size_bytes,
    );
    mem.write_u32(alloc_table_gpa + ALLOC_TABLE_ENTRY_COUNT_OFFSET, 0);
    mem.write_u32(
        alloc_table_gpa + ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET,
        AeroGpuAllocEntry::SIZE_BYTES,
    );
    mem.write_u32(alloc_table_gpa + ALLOC_TABLE_RESERVED0_OFFSET, 0);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        0,
        0,
        alloc_table_gpa,
        huge_size_bytes,
        88,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 88);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::TooLarge,
            )),
        "expected TooLarge error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn inconsistent_cmd_stream_descriptor_sets_error_irq_and_advances_head() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 2, regs.abi_version);

    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    // cmd_gpa set but cmd_size_bytes=0 must be rejected.
    write_submit_desc(&mut mem, desc0_gpa, 0x6000, 0, 0, 0, 5);

    let desc1_gpa = desc0_gpa + u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    // cmd_size_bytes set but cmd_gpa=0 must also be rejected.
    write_submit_desc(&mut mem, desc1_gpa, 0, 24, 0, 0, 6);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 2);
    assert_eq!(regs.completed_fence, 6);
    assert_eq!(regs.stats.malformed_submissions, 2);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let records: Vec<_> = exec.last_submissions.iter().collect();
    assert_eq!(records.len(), 2);
    for record in records {
        assert!(
            record
                .decode_errors
                .contains(&AeroGpuSubmissionDecodeError::CmdStream(
                    AeroGpuCmdStreamDecodeError::InconsistentDescriptor,
                )),
            "expected InconsistentDescriptor error, got: {:?}",
            record.decode_errors
        );
    }
}

#[test]
fn last_submissions_is_bounded_by_keep_last_submissions() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 2,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });
    exec.set_backend(Box::new(NullAeroGpuBackend::new()));

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 3, regs.abi_version);

    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    for (slot, fence) in [1u64, 2, 3].into_iter().enumerate() {
        let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + (slot as u64) * stride;
        write_submit_desc(&mut mem, desc_gpa, 0, 0, 0, 0, fence);
    }

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 3);
    assert_eq!(exec.last_submissions.len(), 2);

    let records: Vec<_> = exec.last_submissions.iter().collect();
    assert_eq!(records[0].submission.desc.signal_fence, 2);
    assert_eq!(records[1].submission.desc.signal_fence, 3);
    assert_eq!(records[0].ring_head, 1);
    assert_eq!(records[1].ring_head, 2);
}

#[test]
fn cmd_stream_descriptor_address_overflow_sets_error_irq_and_advances_head() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    // cmd_gpa+cmd_size_bytes wraps u64.
    write_submit_desc(&mut mem, desc_gpa, u64::MAX - 8, 24, 0, 0, 9);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 9);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::CmdStream(
                AeroGpuCmdStreamDecodeError::AddressOverflow,
            )),
        "expected AddressOverflow error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn alloc_table_descriptor_address_overflow_sets_error_irq_and_advances_head() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let cmd_gpa = 0x6000u64;
    let cmd_size_bytes = write_cmd_stream_header(
        &mut mem,
        cmd_gpa,
        regs.abi_version,
        24,
        AEROGPU_CMD_STREAM_MAGIC,
    );

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    // alloc_table_gpa+alloc_table_size_bytes wraps u64.
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_size_bytes,
        u64::MAX - 8,
        24,
        10,
    );

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 10);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
    assert_eq!(regs.error_code, AerogpuErrorCode::Oob as u32);
    assert_eq!(regs.error_fence, 10);
    assert_eq!(regs.error_count, 1);

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record
            .decode_errors
            .contains(&AeroGpuSubmissionDecodeError::AllocTable(
                AeroGpuAllocTableDecodeError::AddressOverflow,
            )),
        "expected AddressOverflow error, got: {:?}",
        record.decode_errors
    );
}

#[test]
fn backend_submit_error_does_not_block_fence_completion() {
    #[derive(Default)]
    struct RejectBackend;

    impl AeroGpuCommandBackend for RejectBackend {
        fn reset(&mut self) {}

        fn submit(
            &mut self,
            _mem: &mut dyn MemoryBus,
            submission: AeroGpuBackendSubmission,
        ) -> Result<(), String> {
            Err(format!("rejected fence={}", submission.signal_fence))
        }

        fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
            Vec::new()
        }

        fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
            None
        }
    }

    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::<RejectBackend>::default());

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    // Submit an empty command stream; the backend still receives the submission.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, 0, 0, 0, 0, 7);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 7);
    assert_eq!(regs.stats.gpu_exec_errors, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}

#[test]
fn backend_completion_error_advances_fence_and_sets_error_irq() {
    #[derive(Default)]
    struct ErrorCompletionBackend {
        completed: Vec<AeroGpuBackendCompletion>,
    }

    impl AeroGpuCommandBackend for ErrorCompletionBackend {
        fn reset(&mut self) {
            self.completed.clear();
        }

        fn submit(
            &mut self,
            _mem: &mut dyn MemoryBus,
            submission: AeroGpuBackendSubmission,
        ) -> Result<(), String> {
            self.completed.push(AeroGpuBackendCompletion {
                fence: submission.signal_fence,
                error: Some("simulated backend execution failure".into()),
            });
            Ok(())
        }

        fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
            self.completed.drain(..).collect()
        }

        fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
            None
        }
    }

    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::<ErrorCompletionBackend>::default());

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, 0, 0, 0, 0, 9);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 9);
    assert_eq!(regs.stats.gpu_exec_errors, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
    assert_eq!(regs.error_code, AerogpuErrorCode::Backend as u32);
    assert_eq!(regs.error_fence, 9);
    assert_eq!(regs.error_count, 1);
}

#[test]
fn fence_wrap_completion_requires_extended_64bit_fences() {
    /*
     * The AeroGPU ring protocol uses 64-bit fences.
     *
     * Win7/WDDM 1.1 submission fences are 32-bit; when they wrap, the KMD must extend them into a
     * monotonic 64-bit domain so host-side fence scheduling keeps making progress.
     */
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 2, regs.abi_version);

    // Simulate a 32-bit wrap: 0xFFFF_FFFF -> 0x0000_0000, extended into a 64-bit epoch domain.
    let fence0 = 0x0000_0000_FFFF_FFFFu64;
    let fence1 = 0x0000_0001_0000_0000u64;

    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    let desc1_gpa = desc0_gpa + u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    write_submit_desc(&mut mem, desc0_gpa, 0, 0, 0, 0, fence0);
    write_submit_desc(&mut mem, desc1_gpa, 0, 0, 0, 0, fence1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.fence_gpa = 0x2000u64;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 2);
    assert_eq!(regs.completed_fence, fence1);
    assert!(regs.completed_fence > u64::from(u32::MAX));
    assert_eq!(
        mem.read_u64(regs.fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        regs.completed_fence
    );
    assert_eq!(regs.stats.malformed_submissions, 0);
}

#[test]
fn vsync_present_is_gated_until_vblank_tick_immediate_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    // Ring with one PRESENT submission that signals fence=1.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, stream_size);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    // Vsync-present fence must not complete until vblank tick.
    assert_eq!(regs.completed_fence, 0);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 1);
}

#[test]
fn vsync_present_is_gated_even_without_submit_present_hint_immediate_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    // Ring with one PRESENT submission that signals fence=1, but without the submit-level PRESENT
    // hint flag set.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, stream_size);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    // Vsync-present fence must not complete until vblank tick, even if the KMD failed to set the
    // submit-level PRESENT hint bit.
    assert_eq!(regs.completed_fence, 0);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 1);
}

#[test]
fn pending_vsync_fence_is_flushed_when_scanout_is_disabled_immediate_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);

    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, cmd_gpa, stream_size, 0, 0, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 0);

    // If scanout/vblank pacing is disabled after a vsync present is queued, do not leave the fence
    // blocked forever. The device should flush/publish the completion.
    regs.scanout0.enable = false;
    exec.flush_pending_fences(&mut regs, &mut mem);

    assert_eq!(regs.completed_fence, 1);
}

#[test]
fn vsync_fence_blocks_immediate_fences_behind_it_until_vblank_immediate_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    // Two submissions: fence 1 (vsync) then fence 2 (immediate, empty submission).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 2, regs.abi_version);
    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    let desc_base_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;

    let desc0_gpa = desc_base_gpa;
    write_submit_desc(&mut mem, desc0_gpa, cmd_gpa, stream_size, 0, 0, 1);

    let desc1_gpa = desc_base_gpa + stride;
    // Empty submission (no cmd stream) should be treated as immediate.
    write_submit_desc(&mut mem, desc1_gpa, 0, 0, 0, 0, 2);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(
        regs.completed_fence, 0,
        "immediate fence behind vsync fence must not complete on doorbell"
    );

    exec.process_vblank_tick(&mut regs, &mut mem);

    // Completing the vsync fence should also allow the immediate fence behind it to complete on
    // the same vblank tick.
    assert_eq!(regs.completed_fence, 2);
}

#[test]
fn completes_at_most_one_vsync_fence_per_vblank_tick_immediate_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 2, regs.abi_version);
    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    let desc_base_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;

    let desc0_gpa = desc_base_gpa;
    write_submit_desc(&mut mem, desc0_gpa, cmd_gpa, stream_size, 0, 0, 1);

    let desc1_gpa = desc_base_gpa + stride;
    write_submit_desc(&mut mem, desc1_gpa, cmd_gpa, stream_size, 0, 0, 2);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 0);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 1);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 2);
}

#[test]
fn vblank_irq_is_only_latched_while_enabled() {
    let mut mem = VecMemory::new(0x1000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    regs.irq_enable = 0;
    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    regs.irq_enable = irq_bits::SCANOUT_VBLANK;
    // Should not see a "stale" IRQ from the previous tick; only from this tick.
    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_ne!(regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
}

#[test]
fn flush_pending_fences_unblocks_vsync_fence_when_scanout_disabled_deferred_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs {
        irq_enable: irq_bits::FENCE,
        ..Default::default()
    };
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::<CompletingBackend>::default());

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    // Ring with one PRESENT submission that signals fence=1.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, stream_size);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    // Backend completed immediately, but vsync should gate advancement.
    assert_eq!(regs.completed_fence, 0);

    // Simulate disabling scanout/vblank pacing: unblock.
    regs.scanout0.enable = false;
    exec.flush_pending_fences(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 1);
}

#[test]
fn vsync_present_is_gated_until_vblank_tick_deferred_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::<CompletingBackend>::default());

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    // Ring with one PRESENT submission that signals fence=1.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa);
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, stream_size);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    // Backend completed immediately, but vsync should gate advancement.
    assert_eq!(regs.completed_fence, 0);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 1);
}

#[test]
fn vsync_fence_blocks_immediate_fences_behind_it_until_vblank_deferred_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::<CompletingBackend>::default());

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    // Two submissions: fence 1 (vsync) then fence 2 (immediate, empty submission).
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 2, regs.abi_version);
    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    let desc_base_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;

    let desc0_gpa = desc_base_gpa;
    write_submit_desc(&mut mem, desc0_gpa, cmd_gpa, stream_size, 0, 0, 1);

    let desc1_gpa = desc_base_gpa + stride;
    write_submit_desc(&mut mem, desc1_gpa, 0, 0, 0, 0, 2);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(
        regs.completed_fence, 0,
        "immediate fence behind vsync fence must not complete on doorbell"
    );

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 2);
}

#[test]
fn completes_at_most_one_vsync_fence_per_vblank_tick_deferred_mode() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::<CompletingBackend>::default());

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 2, regs.abi_version);
    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    let desc_base_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;

    let desc0_gpa = desc_base_gpa;
    write_submit_desc(&mut mem, desc0_gpa, cmd_gpa, stream_size, 0, 0, 1);

    let desc1_gpa = desc_base_gpa + stride;
    write_submit_desc(&mut mem, desc1_gpa, cmd_gpa, stream_size, 0, 0, 2);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 0);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 1);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 2);
}

#[test]
fn scanout_writeback_converts_rgba_to_16bpp_formats() {
    // Exercising the scanout writeback path via a deferred present completion.
    #[derive(Default)]
    struct ScanoutBackend {
        completed: Vec<AeroGpuBackendCompletion>,
    }

    impl AeroGpuCommandBackend for ScanoutBackend {
        fn reset(&mut self) {
            self.completed.clear();
        }

        fn submit(
            &mut self,
            _mem: &mut dyn MemoryBus,
            submission: AeroGpuBackendSubmission,
        ) -> Result<(), String> {
            self.completed.push(AeroGpuBackendCompletion {
                fence: submission.signal_fence,
                error: None,
            });
            Ok(())
        }

        fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
            self.completed.drain(..).collect()
        }

        fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
            Some(AeroGpuBackendScanout {
                width: 1,
                height: 1,
                rgba8: vec![255, 0, 0, 255], // opaque red
            })
        }
    }

    let mut mem = VecMemory::new(0x8000);
    let fb_gpa = 0x1000u64;

    let mut regs = AeroGpuRegs::default();
    regs.scanout0.enable = true;
    regs.scanout0.width = 1;
    regs.scanout0.height = 1;
    regs.scanout0.fb_gpa = fb_gpa;
    regs.scanout0.pitch_bytes = 2;
    regs.scanout0.format = AeroGpuFormat::B5G6R5Unorm;

    // Ring with one PRESENT submission (no cmd stream needed for this test).
    let ring_gpa = 0x2000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::<ScanoutBackend>::default());

    // Submit (process_doorbell polls completions and triggers scanout writeback).
    exec.process_doorbell(&mut regs, &mut mem);

    // RGB565: R=31 -> 0xF800.
    assert_eq!(mem.read_u16(fb_gpa), 0xF800);

    // Submit another present for BGRA5551.
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 2);
    let desc2_gpa = desc_gpa + u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    mem.write_u32(
        desc2_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET,
        AeroGpuSubmitDesc::SIZE_BYTES,
    );
    mem.write_u32(
        desc2_gpa + SUBMIT_DESC_FLAGS_OFFSET,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );
    mem.write_u64(desc2_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 2);

    regs.scanout0.format = AeroGpuFormat::B5G5R5A1Unorm;
    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(mem.read_u16(fb_gpa), 0xFC00);
}

#[test]
fn deferred_mode_retains_submission_until_complete_fence_called() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs {
        irq_enable: irq_bits::FENCE,
        ..Default::default()
    };

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::new(NullAeroGpuBackend::new()));

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, 0, 0, 0, 0, 7);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);

    // Deferred mode should not advance fences without an explicit completion.
    assert_eq!(regs.completed_fence, 0);
    assert_eq!(regs.irq_status & irq_bits::FENCE, 0);

    exec.complete_fence(&mut regs, &mut mem, 7);
    assert_eq!(regs.completed_fence, 7);
    assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
}

#[test]
fn deferred_mode_advances_completed_fence_in_order_even_with_out_of_order_completions() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs {
        irq_enable: irq_bits::FENCE,
        ..Default::default()
    };

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::new(NullAeroGpuBackend::new()));

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 3, regs.abi_version);

    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    let desc_base_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    let desc0_gpa = desc_base_gpa;
    let desc1_gpa = desc_base_gpa + stride;
    let desc2_gpa = desc_base_gpa + 2 * stride;
    write_submit_desc(&mut mem, desc0_gpa, 0, 0, 0, 0, 1);
    write_submit_desc(&mut mem, desc1_gpa, 0, 0, 0, 0, 2);
    write_submit_desc(&mut mem, desc2_gpa, 0, 0, 0, 0, 3);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 3);
    assert_eq!(regs.completed_fence, 0);

    // Complete fence 2 out-of-order: must not advance without fence 1.
    exec.complete_fence(&mut regs, &mut mem, 2);
    assert_eq!(regs.completed_fence, 0);
    assert_eq!(regs.irq_status & irq_bits::FENCE, 0);

    // Completing fence 1 should allow advancement up to 2 (since fence 2 is already complete).
    exec.complete_fence(&mut regs, &mut mem, 1);
    assert_eq!(regs.completed_fence, 2);
    assert_ne!(regs.irq_status & irq_bits::FENCE, 0);

    // Clear the IRQ bit and ensure completing fence 3 raises again.
    regs.irq_status = 0;
    exec.complete_fence(&mut regs, &mut mem, 3);
    assert_eq!(regs.completed_fence, 3);
    assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
}

#[test]
fn deferred_mode_applies_completion_received_before_submit_for_immediate_fence() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs {
        irq_enable: irq_bits::FENCE,
        ..Default::default()
    };

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::new(NullAeroGpuBackend::new()));

    // Completion arrives before the doorbell consumes the submit descriptor.
    exec.complete_fence(&mut regs, &mut mem, 7);
    assert_eq!(regs.completed_fence, 0);

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, 0, 0, 0, 0, 7);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);
    assert_eq!(regs.completed_fence, 7);
    assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
}

#[test]
fn deferred_mode_does_not_bypass_vsync_gating_for_completion_received_before_submit() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs {
        irq_enable: irq_bits::FENCE,
        ..Default::default()
    };
    regs.features |= FEATURE_VBLANK;
    regs.scanout0.enable = true;

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    });
    exec.set_backend(Box::new(NullAeroGpuBackend::new()));

    // Fence completion arrives before the submission is even processed.
    exec.complete_fence(&mut regs, &mut mem, 1);
    assert_eq!(regs.completed_fence, 0);

    let cmd_gpa = 0x6000u64;
    let stream_size = write_vsync_present_cmd_stream(&mut mem, cmd_gpa, regs.abi_version);

    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    write_ring(&mut mem, ring_gpa, ring_size, 8, 0, 1, regs.abi_version);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    write_submit_desc(&mut mem, desc_gpa, cmd_gpa, stream_size, 0, 0, 1);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);

    // Even though the fence was already "complete", vsync gating must still delay completion until vblank.
    assert_eq!(regs.completed_fence, 0);
    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 1);
}
