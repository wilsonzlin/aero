use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_MINOR;
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuAllocEntry as ProtocolAllocEntry, AerogpuAllocTableHeader as ProtocolAllocTableHeader,
    AerogpuRingHeader as ProtocolRingHeader, AerogpuSubmitDesc as ProtocolSubmitDesc,
};
use emulator::devices::aerogpu_regs::{irq_bits, ring_control, AeroGpuRegs};
use emulator::devices::aerogpu_ring::{
    AeroGpuAllocEntry, AeroGpuSubmitDesc, AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC,
    FENCE_PAGE_COMPLETED_FENCE_OFFSET, RING_HEAD_OFFSET, RING_TAIL_OFFSET,
};
use emulator::gpu_worker::aerogpu_backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend,
};
use emulator::gpu_worker::aerogpu_executor::{
    AeroGpuExecutor, AeroGpuExecutorConfig, AeroGpuFenceCompletionMode,
};
use memory::MemoryBus;

const RING_MAGIC_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, magic) as u64;
const RING_ABI_VERSION_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, abi_version) as u64;
const RING_SIZE_BYTES_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, size_bytes) as u64;
const RING_ENTRY_COUNT_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, entry_count) as u64;
const RING_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolRingHeader, entry_stride_bytes) as u64;
const RING_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolRingHeader, flags) as u64;

const SUBMIT_DESC_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, desc_size_bytes) as u64;
const SUBMIT_DESC_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, flags) as u64;
const SUBMIT_DESC_CONTEXT_ID_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, context_id) as u64;
const SUBMIT_DESC_ENGINE_ID_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, engine_id) as u64;
const SUBMIT_DESC_CMD_GPA_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, cmd_gpa) as u64;
const SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, cmd_size_bytes) as u64;
const SUBMIT_DESC_CMD_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, cmd_reserved0) as u64;
const SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_gpa) as u64;
const SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_size_bytes) as u64;
const SUBMIT_DESC_ALLOC_TABLE_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_reserved0) as u64;
const SUBMIT_DESC_SIGNAL_FENCE_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, signal_fence) as u64;
const SUBMIT_DESC_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, reserved0) as u64;

const ALLOC_TABLE_MAGIC_OFFSET: u64 = core::mem::offset_of!(ProtocolAllocTableHeader, magic) as u64;
const ALLOC_TABLE_ABI_VERSION_OFFSET: u64 =
    core::mem::offset_of!(ProtocolAllocTableHeader, abi_version) as u64;
const ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolAllocTableHeader, size_bytes) as u64;
const ALLOC_TABLE_ENTRY_COUNT_OFFSET: u64 =
    core::mem::offset_of!(ProtocolAllocTableHeader, entry_count) as u64;
const ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolAllocTableHeader, entry_stride_bytes) as u64;
const ALLOC_TABLE_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(ProtocolAllocTableHeader, reserved0) as u64;

const ALLOC_ENTRY_ALLOC_ID_OFFSET: u64 = core::mem::offset_of!(ProtocolAllocEntry, alloc_id) as u64;
const ALLOC_ENTRY_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolAllocEntry, flags) as u64;
const ALLOC_ENTRY_GPA_OFFSET: u64 = core::mem::offset_of!(ProtocolAllocEntry, gpa) as u64;
const ALLOC_ENTRY_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolAllocEntry, size_bytes) as u64;
const ALLOC_ENTRY_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(ProtocolAllocEntry, reserved0) as u64;

const CMD_STREAM_MAGIC_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdStreamHeader, magic) as u64;
const CMD_STREAM_ABI_VERSION_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, abi_version) as u64;
const CMD_STREAM_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes) as u64;
const CMD_STREAM_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdStreamHeader, flags) as u64;
const CMD_STREAM_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, reserved0) as u64;
const CMD_STREAM_RESERVED1_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, reserved1) as u64;

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
    ProtocolCmdStreamHeader::SIZE_BYTES as u32
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
        ProtocolCmdStreamHeader::SIZE_BYTES as u32,
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
    assert_eq!(
        header.size_bytes,
        ProtocolCmdStreamHeader::SIZE_BYTES as u32
    );
    assert_eq!(
        record.submission.cmd_stream.len(),
        ProtocolCmdStreamHeader::SIZE_BYTES
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
fn accepts_unknown_minor_versions_for_submission_headers() {
    let mut mem = VecMemory::new(0x40_000);
    let mut regs = AeroGpuRegs::default();
    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 8,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    // Use an unknown minor version while keeping the same major to validate forward-compat rules.
    let newer_minor = (regs.abi_version & 0xffff_0000) | (AEROGPU_ABI_MINOR + 1);

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
        record.decode_errors.contains(&emulator::gpu_worker::aerogpu_executor::AeroGpuSubmissionDecodeError::AllocTable(
            emulator::gpu_worker::aerogpu_executor::AeroGpuAllocTableDecodeError::DuplicateAllocId,
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
        record.decode_errors.contains(&emulator::gpu_worker::aerogpu_executor::AeroGpuSubmissionDecodeError::AllocTable(
            emulator::gpu_worker::aerogpu_executor::AeroGpuAllocTableDecodeError::AddressOverflow,
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
            record.decode_errors.contains(
                &emulator::gpu_worker::aerogpu_executor::AeroGpuSubmissionDecodeError::CmdStream(
                    emulator::gpu_worker::aerogpu_executor::AeroGpuCmdStreamDecodeError::InconsistentDescriptor,
                )
            ),
            "expected InconsistentDescriptor error, got: {:?}",
            record.decode_errors
        );
    }
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
        record.decode_errors.contains(
            &emulator::gpu_worker::aerogpu_executor::AeroGpuSubmissionDecodeError::CmdStream(
                emulator::gpu_worker::aerogpu_executor::AeroGpuCmdStreamDecodeError::AddressOverflow,
            )
        ),
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

    let record = exec
        .last_submissions
        .back()
        .expect("missing submission record");
    assert!(
        record.decode_errors.contains(
            &emulator::gpu_worker::aerogpu_executor::AeroGpuSubmissionDecodeError::AllocTable(
                emulator::gpu_worker::aerogpu_executor::AeroGpuAllocTableDecodeError::AddressOverflow,
            )
        ),
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
}

#[test]
fn fence_wrap_completion_requires_extended_64bit_fences() {
    /*
     * The AeroGPU v1 ring protocol requires monotonically increasing 64-bit fences.
     *
     * Win7/WDDM 1.1 fences are only 32-bit; when they wrap, the KMD must extend them into a
     * monotonic 64-bit domain so the executor's `desc.signal_fence > last_fence` scheduling keeps
     * progressing.
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
    assert_eq!(
        mem.read_u64(regs.fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        regs.completed_fence
    );
    assert!(regs.completed_fence > u64::from(u32::MAX));
    assert_eq!(regs.stats.malformed_submissions, 0);
}
