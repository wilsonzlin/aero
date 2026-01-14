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

fn new_test_machine() -> Machine {
    Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal and deterministic for these unit tests.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap()
}

fn setup_pci_and_get_bar0(m: &mut Machine) -> (PciBdf, u64) {
    // Canonical AeroGPU BDF (A3A0:0001).
    let bdf = PciBdf::new(0, 0x07, 0);

    // BAR0 base (assigned by `bios_post`).
    let bar0 = u64::from(cfg_read(m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(m, bdf, 0x04, 2, u32::from(command));

    (bdf, bar0)
}

fn write_ring_header(
    m: &mut Machine,
    ring_gpa: u64,
    entry_count: u32,
    head: u32,
    tail: u32,
) -> u32 {
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    m.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, head);
    m.write_physical_u32(ring_gpa + 28, tail);

    ring_size_bytes
}

fn write_submit_desc(
    m: &mut Machine,
    desc_gpa: u64,
    cmd_gpa: u64,
    cmd_size_bytes: u32,
    signal_fence: u64,
    flags: u32,
) {
    m.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, flags);
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, cmd_size_bytes);
    m.write_physical_u32(desc_gpa + 28, 0);
    m.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    m.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    m.write_physical_u32(desc_gpa + 44, 0);
    m.write_physical_u64(desc_gpa + 48, signal_fence);
    m.write_physical_u64(desc_gpa + 56, 0);
}

#[test]
fn vsync_present_fence_does_not_complete_until_vblank_tick() {
    let mut m = new_test_machine();
    let (bdf, bar0) = setup_pci_and_get_bar0(&mut m);

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    // Build a command stream containing a PRESENT with the VSYNC flag.
    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    m.write_physical(cmd_gpa, &cmd_stream);

    // Ring with a single submission.
    let entry_count = 8u32;
    let ring_size_bytes = write_ring_header(
        &mut m,
        ring_gpa,
        entry_count,
        /*head=*/ 0,
        /*tail=*/ 1,
    );

    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 1u64;
    write_submit_desc(
        &mut m,
        desc_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        signal_fence,
        0,
    );

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

    // Enable scanout/vblank scheduling and fence IRQ delivery.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

    // Doorbell + process.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Doorbell must not complete vsynced presents: the fence should remain pending.
    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 0);

    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "INTx should not assert before the next vblank tick"
    );

    // Ring head should advance to match tail.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);

    // On vblank, the vsync fence becomes eligible and completes.
    let period_ns = u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS)),
    );
    m.tick_platform(period_ns);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, signal_fence);

    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to assert after vsync-paced fence completion"
    );

    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence);
}

#[test]
fn vsync_fence_completion_is_gated_on_pci_bus_master_enable() {
    let mut m = new_test_machine();
    let (bdf, bar0) = setup_pci_and_get_bar0(&mut m);

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    // Build a command stream containing a PRESENT with the VSYNC flag.
    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    m.write_physical(cmd_gpa, &cmd_stream);

    // Ring with a single submission.
    let entry_count = 8u32;
    let ring_size_bytes = write_ring_header(
        &mut m,
        ring_gpa,
        entry_count,
        /*head=*/ 0,
        /*tail=*/ 1,
    );

    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 1u64;
    write_submit_desc(
        &mut m,
        desc_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        signal_fence,
        0,
    );

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

    // Enable scanout/vblank scheduling and fence IRQ delivery.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

    // Doorbell + process.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Disable bus mastering before the vblank edge.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command &= !(1 << 2); // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    let period_ns = u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS)),
    );

    // On vblank, the fence is eligible, but must not complete without bus mastering.
    m.tick_platform(period_ns);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 0);
    assert_eq!(
        m.read_physical_u64(fence_gpa + 8),
        0,
        "device must not DMA an updated fence page while bus mastering is disabled"
    );
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "INTx should not assert while bus mastering is disabled"
    );

    // Re-enable bus mastering and allow the next vblank tick to complete the fence.
    command |= 1 << 2;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    m.tick_platform(period_ns);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, signal_fence);
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence);
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to assert after vsync-paced fence completion"
    );
}

#[test]
fn duplicate_fence_vsync_present_upgrades_kind_and_merges_irq_semantics() {
    let mut m = new_test_machine();
    let (bdf, bar0) = setup_pci_and_get_bar0(&mut m);

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    // Command stream containing a vsynced PRESENT.
    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    m.write_physical(cmd_gpa, &cmd_stream);

    // Ring with two submissions sharing the same fence.
    let entry_count = 8u32;
    let ring_size_bytes = write_ring_header(
        &mut m,
        ring_gpa,
        entry_count,
        /*head=*/ 0,
        /*tail=*/ 2,
    );

    let desc0_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 1u64;

    // First submission: NO_IRQ and no cmd stream (would normally be treated as immediate).
    write_submit_desc(
        &mut m,
        desc0_gpa,
        0,
        0,
        signal_fence,
        ring::AEROGPU_SUBMIT_FLAG_NO_IRQ,
    );

    // Second submission: same fence, wants IRQ, and contains a vsync PRESENT. This should upgrade
    // the fence to vblank-paced completion semantics.
    let desc1_gpa = desc0_gpa + ring::AerogpuSubmitDesc::SIZE_BYTES as u64;
    write_submit_desc(
        &mut m,
        desc1_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        signal_fence,
        0,
    );

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

    // Enable scanout/vblank scheduling and fence IRQ delivery.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

    // Doorbell + process.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Ring head should advance to match tail.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 2);

    // Because the second submission upgraded the fence to vsync-paced completion, the fence must
    // remain pending until the next vblank tick.
    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 0);
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "INTx should not assert before the next vblank tick"
    );

    // On vblank, the vsync fence becomes eligible and completes. IRQ should assert because the
    // second submission wanted it.
    let period_ns = u64::from(
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS)),
    );
    m.tick_platform(period_ns);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, signal_fence);

    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to assert after vsync-paced fence completion"
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence);
}

