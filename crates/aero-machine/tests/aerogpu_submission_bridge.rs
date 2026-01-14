use aero_devices::pci::{PciBdf, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_PRESENT_FLAG_VSYNC;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

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

#[test]
fn aerogpu_submission_bridge_drains_and_requires_host_fence_completion() {
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

    let mut m = Machine::new(cfg).unwrap();
    // Enable the submission bridge before any guest submissions occur.
    m.aerogpu_enable_submission_bridge();
    assert!(m.aerogpu_drain_submissions().is_empty());

    // Canonical AeroGPU BDF (A3A0:0001).
    let bdf = PciBdf::new(0, 0x07, 0);

    // BAR0 base (assigned by `bios_post`).
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    m.write_physical(cmd_gpa, &[0xDE, 0xAD, 0xBE, 0xEF]);

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
    let signal_fence = 0x1234_5678_9ABC_DEF0u64;

    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, 4); // cmd_size_bytes
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

    // Enable fence IRQ delivery.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    // Doorbell: ring is consumed but fence does not complete until the host acknowledges it.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Ring head advanced.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);

    // Completed fence remains at 0 until the host calls `aerogpu_complete_fence`.
    let completed_fence = (u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI)),
    ) << 32)
        | u64::from(
            m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO)),
        );
    assert_eq!(completed_fence, 0);

    // Fence page written (but still reports completed_fence=0).
    assert_eq!(
        m.read_physical_u32(fence_gpa),
        ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        m.read_physical_u32(fence_gpa + 4),
        pci::AEROGPU_ABI_VERSION_U32
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), 0);

    // Drain submissions for out-of-process execution.
    let subs = m.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 1);
    let sub = &subs[0];
    assert_eq!(sub.signal_fence, signal_fence);
    assert_eq!(sub.context_id, 0);
    assert_eq!(sub.engine_id, ring::AEROGPU_ENGINE_0);
    assert_eq!(sub.flags, 0);
    assert_eq!(sub.cmd_stream, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(sub.alloc_table, None);

    // Host reports completion: fence page + IRQ state update.
    m.aerogpu_complete_fence(signal_fence);
    m.poll_pci_intx_lines();

    let completed_fence = (u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI)),
    ) << 32)
        | u64::from(
            m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO)),
        );
    assert_eq!(completed_fence, signal_fence);
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence);

    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, pci::AEROGPU_IRQ_FENCE);

    // PCI INTx line asserted.
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to assert after host fence completion"
    );
}

#[test]
fn aerogpu_submission_bridge_queue_overflow_does_not_deadlock_fences() {
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

    let mut m = Machine::new(cfg).unwrap();
    m.aerogpu_enable_submission_bridge();

    // Canonical AeroGPU BDF (A3A0:0001).
    let bdf = PciBdf::new(0, 0x07, 0);
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0);

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    m.write_physical(cmd_gpa, &[0xDE, 0xAD, 0xBE, 0xEF]);

    // The submission drain queue is capped at 256 entries. Submitting one more entry should drop
    // the oldest submission, but must not deadlock guest fences (the dropped fence is
    // auto-completed with an error so later fences can still progress once the host completes
    // them).
    let submission_count = 257u32;

    // Ring header large enough for all submissions (entry_count must be power-of-two).
    let entry_count = 512u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    m.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, submission_count); // tail

    for i in 0..submission_count {
        let slot_gpa = ring_gpa
            + ring::AerogpuRingHeader::SIZE_BYTES as u64
            + u64::from(i) * u64::from(entry_stride_bytes);
        let fence = u64::from(i + 1);

        m.write_physical_u32(slot_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
        m.write_physical_u32(slot_gpa + 4, 0); // flags
        m.write_physical_u32(slot_gpa + 8, 0); // context_id
        m.write_physical_u32(slot_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
        m.write_physical_u64(slot_gpa + 16, cmd_gpa);
        m.write_physical_u32(slot_gpa + 24, 4); // cmd_size_bytes
        m.write_physical_u32(slot_gpa + 28, 0);
        m.write_physical_u64(slot_gpa + 32, 0); // alloc_table_gpa
        m.write_physical_u32(slot_gpa + 40, 0); // alloc_table_size_bytes
        m.write_physical_u32(slot_gpa + 44, 0);
        m.write_physical_u64(slot_gpa + 48, fence);
        m.write_physical_u64(slot_gpa + 56, 0);
    }

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

    // Doorbell: consume all submissions.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    assert_eq!(m.read_physical_u32(ring_gpa + 24), submission_count);

    // Oldest submission dropped from the queue, but its fence must still be completed so later
    // fences can advance once the host reports them.
    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 1);

    // Queue overflow is treated as a backend error for the dropped fence so a stuck host cannot
    // silently deadlock the guest.
    assert_eq!(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_CODE)),
        pci::AerogpuErrorCode::Backend as u32
    );
    let error_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_ERROR_FENCE_LO,
        pci::AEROGPU_MMIO_REG_ERROR_FENCE_HI,
    );
    assert_eq!(error_fence, 1);
    assert_eq!(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_ERROR_COUNT)),
        1
    );
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(irq_status & pci::AEROGPU_IRQ_ERROR, 0);

    let subs = m.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 256);
    assert_eq!(subs[0].signal_fence, 2);
    assert_eq!(subs[255].signal_fence, 257);

    // Host completes all drained fences; completed_fence should advance through the end even
    // though fence=1 was never drained.
    for sub in subs {
        m.aerogpu_complete_fence(sub.signal_fence);
    }

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 257);
}

