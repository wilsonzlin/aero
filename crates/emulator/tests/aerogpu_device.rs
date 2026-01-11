use std::time::{Duration, Instant};

use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdPresent as ProtocolCmdPresent,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_PRESENT_FLAG_VSYNC,
};
use aero_protocol::aerogpu::aerogpu_pci::{AEROGPU_ABI_MAJOR, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuRingHeader as ProtocolRingHeader, AerogpuSubmitDesc as ProtocolSubmitDesc,
};
use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_ring as ring};
use emulator::devices::aerogpu_regs::{irq_bits, mmio, ring_control, AEROGPU_MMIO_MAGIC};
use emulator::devices::aerogpu_ring::{
    AeroGpuRingHeader, AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_FENCE_PAGE_SIZE_BYTES,
    AEROGPU_RING_HEADER_SIZE_BYTES, AEROGPU_RING_MAGIC, FENCE_PAGE_COMPLETED_FENCE_OFFSET,
    FENCE_PAGE_MAGIC_OFFSET, RING_HEAD_OFFSET, RING_TAIL_OFFSET,
};
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::gpu_worker::aerogpu_backend::ImmediateAeroGpuBackend;
use emulator::gpu_worker::aerogpu_executor::{AeroGpuExecutorConfig, AeroGpuFenceCompletionMode};
use emulator::io::pci::MmioDevice;
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
const SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_gpa) as u64;
const SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, alloc_table_size_bytes) as u64;
const SUBMIT_DESC_SIGNAL_FENCE_OFFSET: u64 =
    core::mem::offset_of!(ProtocolSubmitDesc, signal_fence) as u64;

const CMD_STREAM_MAGIC_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, magic) as u64;
const CMD_STREAM_ABI_VERSION_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, abi_version) as u64;
const CMD_STREAM_SIZE_BYTES_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes) as u64;
const CMD_STREAM_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdStreamHeader, flags) as u64;
const CMD_STREAM_RESERVED0_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, reserved0) as u64;
const CMD_STREAM_RESERVED1_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdStreamHeader, reserved1) as u64;

const CMD_HDR_OPCODE_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdHdr, opcode) as u64;
const CMD_HDR_SIZE_BYTES_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdHdr, size_bytes) as u64;

const CMD_PRESENT_SCANOUT_ID_OFFSET: u64 =
    core::mem::offset_of!(ProtocolCmdPresent, scanout_id) as u64;
const CMD_PRESENT_FLAGS_OFFSET: u64 = core::mem::offset_of!(ProtocolCmdPresent, flags) as u64;

const CMD_STREAM_HEADER_SIZE_BYTES: u64 = ProtocolCmdStreamHeader::SIZE_BYTES as u64;
const CMD_PRESENT_SIZE_BYTES: u32 = core::mem::size_of::<ProtocolCmdPresent>() as u32;

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

#[test]
fn doorbell_updates_ring_head_fence_page_and_irq() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    assert_eq!(dev.mmio_read(&mut mem, mmio::MAGIC, 4), AEROGPU_MMIO_MAGIC);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Ring header.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 0); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + RING_HEAD_OFFSET);
    assert_eq!(head_after, 1);

    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), AEROGPU_FENCE_PAGE_MAGIC);
    assert_eq!(mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET), 42);

    dev.mmio_write(&mut mem, mmio::IRQ_ACK, 4, irq_bits::FENCE);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());
}

#[test]
fn doorbell_accepts_newer_minor_abi_version() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Ring header: advertise an unknown minor version while keeping the same major.
    // The protocol versioning rules require consumers to accept forward-compatible minor
    // extensions and ignore what they don't understand.
    let newer_minor = (AEROGPU_ABI_MAJOR << 16) | 999;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, newer_minor);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, 0); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, 0); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, 0); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    assert_eq!(dev.regs.stats.malformed_submissions, 0);
    assert_eq!(dev.regs.completed_fence, 42);
    assert_eq!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 1);

    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), AEROGPU_FENCE_PAGE_MAGIC);
    assert_eq!(mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET), 42);
}

