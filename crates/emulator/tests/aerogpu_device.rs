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