#[test]
fn aerogpu_submission_bridge_drains_cmd_streams_with_zero_fence() {
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

    let mut m = Machine::new(cfg).unwrap();
    m.aerogpu_enable_submission_bridge();

    let bdf = PciBdf::new(0, 0x07, 0);
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");

    let ring_gpa = 0x10000u64;
    let cmd_gpa = 0x30000u64;
    m.write_physical(cmd_gpa, &[0xAA, 0xBB, 0xCC, 0xDD]);

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

    // Submit desc in slot 0 with signal_fence=0 (best-effort/internal submissions).
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, 4); // cmd_size_bytes
    m.write_physical_u32(desc_gpa + 28, 0);
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc_gpa + 44, 0);
    m.write_physical_u64(desc_gpa + 48, 0); // signal_fence
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

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    // Doorbell: ring is consumed.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);

    // Command stream should still be surfaced for out-of-process execution even though it has no
    // fence to complete.
    let subs = m.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 1);
    let sub = &subs[0];
    assert_eq!(sub.signal_fence, 0);
    assert_eq!(sub.context_id, 0);
    assert_eq!(sub.engine_id, ring::AEROGPU_ENGINE_0);
    assert_eq!(sub.flags, 0);
    assert_eq!(sub.cmd_stream, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(sub.alloc_table, None);
}