#[test]
fn doorbell_accepts_larger_submit_desc_stride_and_size() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 128u32;

    // Ring header.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 2); // tail

    // Submit descriptors at slots 0 and 1 with an extended size.
    // Slot 1 is deliberately placed at header + entry_stride to verify the device uses
    // `entry_stride_bytes` when walking the ring.
    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc0_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, 128); // desc_size_bytes
    mem.write_u64(desc0_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 41); // signal_fence

    let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_stride);
    mem.write_u32(desc1_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, 128); // desc_size_bytes
    mem.write_u64(desc1_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    assert_eq!(dev.regs.stats.malformed_submissions, 0);
    assert_eq!(dev.regs.stats.submissions, 2);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 2);
    assert_eq!(dev.regs.completed_fence, 42);
}

#[test]
fn doorbell_rejects_unknown_major_abi_version() {
    let mut mem = VecMemory::new(0x20_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Ring header: advertise an unsupported major version.
    let unsupported_major = ((AEROGPU_ABI_MAJOR + 1) << 16) | 0;
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, unsupported_major);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Submit descriptor at slot 0 (should not be processed due to ABI mismatch).
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES); // desc_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::ERROR);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
    assert!(dev.irq_level());

    // Ring and fence state should remain unchanged.
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(mem.read_u32(ring_gpa + RING_HEAD_OFFSET), 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);
}

#[test]
fn ring_abi_matches_c_header() {
    assert_eq!(AEROGPU_RING_MAGIC, 0x474E_5241);
    assert_eq!(AEROGPU_ABI_VERSION_U32, 0x0001_0001);
    assert_eq!(AEROGPU_RING_HEADER_SIZE_BYTES, 64);
    assert_eq!(RING_HEAD_OFFSET, 24);
    assert_eq!(RING_TAIL_OFFSET, 28);

    assert_eq!(AEROGPU_FENCE_PAGE_MAGIC, 0x434E_4546);
    assert_eq!(AEROGPU_FENCE_PAGE_SIZE_BYTES, 56);
    assert_eq!(FENCE_PAGE_COMPLETED_FENCE_OFFSET, 8);
}

#[test]
fn ring_header_validation_checks_magic_and_abi_version() {
    let mut mem = VecMemory::new(0x20_000);
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    // Start with a header that is valid in every way except the field under test.
    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 0);

    let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
    assert!(!ring.is_valid(ring_size), "wrong magic must be rejected");

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, 0);

    let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
    assert!(!ring.is_valid(ring_size), "wrong ABI version must be rejected");

    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, AEROGPU_ABI_VERSION_U32);

    let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
    assert!(ring.is_valid(ring_size));
}

#[test]
fn scanout_bgra_converts_to_rgba() {
    let mut mem = VecMemory::new(0x20_000);
    let dev = &mut AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    let fb_gpa = 0x5000u64;
    // 2x1 pixels, BGRA: (R=1,G=2,B=3,A=4), (R=10,G=20,B=30,A=40).
    mem.write_physical(fb_gpa, &[3, 2, 1, 4, 30, 20, 10, 40]);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 2);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::SCANOUT0_FORMAT,
        4,
        AeroGpuFormat::B8G8R8A8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 8);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, fb_gpa as u32);
    dev.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_HI, 4, (fb_gpa >> 32) as u32);

    let rgba = dev.read_scanout0_rgba(&mut mem).unwrap();
    assert_eq!(rgba, vec![1, 2, 3, 4, 10, 20, 30, 40]);
}

#[test]
fn vblank_tick_sets_irq_status() {
    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vblank_hz = Some(10);
    let mut mem = VecMemory::new(0x1000);
    let mut dev = AeroGpuPciDevice::new(cfg, 0);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK);

    let t0 = Instant::now();
    dev.tick(&mut mem, t0);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    dev.tick(&mut mem, t0 + Duration::from_millis(100));
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}

