use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC;
use emulator::devices::aerogpu_regs::{irq_bits, ring_control, AeroGpuRegs};
use emulator::devices::aerogpu_ring::{AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_RING_MAGIC};
use emulator::gpu_worker::aerogpu_executor::{
    AeroGpuExecutor, AeroGpuExecutorConfig, AeroGpuFenceCompletionMode,
};
use memory::MemoryBus;

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
    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, 64); // entry_stride_bytes
    mem.write_u32(ring_gpa + 20, 0); // flags
    mem.write_u32(ring_gpa + 24, head);
    mem.write_u32(ring_gpa + 28, tail);
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
    mem.write_u32(desc_gpa + 0, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id

    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, cmd_size_bytes);
    mem.write_u32(desc_gpa + 28, 0);

    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size_bytes);
    mem.write_u32(desc_gpa + 44, 0);

    mem.write_u64(desc_gpa + 48, signal_fence);
    mem.write_u64(desc_gpa + 56, 0);
}

fn write_alloc_table(mem: &mut VecMemory, gpa: u64, abi_version: u32, magic: u32) -> u32 {
    let header_size = 24u32;
    let entry_stride = 32u32;
    let entry_count = 1u32;
    let size_bytes = header_size + entry_count * entry_stride;

    mem.write_u32(gpa + 0, magic);
    mem.write_u32(gpa + 4, abi_version);
    mem.write_u32(gpa + 8, size_bytes);
    mem.write_u32(gpa + 12, entry_count);
    mem.write_u32(gpa + 16, entry_stride);
    mem.write_u32(gpa + 20, 0);

    // entry 0
    let entry_gpa = gpa + u64::from(header_size);
    mem.write_u32(entry_gpa + 0, 1); // alloc_id
    mem.write_u32(entry_gpa + 4, 0); // flags
    mem.write_u64(entry_gpa + 8, 0x9000); // gpa
    mem.write_u64(entry_gpa + 16, 0x1000); // size_bytes
    mem.write_u64(entry_gpa + 24, 0);

    size_bytes
}

fn write_cmd_stream_header(
    mem: &mut VecMemory,
    gpa: u64,
    abi_version: u32,
    size_bytes: u32,
    magic: u32,
) -> u32 {
    mem.write_u32(gpa + 0, magic);
    mem.write_u32(gpa + 4, abi_version);
    mem.write_u32(gpa + 8, size_bytes);
    mem.write_u32(gpa + 12, 0);
    mem.write_u32(gpa + 16, 0);
    mem.write_u32(gpa + 20, 0);
    24
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

    let desc_gpa = ring_gpa + 64;
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

    assert_eq!(mem.read_u32(ring_gpa + 24), 1);
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

    let desc_gpa = ring_gpa + 64;
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

    assert_eq!(mem.read_u32(ring_gpa + 24), 1);
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

    let desc_gpa = ring_gpa + 64;
    write_submit_desc(&mut mem, desc_gpa, cmd_gpa, cmd_size_bytes, 0, 0, 5);

    regs.ring_gpa = ring_gpa;
    regs.ring_size_bytes = ring_size;
    regs.ring_control = ring_control::ENABLE;

    exec.process_doorbell(&mut regs, &mut mem);

    assert_eq!(mem.read_u32(ring_gpa + 24), 1);
    assert_eq!(regs.completed_fence, 5);
    assert_eq!(regs.stats.malformed_submissions, 1);
    assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
}