#[test]
fn aerogpu_submission_bridge_duplicate_fence_merges_no_irq_semantics() {
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

    let mut m = Machine::new(cfg).unwrap();
    // Enable the submission bridge before any guest submissions occur.
    m.aerogpu_enable_submission_bridge();

    // Canonical AeroGPU BDF (A3A0:0001).
    let bdf = PciBdf::new(0, 0x07, 0);
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    m.write_physical(cmd_gpa, &[0xDE, 0xAD, 0xBE, 0xEF]);

    // Build a minimal valid ring containing two submit descs (head=0, tail=2) that share the
    // same fence value.
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
    m.write_physical_u32(ring_gpa + 28, 2); // tail

    let signal_fence = 0x1234_5678_9ABC_DEF0u64;

    // Submit desc in slot 0: NO_IRQ set.
    let desc0_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    m.write_physical_u32(desc0_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc0_gpa + 4, ring::AEROGPU_SUBMIT_FLAG_NO_IRQ); // flags
    m.write_physical_u32(desc0_gpa + 8, 0); // context_id
    m.write_physical_u32(desc0_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc0_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc0_gpa + 24, 4); // cmd_size_bytes
    m.write_physical_u32(desc0_gpa + 28, 0);
    m.write_physical_u64(desc0_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc0_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc0_gpa + 44, 0);
    m.write_physical_u64(desc0_gpa + 48, signal_fence);
    m.write_physical_u64(desc0_gpa + 56, 0);

    // Submit desc in slot 1: wants IRQ, reuses the same fence value.
    let desc1_gpa = desc0_gpa + u64::from(entry_stride_bytes);
    m.write_physical_u32(desc1_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc1_gpa + 4, 0); // flags
    m.write_physical_u32(desc1_gpa + 8, 0); // context_id
    m.write_physical_u32(desc1_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc1_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc1_gpa + 24, 4); // cmd_size_bytes
    m.write_physical_u32(desc1_gpa + 28, 0);
    m.write_physical_u64(desc1_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc1_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc1_gpa + 44, 0);
    m.write_physical_u64(desc1_gpa + 48, signal_fence);
    m.write_physical_u64(desc1_gpa + 56, 0);

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

    // Enable fence IRQ delivery.
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    // Doorbell: ring is consumed but fence does not complete until the host acknowledges it.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Ring head advanced.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 2);

    // Completed fence remains at 0 until the host calls `aerogpu_complete_fence`.
    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
        ),
        0
    );

    // Drain submissions: both should be surfaced for out-of-process execution.
    let subs = m.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0].signal_fence, signal_fence);
    assert_eq!(subs[1].signal_fence, signal_fence);

    // Host reports completion. IRQ must assert because the second submission wanted it.
    m.aerogpu_complete_fence(signal_fence);
    m.poll_pci_intx_lines();

    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
        ),
        signal_fence
    );

    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, pci::AEROGPU_IRQ_FENCE);

    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to assert when any submission for the fence wants an IRQ"
    );
}

#[test]
fn aerogpu_submission_bridge_vsync_present_fence_completes_on_next_vblank() {
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

    let mut m = Machine::new(cfg).unwrap();
    m.aerogpu_enable_submission_bridge();

    let bdf = PciBdf::new(0, 0x07, 0);
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0);

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    let ring_gpa = 0x10000u64;
    let cmd_gpa = 0x30000u64;

    // Build a minimal command stream with a vsynced PRESENT so the fence is vblank-gated.
    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    m.write_physical(cmd_gpa, &cmd_stream);

    // Ring header (single entry, head=0 tail=1).
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
    let signal_fence = 1u64;
    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, ring::AEROGPU_SUBMIT_FLAG_PRESENT); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, cmd_stream.len() as u32); // cmd_size_bytes
    m.write_physical_u32(desc_gpa + 28, 0);
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc_gpa + 44, 0);
    m.write_physical_u64(desc_gpa + 48, signal_fence);
    m.write_physical_u64(desc_gpa + 56, 0);

    // Program BAR0 registers and enable scanout/vblank scheduling.
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
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    // Doorbell consumes the ring and queues the fence as vblank-paced.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    assert_eq!(m.aerogpu_drain_submissions().len(), 1);

    // Host reports completion. With the submission bridge enabled, the device model still applies
    // vblank pacing for vsync PRESENTs; the fence must not complete until the next vblank edge.
    m.aerogpu_complete_fence(signal_fence);
    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 0, "vsync fence must not complete immediately");

    // Advance to the next vblank; the vsync fence becomes eligible and completes.
    let period_ns = u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS)),
    );
    m.tick_platform(period_ns);
    m.process_aerogpu();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, signal_fence);
}