#[test]
fn vsynced_present_fence_completes_on_vblank() {
    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vblank_hz = Some(10);

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = AeroGpuPciDevice::new(cfg, 0);

    // Enable scanout so vblank ticks run.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command buffer: ACMD header + PRESENT(vsync).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32 + CMD_PRESENT_SIZE_BYTES;

    mem.write_u32(cmd_gpa + CMD_STREAM_MAGIC_OFFSET, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(cmd_gpa + CMD_STREAM_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(cmd_gpa + CMD_STREAM_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(cmd_gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    let present_gpa = cmd_gpa + CMD_STREAM_HEADER_SIZE_BYTES;
    mem.write_u32(present_gpa + CMD_HDR_OPCODE_OFFSET, AerogpuCmdOpcode::Present as u32);
    mem.write_u32(present_gpa + CMD_HDR_SIZE_BYTES_OFFSET, CMD_PRESENT_SIZE_BYTES);
    mem.write_u32(present_gpa + CMD_PRESENT_SCANOUT_ID_OFFSET, 0);
    mem.write_u32(present_gpa + CMD_PRESENT_FLAGS_OFFSET, AEROGPU_PRESENT_FLAG_VSYNC);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, AeroGpuSubmitDesc::FLAG_PRESENT); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + RING_HEAD_OFFSET);
    assert_eq!(head_after, 1);

    let t0 = Instant::now();
    dev.tick(&mut mem, t0);
    assert_eq!(dev.regs.completed_fence, 0);

    dev.tick(&mut mem, t0 + Duration::from_millis(100));

    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        42
    );
}

#[test]
fn vsynced_present_fence_completes_on_vblank_with_deferred_backend() {
    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vblank_hz = Some(10);
    cfg.executor = AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Deferred,
    };

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = AeroGpuPciDevice::new(cfg, 0);
    dev.set_backend(Box::new(ImmediateAeroGpuBackend::new()));

    // Enable scanout so vblank ticks run.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command buffer: ACMD header + PRESENT(vsync).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32 + CMD_PRESENT_SIZE_BYTES;

    mem.write_u32(cmd_gpa + CMD_STREAM_MAGIC_OFFSET, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(cmd_gpa + CMD_STREAM_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(cmd_gpa + CMD_STREAM_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(cmd_gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    let present_gpa = cmd_gpa + CMD_STREAM_HEADER_SIZE_BYTES;
    mem.write_u32(present_gpa + CMD_HDR_OPCODE_OFFSET, AerogpuCmdOpcode::Present as u32);
    mem.write_u32(present_gpa + CMD_HDR_SIZE_BYTES_OFFSET, CMD_PRESENT_SIZE_BYTES);
    mem.write_u32(present_gpa + CMD_PRESENT_SCANOUT_ID_OFFSET, 0);
    mem.write_u32(present_gpa + CMD_PRESENT_FLAGS_OFFSET, AEROGPU_PRESENT_FLAG_VSYNC);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, AeroGpuSubmitDesc::FLAG_PRESENT); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Backend completes immediately, but vsynced presents should still wait until vblank.
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(!dev.irq_level());

    let head_after = mem.read_u32(ring_gpa + RING_HEAD_OFFSET);
    assert_eq!(head_after, 1);

    let t0 = Instant::now();
    dev.tick(&mut mem, t0);
    assert_eq!(dev.regs.completed_fence, 0);

    dev.tick(&mut mem, t0 + Duration::from_millis(100));

    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
    assert!(dev.irq_level());

    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        42
    );
}

#[test]
fn vsynced_present_does_not_complete_on_catchup_vblank_before_submission() {
    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vblank_hz = Some(10);

    let mut mem = VecMemory::new(0x40_000);
    let mut dev = AeroGpuPciDevice::new(cfg, 0);

    // Enable scanout so vblank ticks run.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::FENCE);

    // Simulate the host not calling `tick()` for a while by creating a vblank schedule anchored in
    // the past. This makes `next_vblank` < Instant::now() before we submit work.
    let past = Instant::now() - Duration::from_millis(500);
    dev.tick(&mut mem, past);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = AeroGpuSubmitDesc::SIZE_BYTES;

    mem.write_u32(ring_gpa + RING_MAGIC_OFFSET, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + RING_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(ring_gpa + RING_SIZE_BYTES_OFFSET, ring_size);
    mem.write_u32(ring_gpa + RING_ENTRY_COUNT_OFFSET, entry_count);
    mem.write_u32(ring_gpa + RING_ENTRY_STRIDE_BYTES_OFFSET, entry_stride);
    mem.write_u32(ring_gpa + RING_FLAGS_OFFSET, 0);
    mem.write_u32(ring_gpa + RING_HEAD_OFFSET, 0); // head
    mem.write_u32(ring_gpa + RING_TAIL_OFFSET, 1); // tail

    // Command buffer: ACMD header + PRESENT(vsync).
    let cmd_gpa = 0x4000u64;
    let cmd_size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32 + CMD_PRESENT_SIZE_BYTES;

    mem.write_u32(cmd_gpa + CMD_STREAM_MAGIC_OFFSET, AEROGPU_CMD_STREAM_MAGIC);
    mem.write_u32(cmd_gpa + CMD_STREAM_ABI_VERSION_OFFSET, dev.regs.abi_version);
    mem.write_u32(cmd_gpa + CMD_STREAM_SIZE_BYTES_OFFSET, cmd_size_bytes);
    mem.write_u32(cmd_gpa + CMD_STREAM_FLAGS_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED0_OFFSET, 0);
    mem.write_u32(cmd_gpa + CMD_STREAM_RESERVED1_OFFSET, 0);

    let present_gpa = cmd_gpa + CMD_STREAM_HEADER_SIZE_BYTES;
    mem.write_u32(present_gpa + CMD_HDR_OPCODE_OFFSET, AerogpuCmdOpcode::Present as u32);
    mem.write_u32(present_gpa + CMD_HDR_SIZE_BYTES_OFFSET, CMD_PRESENT_SIZE_BYTES);
    mem.write_u32(present_gpa + CMD_PRESENT_SCANOUT_ID_OFFSET, 0);
    mem.write_u32(present_gpa + CMD_PRESENT_FLAGS_OFFSET, AEROGPU_PRESENT_FLAG_VSYNC);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES;
    mem.write_u32(desc_gpa + SUBMIT_DESC_SIZE_BYTES_OFFSET, AeroGpuSubmitDesc::SIZE_BYTES); // desc_size_bytes
    mem.write_u32(desc_gpa + SUBMIT_DESC_FLAGS_OFFSET, AeroGpuSubmitDesc::FLAG_PRESENT); // flags
    mem.write_u32(desc_gpa + SUBMIT_DESC_CONTEXT_ID_OFFSET, 0); // context_id
    mem.write_u32(desc_gpa + SUBMIT_DESC_ENGINE_ID_OFFSET, 0); // engine_id
    mem.write_u64(desc_gpa + SUBMIT_DESC_CMD_GPA_OFFSET, cmd_gpa); // cmd_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_CMD_SIZE_BYTES_OFFSET, cmd_size_bytes); // cmd_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_GPA_OFFSET, 0); // alloc_table_gpa
    mem.write_u32(desc_gpa + SUBMIT_DESC_ALLOC_TABLE_SIZE_BYTES_OFFSET, 0); // alloc_table_size_bytes
    mem.write_u64(desc_gpa + SUBMIT_DESC_SIGNAL_FENCE_OFFSET, 42); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    // DOORBELL internally catches up the vblank clock to Instant::now() *before* queuing fences.
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    // Catch-up vblanks that happened before the submission must not complete a vsynced present.
    let now = Instant::now();
    dev.tick(&mut mem, now);
    assert_eq!(dev.regs.completed_fence, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);

    // The next vblank after the submission should complete the fence.
    dev.tick(&mut mem, now + Duration::from_millis(100));
    assert_eq!(dev.regs.completed_fence, 42);
    assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
}

#[test]
fn scanout_disable_stops_vblank_and_clears_pending_irq() {
    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vblank_hz = Some(10);
    let mut mem = VecMemory::new(0x1000);
    let mut dev = AeroGpuPciDevice::new(cfg, 0);

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK);

    let t0 = Instant::now();
    dev.tick(&mut mem, t0);
    dev.tick(&mut mem, t0 + Duration::from_millis(100));

    let seq_before_disable = dev.regs.scanout0_vblank_seq;
    assert_ne!(seq_before_disable, 0);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());

    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    dev.tick(&mut mem, t0 + Duration::from_millis(200));
    assert_eq!(dev.regs.scanout0_vblank_seq, seq_before_disable);

    // Re-enable scanout and tick before the next period: should not generate a "stale" vblank.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    dev.tick(&mut mem, t0 + Duration::from_millis(250));
    assert_eq!(dev.regs.scanout0_vblank_seq, seq_before_disable);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    dev.tick(&mut mem, t0 + Duration::from_millis(350));
    assert!(dev.regs.scanout0_vblank_seq > seq_before_disable);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_bits().to_le_bytes());
}

