use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuAllocEntry as ProtocolAllocEntry, AerogpuAllocTableHeader as ProtocolAllocTableHeader,
    AerogpuRingHeader as ProtocolRingHeader, AerogpuSubmitDesc as ProtocolSubmitDesc,
};
use emulator::devices::aerogpu_regs::{irq_bits, ring_control, AeroGpuRegs};
use emulator::devices::aerogpu_ring::{
    AeroGpuAllocEntry, AeroGpuSubmitDesc, AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC, RING_HEAD_OFFSET,
    RING_TAIL_OFFSET,
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
const SUBMIT_DESC_CONTEXT_ID_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, context_id) as u64;
const SUBMIT_DESC_ENGINE_ID_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, engine_id) as u64;
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
const SUBMIT_DESC_SIGNAL_FENCE_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, signal_fence) as u64;
const SUBMIT_DESC_RESERVED0_OFFSET: u64 = core::mem::offset_of!(ProtocolSubmitDesc, reserved0) as u64;

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
const ALLOC_ENTRY_SIZE_BYTES_OFFSET: u64 = core::mem::offset_of!(ProtocolAllocEntry, size_bytes) as u64;
const ALLOC_ENTRY_RESERVED0_OFFSET: u64 = core::mem::offset_of!(ProtocolAllocEntry, reserved0) as u64;

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
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, alloc_table_gpa);
    mem.write_u32(
        desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET,
        alloc_table_size_bytes,
    );
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_RESERVED0_OFFSET, 0);
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, signal_fence);
    mem.write_u64(desc_gpa + SUBMIT_DESC_RESERVED0_OFFSET, 0);
}

fn write_alloc_table(mem: &mut VecMemory, gpa: u64, abi_version: u32, magic: u32) -> u32 {
    let header_size = AEROGPU_ALLOC_TABLE_HEADER_SIZE_BYTES;
    let entry_stride = AeroGpuAllocEntry::SIZE_BYTES;
    let entry_count = 1u32;
    let size_bytes = header_size + entry_count * entry_stride;

    mem.write_u32(gpa + ALLOC_TABLE_MAGIC_OFFSET, magic);
    mem.write_u32(gpa + ALLOC_TABLE_ABI_VERSION_OFFSET, abi_version);
    mem.write_u32(gpa + ALLOC_TABLE_SIZE_BYTES_OFFSET, size_bytes);
    mem.write_u32(gpa + ALLOC_TABLE_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(gpa + ALLOC_TABLE_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(gpa + ALLOC_TABLE_RESERVED0_OFFSET, 0);

    // entry 0
    let entry_gpa = gpa + u64::from(header_size);
    mem.write_u32(entry_gpa + ALLOC_ENTRY_ALLOC_ID_OFFSET, 1); // alloc_id
    mem.write_u32(entry_gpa + ALLOC_ENTRY_FLAGS_OFFSET, 0); // flags
    mem.write_u64(entry_gpa + ALLOC_ENTRY_GPA_OFFSET, 0x9000); // gpa
    mem.write_u64(entry_gpa + ALLOC_ENTRY_SIZE_BYTES_OFFSET, 0x1000); // size_bytes
    mem.write_u64(entry_gpa + ALLOC_ENTRY_RESERVED0_OFFSET, 0);

    size_bytes
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