#[test]
fn aerogpu_submission_bridge_duplicate_fence_does_not_block_later_fences() {
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

    let mut m = Machine::new(cfg).unwrap();
    m.aerogpu_enable_submission_bridge();

    let bdf = PciBdf::new(0, 0x07, 0);
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0);

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    let ring_gpa = 0x10000u64;
    let cmd0_gpa = 0x30000u64;
    let cmd1_gpa = 0x31000u64;
    let cmd2_gpa = 0x32000u64;

    // Two command streams that reuse the same signal fence. The first present is not vsynced; the
    // second is. With the submission bridge enabled, the device model still applies vblank pacing
    // for vsync PRESENTs, and must merge duplicate fence semantics so later fences are not
    // permanently blocked.
    let cmd0 = {
        let mut writer = AerogpuCmdWriter::new();
        writer.present(0, 0);
        writer.finish()
    };
    let cmd1 = {
        let mut writer = AerogpuCmdWriter::new();
        writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
        writer.finish()
    };
    let cmd2 = {
        let mut writer = AerogpuCmdWriter::new();
        writer.present(0, 0);
        writer.finish()
    };
    m.write_physical(cmd0_gpa, &cmd0);
    m.write_physical(cmd1_gpa, &cmd1);
    m.write_physical(cmd2_gpa, &cmd2);

    // Ring header (three entries, head=0 tail=3).
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
    m.write_physical_u32(ring_gpa + 28, 3); // tail

    let signal_fence = 1u64;
    let fence2 = 2u64;
    let desc0_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let desc1_gpa = desc0_gpa + u64::from(entry_stride_bytes);
    let desc2_gpa = desc1_gpa + u64::from(entry_stride_bytes);

    // Descriptor 0 (non-vsync present).
    m.write_physical_u32(desc0_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc0_gpa + 4, ring::AEROGPU_SUBMIT_FLAG_PRESENT); // flags
    m.write_physical_u32(desc0_gpa + 8, 0); // context_id
    m.write_physical_u32(desc0_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc0_gpa + 16, cmd0_gpa);
    m.write_physical_u32(desc0_gpa + 24, cmd0.len() as u32); // cmd_size_bytes
    m.write_physical_u32(desc0_gpa + 28, 0);
    m.write_physical_u64(desc0_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc0_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc0_gpa + 44, 0);
    m.write_physical_u64(desc0_gpa + 48, signal_fence);
    m.write_physical_u64(desc0_gpa + 56, 0);

    // Descriptor 1 (vsync present) reusing the same fence value.
    m.write_physical_u32(desc1_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc1_gpa + 4, ring::AEROGPU_SUBMIT_FLAG_PRESENT); // flags
    m.write_physical_u32(desc1_gpa + 8, 0); // context_id
    m.write_physical_u32(desc1_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc1_gpa + 16, cmd1_gpa);
    m.write_physical_u32(desc1_gpa + 24, cmd1.len() as u32); // cmd_size_bytes
    m.write_physical_u32(desc1_gpa + 28, 0);
    m.write_physical_u64(desc1_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc1_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc1_gpa + 44, 0);
    m.write_physical_u64(desc1_gpa + 48, signal_fence);
    m.write_physical_u64(desc1_gpa + 56, 0);

    // Descriptor 2 (new fence value behind the duplicate fence).
    m.write_physical_u32(desc2_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc2_gpa + 4, ring::AEROGPU_SUBMIT_FLAG_PRESENT); // flags
    m.write_physical_u32(desc2_gpa + 8, 0); // context_id
    m.write_physical_u32(desc2_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc2_gpa + 16, cmd2_gpa);
    m.write_physical_u32(desc2_gpa + 24, cmd2.len() as u32); // cmd_size_bytes
    m.write_physical_u32(desc2_gpa + 28, 0);
    m.write_physical_u64(desc2_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc2_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc2_gpa + 44, 0);
    m.write_physical_u64(desc2_gpa + 48, fence2);
    m.write_physical_u64(desc2_gpa + 56, 0);

    // Program BAR0 registers and enable scanout/vblank scheduling.
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
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    // Doorbell consumes the ring and queues both submissions.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    assert_eq!(m.aerogpu_drain_submissions().len(), 3);

    // Host reports completion for the duplicated fence. The duplicate fence is upgraded to
    // vsync-paced completion semantics, so it must not complete until the next vblank tick.
    m.aerogpu_complete_fence(signal_fence);
    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 0);

    let period_ns = u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS)),
    );
    m.tick_platform(period_ns);
    m.process_aerogpu();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, signal_fence);

    // Host reports completion for the later fence; it must not be blocked by a stale duplicate
    // fence entry.
    m.aerogpu_complete_fence(fence2);
    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, fence2);
}