fn fnv1a32(s: &str) -> u32 {
    let mut hash = 2166136261u32;
    for b in s.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    hash
}

fn read_pixel_bgra(mem: &mut dyn MemoryBus, base_gpa: u64, pitch_bytes: u32, x: u32, y: u32) -> u32 {
    let addr = base_gpa + (y as u64) * (pitch_bytes as u64) + (x as u64) * 4;
    mem.read_u32(addr)
}

#[test]
fn cmd_exec_d3d9_triangle_renders_to_guest_memory() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size = ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1); // entry_count
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0); // reserved0
    // entry 0
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0); // flags
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0); // reserved0
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1); // texture_handle
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1); // AEROGPU_FORMAT_B8G8R8A8_UNORM
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1); // mip_levels
    push_u32(&mut stream, 1); // array_layers
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1); // backing_alloc_id
    push_u32(&mut stream, 0); // backing_offset_bytes
    push_u64(&mut stream, 0); // reserved0

    // CREATE_BUFFER (handle 2) host-allocated.
    const VTX_STRIDE_D3D9: u32 = 20;
    const VERTS_D3D9: usize = 3;
    let vb_bytes = (VTX_STRIDE_D3D9 as usize) * VERTS_D3D9;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2); // buffer_handle
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_bytes as u64);
    push_u32(&mut stream, 0); // backing_alloc_id
    push_u32(&mut stream, 0); // backing_offset_bytes
    push_u64(&mut stream, 0); // reserved0

    // UPLOAD_RESOURCE into VB.
    let mut vb_payload = Vec::with_capacity(vb_bytes);
    // Vertex format: {x,y,z,rhw,color_argb}.
    let green_argb = 0xFF00FF00u32;
    let w = rt_width as f32;
    let h = rt_height as f32;
    for (x, y) in [(w * 0.25, h * 0.25), (w * 0.75, h * 0.25), (w * 0.5, h * 0.75)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.5);
        push_f32(&mut vb_payload, 1.0);
        push_u32(&mut vb_payload, green_argb);
    }
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2); // resource_handle
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0); // offset_bytes
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // SET_RENDER_TARGETS: RT0 = texture 1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1); // color_count
    push_u32(&mut stream, 0); // depth_stencil
    push_u32(&mut stream, 1); // colors[0]
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT full RT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS slot 0 -> buffer 2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 16 + 16);
    push_u32(&mut stream, 0); // start_slot
    push_u32(&mut stream, 1); // buffer_count
    push_u32(&mut stream, 2); // binding.buffer
    push_u32(&mut stream, VTX_STRIDE_D3D9);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY TRIANGLELIST.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4); // AEROGPU_TOPOLOGY_TRIANGLELIST
    push_u32(&mut stream, 0);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3); // vertex_count
    push_u32(&mut stream, 1); // instance_count
    push_u32(&mut stream, 0); // first_vertex
    push_u32(&mut stream, 0); // first_instance

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0); // head
    mem.write_u32(ring_gpa + 28, 1); // tail

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa + 0, 64); // desc_size_bytes
    mem.write_u32(desc_gpa + 4, 0); // flags
    mem.write_u32(desc_gpa + 8, 0); // context_id
    mem.write_u32(desc_gpa + 12, 0); // engine_id
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 1); // signal_fence

    // Fence page.
    let fence_gpa = 0x3000u64;
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
    dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    assert_eq!(dev.regs.completed_fence, 1);
    assert_eq!(mem.read_u32(fence_gpa + 0), AEROGPU_FENCE_PAGE_MAGIC);

    let center = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, rt_width / 2, rt_height / 2);
    let corner = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 5, 5);
    assert_eq!(center & 0x00FF_FFFF, 0x0000_FF00); // green
    assert_eq!(corner & 0x00FF_FFFF, 0x00FF_0000); // red
}

