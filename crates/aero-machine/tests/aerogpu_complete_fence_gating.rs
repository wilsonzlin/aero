use aero_devices::pci::profile;
use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

fn make_minimal_machine() -> Machine {
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

fn enable_aerogpu_pci_mmio_and_bus_master(m: &mut Machine) -> u64 {
    // Enable PCI memory decoding + bus mastering so the device is allowed to DMA and raise INTx.
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        bus.write_config(profile::AEROGPU.bdf, 0x04, 2, (1 << 1) | (1 << 2));
        bus.device_config(profile::AEROGPU.bdf)
            .and_then(|cfg| cfg.bar_range(0))
            .map(|range| range.base)
            .unwrap_or(0)
    };
    assert_ne!(
        bar0_base, 0,
        "expected AeroGPU BAR0 to be assigned by BIOS POST"
    );
    bar0_base
}

fn set_aerogpu_bus_master(m: &mut Machine, enabled: bool) {
    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();
    let cmd = if enabled { (1 << 1) | (1 << 2) } else { 1 << 1 };
    bus.write_config(profile::AEROGPU.bdf, 0x04, 2, cmd);
}

fn write_simple_ring(
    m: &mut Machine,
    ring_gpa: u64,
    cmd_gpa: u64,
    cmd_bytes: &[u8],
    signal_fence: u64,
) -> u32 {
    m.write_physical(cmd_gpa, cmd_bytes);

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
    m.write_physical_u32(desc_gpa + 24, cmd_bytes.len() as u32); // cmd_size_bytes
    m.write_physical_u32(desc_gpa + 28, 0);
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc_gpa + 44, 0);
    m.write_physical_u64(desc_gpa + 48, signal_fence);
    m.write_physical_u64(desc_gpa + 56, 0);

    ring_size_bytes
}

fn read_completed_fence(m: &mut Machine, bar0_base: u64) -> u64 {
    (u64::from(
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI)),
    ) << 32)
        | u64::from(
            m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO)),
        )
}

#[test]
fn aerogpu_complete_fence_is_ignored_until_submission_bridge_is_enabled() {
    let mut m = make_minimal_machine();

    // Install the null backend so fences do not complete.
    m.aerogpu_set_backend_null();

    let bar0_base = enable_aerogpu_pci_mmio_and_bus_master(&mut m);

    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let gsi = pci_intx
        .borrow()
        .gsi_for_intx(profile::AEROGPU.bdf, PciInterruptPin::IntA);

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    let signal_fence = 0x1234_5678_9ABC_DEF0u64;
    let ring_size_bytes = write_simple_ring(
        &mut m,
        ring_gpa,
        cmd_gpa,
        &[0xDE, 0xAD, 0xBE, 0xEF],
        signal_fence,
    );

    // Program BAR0 registers.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        ring_size_bytes,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Ring the doorbell and let the device decode the submission.
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Ring head should advance, but fence must not complete until the host reports completion.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);
    assert_eq!(read_completed_fence(&mut m, bar0_base), 0);
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to remain deasserted until fence completion"
    );

    // Try to complete the fence without enabling the submission bridge: should be ignored.
    m.aerogpu_complete_fence(signal_fence);
    m.poll_pci_intx_lines();
    assert_eq!(read_completed_fence(&mut m, bar0_base), 0);

    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to remain deasserted when completion is ignored"
    );
}

#[test]
fn aerogpu_complete_fence_is_gated_on_pci_bus_master_enable() {
    let mut m = make_minimal_machine();

    // Enable the submission bridge so external fence completion is meaningful.
    m.aerogpu_enable_submission_bridge();

    let bar0_base = enable_aerogpu_pci_mmio_and_bus_master(&mut m);

    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let gsi = pci_intx
        .borrow()
        .gsi_for_intx(profile::AEROGPU.bdf, PciInterruptPin::IntA);

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    let signal_fence = 0x1234_5678_9ABC_DEF0u64;
    let ring_size_bytes = write_simple_ring(
        &mut m,
        ring_gpa,
        cmd_gpa,
        &[0xDE, 0xAD, 0xBE, 0xEF],
        signal_fence,
    );

    // Program BAR0 registers.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        ring_size_bytes,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Ring the doorbell and let the device decode the submission.
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Should have captured the submission for out-of-process execution.
    let subs = m.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].signal_fence, signal_fence);

    // Disable PCI bus mastering before reporting completion: the device must not perform DMA, but
    // it should also not require the host to re-report the completion once DMA is enabled again.
    set_aerogpu_bus_master(&mut m, false);
    m.aerogpu_complete_fence(signal_fence);
    m.poll_pci_intx_lines();

    assert_eq!(read_completed_fence(&mut m, bar0_base), 0);
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to remain deasserted while BME is disabled"
    );

    // Re-enable bus mastering and tick the device: the queued completion should now apply.
    set_aerogpu_bus_master(&mut m, true);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    assert_eq!(read_completed_fence(&mut m, bar0_base), signal_fence);
    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, pci::AEROGPU_IRQ_FENCE);
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to assert after fence completion"
    );
}
