use aero_devices::pci::{profile, PciBdf};
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

fn enable_pci_command_bits(m: &mut Machine, bdf: PciBdf, mask: u16) {
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();
    let command = bus.read_config(bdf, 0x04, 2) as u16;
    bus.write_config(bdf, 0x04, 2, u32::from(command | mask));
}

fn setup_ring(m: &mut Machine, ring_gpa: u64, cmd_gpa: u64, signal_fence: u64) -> u32 {
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

    ring_size_bytes
}

#[test]
fn aerogpu_snapshot_preserves_doorbell_pending_latch() {
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
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg.clone()).unwrap();
    let bdf = m.aerogpu_bdf().expect("expected AeroGPU device present");
    enable_pci_command_bits(&mut m, bdf, (1 << 1) | (1 << 2)); // COMMAND.MEM + COMMAND.BME
    let bar0 = m
        .pci_bar_base(bdf, profile::AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 base");

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;
    let signal_fence = 0x1111_2222_3333_4444u64;
    let ring_size_bytes = setup_ring(&mut m, ring_gpa, cmd_gpa, signal_fence);

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

    // Ring the doorbell but do not call `process_aerogpu()` yet.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    let snap = m.take_snapshot_full().unwrap();

    // Restore into a fresh machine and ensure the pending doorbell still triggers processing.
    let mut m2 = Machine::new(cfg).unwrap();
    m2.restore_snapshot_bytes(&snap).unwrap();
    m2.process_aerogpu();

    // Ring head should have advanced.
    assert_eq!(m2.read_physical_u32(ring_gpa + 24), 1);

    // Submission should be drainable (captured from guest memory).
    let subs = m2.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].signal_fence, signal_fence);
    assert_eq!(subs[0].cmd_stream, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn aerogpu_snapshot_preserves_pending_submission_and_fence_queue_for_bridge_mode() {
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
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg.clone()).unwrap();
    let bdf = m.aerogpu_bdf().expect("expected AeroGPU device present");
    enable_pci_command_bits(&mut m, bdf, (1 << 1) | (1 << 2)); // COMMAND.MEM + COMMAND.BME
    let bar0 = m
        .pci_bar_base(bdf, profile::AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 base");

    // Enable external-executor fence semantics.
    m.aerogpu_enable_submission_bridge();

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;
    let signal_fence = 0xAAAA_BBBB_CCCC_DDDDu64;
    let ring_size_bytes = setup_ring(&mut m, ring_gpa, cmd_gpa, signal_fence);

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

    // Consume the submission; in bridge mode the fence should *not* complete until the host
    // reports completion.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    let completed_before = (u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI)),
    ) << 32)
        | u64::from(
            m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO)),
        );
    assert_eq!(
        completed_before, 0,
        "fence should not complete before backend ack"
    );

    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = Machine::new(cfg).unwrap();
    m2.restore_snapshot_bytes(&snap).unwrap();
    // Restore is into a fresh machine; re-enable bridge semantics before delivering completions.
    m2.aerogpu_enable_submission_bridge();

    // Pending submission payload should still be drainable after restore.
    let subs = m2.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].signal_fence, signal_fence);
    assert_eq!(subs[0].cmd_stream, vec![0xDE, 0xAD, 0xBE, 0xEF]);

    // Completing the fence should advance COMPLETED_FENCE and update the fence page.
    m2.aerogpu_complete_fence(signal_fence);
    let bar0 = m2
        .pci_bar_base(bdf, profile::AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 base after restore");
    let completed_after = (u64::from(
        m2.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI)),
    ) << 32)
        | u64::from(
            m2.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO)),
        );
    assert_eq!(completed_after, signal_fence);
    assert_eq!(m2.read_physical_u64(fence_gpa + 8), signal_fence);
}

#[test]
fn aerogpu_snapshot_preserves_deferred_backend_completion_queued_while_bme_disabled() {
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
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg.clone()).unwrap();
    let bdf = m
        .aerogpu_bdf()
        .expect("expected AeroGPU device present");
    enable_pci_command_bits(&mut m, bdf, (1 << 1) | (1 << 2)); // COMMAND.MEM + COMMAND.BME
    let bar0 = m
        .pci_bar_base(bdf, profile::AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 base");

    // Enable external-executor fence semantics.
    m.aerogpu_enable_submission_bridge();

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;
    let signal_fence = 0x1357_2468_ACE0_0001u64;
    let ring_size_bytes = setup_ring(&mut m, ring_gpa, cmd_gpa, signal_fence);

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

    // Consume the submission; in bridge mode the fence should *not* complete until the host reports
    // completion.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    // Disable bus mastering.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let command = bus.read_config(bdf, 0x04, 2) as u16;
        bus.write_config(bdf, 0x04, 2, u32::from(command & !(1 << 2)));
    }

    // Report completion while BME is disabled. The device should defer applying it.
    m.aerogpu_complete_fence(signal_fence);
    let completed_before = (u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI)),
    ) << 32)
        | u64::from(
            m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO)),
        );
    assert_eq!(completed_before, 0);
    assert_eq!(m.read_physical_u64(fence_gpa + 8), 0);

    // Snapshot while the completion is queued but not yet applied.
    let snap = m.take_snapshot_full().unwrap();

    let mut m2 = Machine::new(cfg).unwrap();
    m2.restore_snapshot_bytes(&snap).unwrap();
    m2.aerogpu_enable_submission_bridge();

    // Re-enable BME and process; the queued completion should be applied without re-sending it.
    enable_pci_command_bits(&mut m2, bdf, 1 << 2);
    m2.process_aerogpu();

    let bar0 = m2
        .pci_bar_base(bdf, profile::AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 base after restore");
    let completed_after = (u64::from(
        m2.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI)),
    ) << 32)
        | u64::from(
            m2.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO)),
        );
    assert_eq!(completed_after, signal_fence);
    assert_eq!(m2.read_physical_u64(fence_gpa + 8), signal_fence);
}