#[test]
fn cmd_exec_d3d11_input_layout_triangle_renders_to_guest_memory() {
    let mut mem = VecMemory::new(0x40_000);
    let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0);

    // Ring layout in guest memory.
    let ring_gpa = 0x1000u64;
    let ring_size = 0x1000u32;
    let entry_count = 8u32;
    let entry_stride = 64u32;

    // Backing allocation for the render target texture (64x64 BGRA).
    let rt_width = 64u32;
    let rt_height = 64u32;
    let rt_pitch = rt_width * 4;
    let rt_bytes = (rt_pitch * rt_height) as u64;
    let rt_alloc_gpa = 0x6000u64;

    // Allocation table (single entry).
    let alloc_table_gpa = 0x4000u64;
    let alloc_table_size = ring::AerogpuAllocTableHeader::SIZE_BYTES + ring::AerogpuAllocEntry::SIZE_BYTES;
    let mut alloc_table = Vec::with_capacity(alloc_table_size);
    push_u32(&mut alloc_table, ring::AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut alloc_table, dev.regs.abi_version);
    push_u32(&mut alloc_table, alloc_table_size as u32);
    push_u32(&mut alloc_table, 1); // entry_count
    push_u32(&mut alloc_table, ring::AerogpuAllocEntry::SIZE_BYTES as u32);
    push_u32(&mut alloc_table, 0);
    push_u32(&mut alloc_table, 1); // alloc_id
    push_u32(&mut alloc_table, 0);
    push_u64(&mut alloc_table, rt_alloc_gpa);
    push_u64(&mut alloc_table, rt_bytes);
    push_u64(&mut alloc_table, 0);
    mem.write_physical(alloc_table_gpa, &alloc_table);

    // Vertex buffer payload: POSITION(float2) + COLOR(float4).
    const VTX_STRIDE_D3D11: u32 = 24;
    let mut vb_payload = Vec::with_capacity(3 * (VTX_STRIDE_D3D11 as usize));
    for (x, y) in [(-0.5f32, -0.5f32), (0.0f32, 0.5f32), (0.5f32, -0.5f32)] {
        push_f32(&mut vb_payload, x);
        push_f32(&mut vb_payload, y);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
        push_f32(&mut vb_payload, 0.0);
        push_f32(&mut vb_payload, 1.0);
    }

    // Input layout blob (ILAY).
    let mut ilay = Vec::new();
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
    push_u32(&mut ilay, cmd::AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
    push_u32(&mut ilay, 2); // element_count
    push_u32(&mut ilay, 0);

    // POSITION
    push_u32(&mut ilay, fnv1a32("POSITION"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 16); // DXGI_FORMAT_R32G32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 0); // offset 0
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);
    // COLOR
    push_u32(&mut ilay, fnv1a32("COLOR"));
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 2); // DXGI_FORMAT_R32G32B32A32_FLOAT
    push_u32(&mut ilay, 0); // slot 0
    push_u32(&mut ilay, 8); // offset 8
    push_u32(&mut ilay, 0);
    push_u32(&mut ilay, 0);

    // Command stream.
    let cmd_gpa = 0x2000u64;
    let mut stream = Vec::new();
    push_u32(&mut stream, cmd::AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut stream, dev.regs.abi_version);
    push_u32(&mut stream, 0); // size_bytes placeholder
    push_u32(&mut stream, 0); // flags
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // CREATE_TEXTURE2D (handle 1) backed by alloc_id=1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateTexture2d as u32);
    push_u32(&mut stream, 56);
    push_u32(&mut stream, 1); // texture_handle
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
    push_u32(&mut stream, 1); // AEROGPU_FORMAT_B8G8R8A8_UNORM
    push_u32(&mut stream, rt_width);
    push_u32(&mut stream, rt_height);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, rt_pitch);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // CREATE_BUFFER (handle 2) host-allocated.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateBuffer as u32);
    push_u32(&mut stream, 40);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, cmd::AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
    push_u64(&mut stream, vb_payload.len() as u64);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);

    // UPLOAD_RESOURCE into VB.
    let upload_size_no_pad = 32 + vb_payload.len();
    let upload_size = (upload_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::UploadResource as u32);
    push_u32(&mut stream, upload_size as u32);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, 0);
    push_u64(&mut stream, 0);
    push_u64(&mut stream, vb_payload.len() as u64);
    stream.extend_from_slice(&vb_payload);
    stream.resize(stream.len() + (upload_size - upload_size_no_pad), 0);

    // CREATE_INPUT_LAYOUT (handle 3) with ILAY blob.
    let ilay_pkt_size_no_pad = 20 + ilay.len();
    let ilay_pkt_size = (ilay_pkt_size_no_pad + 3) & !3;
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::CreateInputLayout as u32);
    push_u32(&mut stream, ilay_pkt_size as u32);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, ilay.len() as u32);
    push_u32(&mut stream, 0);
    stream.extend_from_slice(&ilay);
    stream.resize(stream.len() + (ilay_pkt_size - ilay_pkt_size_no_pad), 0);

    // SET_INPUT_LAYOUT = 3.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetInputLayout as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 0);

    // SET_RENDER_TARGETS: RT0 = texture 1.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetRenderTargets as u32);
    push_u32(&mut stream, 48);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    for _ in 1..cmd::AEROGPU_MAX_RENDER_TARGETS {
        push_u32(&mut stream, 0);
    }

    // SET_VIEWPORT full RT.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetViewport as u32);
    push_u32(&mut stream, 32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, rt_width as f32);
    push_f32(&mut stream, rt_height as f32);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);

    // SET_VERTEX_BUFFERS slot 0 -> buffer 2.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetVertexBuffers as u32);
    push_u32(&mut stream, 32);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 2);
    push_u32(&mut stream, VTX_STRIDE_D3D11);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // SET_PRIMITIVE_TOPOLOGY TRIANGLELIST.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::SetPrimitiveTopology as u32);
    push_u32(&mut stream, 16);
    push_u32(&mut stream, 4);
    push_u32(&mut stream, 0);

    // CLEAR red.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Clear as u32);
    push_u32(&mut stream, 36);
    push_u32(&mut stream, cmd::AEROGPU_CLEAR_COLOR);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 0.0);
    push_f32(&mut stream, 1.0);
    push_f32(&mut stream, 1.0);
    push_u32(&mut stream, 0);

    // DRAW 3 verts.
    push_u32(&mut stream, cmd::AerogpuCmdOpcode::Draw as u32);
    push_u32(&mut stream, 24);
    push_u32(&mut stream, 3);
    push_u32(&mut stream, 1);
    push_u32(&mut stream, 0);
    push_u32(&mut stream, 0);

    // Patch stream size.
    let stream_size = stream.len() as u32;
    stream[8..12].copy_from_slice(&stream_size.to_le_bytes());
    mem.write_physical(cmd_gpa, &stream);

    // Ring header.
    mem.write_u32(ring_gpa + 0, AEROGPU_RING_MAGIC);
    mem.write_u32(ring_gpa + 4, dev.regs.abi_version);
    mem.write_u32(ring_gpa + 8, ring_size);
    mem.write_u32(ring_gpa + 12, entry_count);
    mem.write_u32(ring_gpa + 16, entry_stride);
    mem.write_u32(ring_gpa + 20, 0);
    mem.write_u32(ring_gpa + 24, 0);
    mem.write_u32(ring_gpa + 28, 1);

    // Submit descriptor at slot 0.
    let desc_gpa = ring_gpa + 64;
    mem.write_u32(desc_gpa + 0, 64);
    mem.write_u32(desc_gpa + 4, 0);
    mem.write_u32(desc_gpa + 8, 0);
    mem.write_u32(desc_gpa + 12, 0);
    mem.write_u64(desc_gpa + 16, cmd_gpa);
    mem.write_u32(desc_gpa + 24, stream_size);
    mem.write_u64(desc_gpa + 32, alloc_table_gpa);
    mem.write_u32(desc_gpa + 40, alloc_table_size as u32);
    mem.write_u64(desc_gpa + 48, 2);

    dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
    dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
    dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, ring_size);
    dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);
    dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);

    assert_eq!(dev.regs.completed_fence, 2);

    let center = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, rt_width / 2, rt_height / 2);
    let corner = read_pixel_bgra(&mut mem, rt_alloc_gpa, rt_pitch, 0, 0);
    assert_eq!(center & 0x00FF_FFFF, 0x0000_FF00);
    assert_eq!(corner & 0x00FF_FFFF, 0x00FF_0000);
}