#[test]
fn pending_vsync_fence_is_flushed_when_scanout_is_disabled() {
    let mut m = new_test_machine();
    let (_bdf, bar0) = setup_pci_and_get_bar0(&mut m);

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    m.write_physical(cmd_gpa, &cmd_stream);

    let ring_size_bytes =
        write_ring_header(&mut m, ring_gpa, 8, /*head=*/ 0, /*tail=*/ 1);
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 1u64;
    write_submit_desc(
        &mut m,
        desc_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        signal_fence,
        0,
    );

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

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 0);

    // If scanout/vblank pacing is disabled after a vsync present is queued, do not leave the fence
    // blocked forever. The device should flush/publish the completion.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 0);
    m.process_aerogpu();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, signal_fence);
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence);
}

#[test]
fn disabling_scanout_does_not_flush_vsync_fences_while_pci_bme_is_disabled() {
    let mut m = new_test_machine();
    let (bdf, bar0) = setup_pci_and_get_bar0(&mut m);

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    m.write_physical(cmd_gpa, &cmd_stream);

    let ring_size_bytes =
        write_ring_header(&mut m, ring_gpa, 8, /*head=*/ 0, /*tail=*/ 1);
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 1u64;
    write_submit_desc(
        &mut m,
        desc_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        signal_fence,
        0,
    );

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
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    // Consume the vsynced submission; the fence should remain pending until a vblank edge (or until
    // vblank pacing is disabled).
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
        ),
        0
    );

    // Disable bus mastering, then disable scanout. The device must not complete fences (or DMA the
    // fence page) while COMMAND.BME=0.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command &= !(1 << 2); // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 0);
    m.process_aerogpu();

    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
        ),
        0
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), 0);

    // Re-enable bus mastering and process; the device should now flush the vsync fence completion.
    command |= 1 << 2;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));
    m.process_aerogpu();

    assert_eq!(
        read_mmio_u64(
            &mut m,
            bar0,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
        ),
        signal_fence
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence);
}

#[test]
fn vsync_fence_blocks_immediate_fences_behind_it_until_vblank() {
    let mut m = new_test_machine();
    let (_bdf, bar0) = setup_pci_and_get_bar0(&mut m);

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    m.write_physical(cmd_gpa, &cmd_stream);

    let ring_size_bytes =
        write_ring_header(&mut m, ring_gpa, 8, /*head=*/ 0, /*tail=*/ 2);
    let stride = ring::AerogpuSubmitDesc::SIZE_BYTES as u64;
    let desc0_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let desc1_gpa = desc0_gpa + stride;

    write_submit_desc(&mut m, desc0_gpa, cmd_gpa, cmd_stream.len() as u32, 1, 0);
    // Empty submission (no cmd stream) should be treated as immediate.
    write_submit_desc(&mut m, desc1_gpa, 0, 0, 2, 0);

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
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(
        completed_fence, 0,
        "immediate fence behind vsync fence must not complete on doorbell"
    );

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
    assert_eq!(completed_fence, 2);
    assert_eq!(m.read_physical_u64(fence_gpa + 8), 2);
}

#[test]
fn completes_at_most_one_vsync_fence_per_vblank_tick() {
    let mut m = new_test_machine();
    let (_bdf, bar0) = setup_pci_and_get_bar0(&mut m);

    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;

    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    m.write_physical(cmd_gpa, &cmd_stream);

    let ring_size_bytes =
        write_ring_header(&mut m, ring_gpa, 8, /*head=*/ 0, /*tail=*/ 2);
    let stride = ring::AerogpuSubmitDesc::SIZE_BYTES as u64;
    let desc0_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let desc1_gpa = desc0_gpa + stride;

    write_submit_desc(&mut m, desc0_gpa, cmd_gpa, cmd_stream.len() as u32, 1, 0);
    write_submit_desc(&mut m, desc1_gpa, cmd_gpa, cmd_stream.len() as u32, 2, 0);

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
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();

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
    assert_eq!(completed_fence, 1);

    m.tick_platform(period_ns);
    m.process_aerogpu();

    let completed_fence = read_mmio_u64(
        &mut m,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence, 2);
}
