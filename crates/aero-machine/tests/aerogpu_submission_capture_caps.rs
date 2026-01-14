use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};

const MAX_CMD_STREAM_SIZE_BYTES: u32 = 64 * 1024 * 1024;
const MAX_AEROGPU_ALLOC_TABLE_BYTES: u32 = 16 * 1024 * 1024;

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    // PCI config mechanism #1: 0x8000_0000 | bus<<16 | dev<<11 | fn<<8 | (offset & 0xFC)
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1F) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

fn read_mmio_u64(m: &mut Machine, base: u64, lo_off: u32, hi_off: u32) -> u64 {
    (u64::from(m.read_physical_u32(base + u64::from(hi_off))) << 32)
        | u64::from(m.read_physical_u32(base + u64::from(lo_off)))
}

fn new_minimal_machine() -> Machine {
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal and deterministic for this unit test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };
    Machine::new(cfg).unwrap()
}

fn setup_aerogpu_and_enable_bme(m: &mut Machine) -> (PciBdf, u64) {
    // Canonical AeroGPU BDF (A3A0:0001).
    let bdf = PciBdf::new(0, 0x07, 0);
    let bar0 = u64::from(cfg_read(m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(m, bdf, 0x04, 2, u32::from(command));

    (bdf, bar0)
}

#[test]
fn aerogpu_submission_capture_cmd_stream_header_size_over_max_is_capped() {
    let mut m = new_minimal_machine();
    let (_bdf, bar0) = setup_aerogpu_and_enable_bme(&mut m);

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    // Command stream backing buffer is small, but the on-guest header claims an absurdly large used
    // size. The device model must not attempt to allocate/copy the claimed size.
    let claimed_size_bytes = MAX_CMD_STREAM_SIZE_BYTES + 1;
    let cmd_prefix = {
        let mut buf = vec![0u8; ProtocolCmdStreamHeader::SIZE_BYTES];
        buf[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
        buf[8..12].copy_from_slice(&claimed_size_bytes.to_le_bytes());
        buf
    };
    m.write_physical(cmd_gpa, &cmd_prefix);

    // Build a minimal valid ring containing a single submit desc (head=0, tail=1).
    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    // Ring header.
    m.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, 1); // tail

    // Submit desc in slot 0.
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 1u64;

    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, ProtocolCmdStreamHeader::SIZE_BYTES as u32); // cmd_size_bytes
    m.write_physical_u32(desc_gpa + 28, 0);
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc_gpa + 44, 0);
    m.write_physical_u64(desc_gpa + 48, signal_fence);
    m.write_physical_u64(desc_gpa + 56, 0);

    // Program BAR0 registers.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        ring_size_bytes,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );

    // Doorbell: consume ring, capture submission, and auto-complete fence (legacy bring-up mode).
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    // Ring head advanced.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);

    // Submission payload should be capped to the header prefix, not the claimed size.
    let subs = m.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 1);
    let sub = &subs[0];
    assert_eq!(sub.signal_fence, signal_fence);
    assert!(
        sub.cmd_stream.len() <= ProtocolCmdStreamHeader::SIZE_BYTES,
        "cmd_stream should be capped to the fixed header prefix"
    );
    assert_eq!(sub.cmd_stream, cmd_prefix);
    assert_eq!(sub.alloc_table, None);

    // Fence still makes forward progress (auto-completed without the submission bridge enabled).
    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, signal_fence);

    // Oversized payload truncation does not currently surface an ERROR_INFO payload; it is treated
    // as a soft capture failure.
    assert_eq!(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_CODE)),
        pci::AerogpuErrorCode::None as u32
    );
    assert_eq!(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_COUNT)),
        0
    );
}

#[test]
fn aerogpu_submission_capture_alloc_table_header_size_over_max_is_rejected() {
    let mut m = new_minimal_machine();
    let (_bdf, bar0) = setup_aerogpu_and_enable_bme(&mut m);

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;
    let alloc_table_gpa = 0x40000u64;

    // Minimal command stream: header-only with used size == header size.
    let cmd_stream = {
        let size_bytes = ProtocolCmdStreamHeader::SIZE_BYTES as u32;
        let mut buf = vec![0u8; ProtocolCmdStreamHeader::SIZE_BYTES];
        buf[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
        buf[8..12].copy_from_slice(&size_bytes.to_le_bytes());
        buf
    };
    m.write_physical(cmd_gpa, &cmd_stream);

    // Alloc table header is otherwise valid but claims a size over the host capture cap.
    let alloc_size_bytes = MAX_AEROGPU_ALLOC_TABLE_BYTES + 1;
    let alloc_table_hdr = {
        let mut buf = vec![0u8; ring::AerogpuAllocTableHeader::SIZE_BYTES];
        buf[0..4].copy_from_slice(&ring::AEROGPU_ALLOC_TABLE_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
        buf[8..12].copy_from_slice(&alloc_size_bytes.to_le_bytes());
        buf[12..16].copy_from_slice(&0u32.to_le_bytes()); // entry_count
        buf[16..20].copy_from_slice(&(ring::AerogpuAllocEntry::SIZE_BYTES as u32).to_le_bytes());
        buf
    };
    m.write_physical(alloc_table_gpa, &alloc_table_hdr);

    // Minimal ring with one submission.
    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;
    m.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0);
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, 1); // tail

    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 2u64;
    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32);
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0);
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, ProtocolCmdStreamHeader::SIZE_BYTES as u32);
    m.write_physical_u32(desc_gpa + 28, 0);
    m.write_physical_u64(desc_gpa + 32, alloc_table_gpa);
    m.write_physical_u32(desc_gpa + 40, alloc_size_bytes); // alloc_table_size_bytes >= hdr size
    m.write_physical_u32(desc_gpa + 44, 0);
    m.write_physical_u64(desc_gpa + 48, signal_fence);
    m.write_physical_u64(desc_gpa + 56, 0);

    // Program BAR0 regs.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        ring_size_bytes,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );

    // Consume ring.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);

    // Submission captures command stream but rejects the oversized alloc table.
    let subs = m.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 1);
    let sub = &subs[0];
    assert_eq!(sub.signal_fence, signal_fence);
    assert_eq!(sub.cmd_stream, cmd_stream);
    assert_eq!(sub.alloc_table, None);

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, signal_fence);

    // Oversized alloc table is silently dropped; no ERROR_INFO payload is currently generated.
    assert_eq!(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_CODE)),
        pci::AerogpuErrorCode::None as u32
    );
    assert_eq!(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_COUNT)),
        0
    );
}

