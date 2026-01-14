use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_pci as pci, aerogpu_ring as ring};
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

fn mmio_read_u64_pair(m: &mut Machine, lo: u64, hi: u64) -> u64 {
    (u64::from(m.read_physical_u32(hi)) << 32) | u64::from(m.read_physical_u32(lo))
}

#[test]
fn aerogpu_vsync_present_paces_fence_until_next_vblank() {
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

    // Canonical AeroGPU BDF (A3A0:0001).
    let bdf = PciBdf::new(0, 0x07, 0);

    // BAR0 base (assigned by `bios_post`).
    let bar0 = u64::from(cfg_read(&mut m, bdf, 0x10, 4) & !0xFu32);
    assert_ne!(bar0, 0, "expected AeroGPU BAR0 to be assigned");

    // Enable PCI Bus Mastering so the device is allowed to DMA into guest memory.
    let mut command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    command |= 1 << 2; // COMMAND.BME
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command));

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;
    let scanout_fb_gpa = 0x40000u64;

    // Build a minimal command stream containing a vsynced PRESENT.
    let stream_size_bytes = (cmd::AerogpuCmdStreamHeader::SIZE_BYTES + cmd::AerogpuCmdPresent::SIZE_BYTES) as u32;
    let mut cmd_stream = vec![0u8; stream_size_bytes as usize];
    cmd_stream[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_stream[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_stream[8..12].copy_from_slice(&stream_size_bytes.to_le_bytes());
    cmd_stream[12..16].copy_from_slice(&(cmd::AerogpuCmdStreamFlags::None as u32).to_le_bytes());
    // reserved0/reserved1 remain zero.

    // PRESENT packet (immediately after stream header).
    let present_off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
    cmd_stream[present_off..present_off + 4]
        .copy_from_slice(&(cmd::AerogpuCmdOpcode::Present as u32).to_le_bytes());
    cmd_stream[present_off + 4..present_off + 8]
        .copy_from_slice(&(cmd::AerogpuCmdPresent::SIZE_BYTES as u32).to_le_bytes());
    cmd_stream[present_off + 8..present_off + 12].copy_from_slice(&0u32.to_le_bytes()); // scanout_id
    cmd_stream[present_off + 12..present_off + 16]
        .copy_from_slice(&cmd::AEROGPU_PRESENT_FLAG_VSYNC.to_le_bytes());

    m.write_physical(cmd_gpa, &cmd_stream);

    // Build a minimal valid ring containing a single submit desc (head=0, tail=1).
    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    // Ring header.
    m.write_physical_u32(ring_gpa + 0, ring::AEROGPU_RING_MAGIC);
    m.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    m.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    m.write_physical_u32(ring_gpa + 12, entry_count);
    m.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    m.write_physical_u32(ring_gpa + 20, 0); // flags
    m.write_physical_u32(ring_gpa + 24, 0); // head
    m.write_physical_u32(ring_gpa + 28, 1); // tail

    // Submit desc in slot 0.
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 0xCAFE_BABE_DEAD_BEEFu64;

    m.write_physical_u32(desc_gpa + 0, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, ring::AEROGPU_SUBMIT_FLAG_PRESENT); // flags
    m.write_physical_u32(desc_gpa + 8, 0); // context_id
    m.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    m.write_physical_u64(desc_gpa + 16, cmd_gpa);
    m.write_physical_u32(desc_gpa + 24, stream_size_bytes); // cmd_size_bytes
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

    // Enable scanout0 so vblank ticks advance.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 1);
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 1);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES), 4);
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        scanout_fb_gpa as u32,
    );
    m.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (scanout_fb_gpa >> 32) as u32,
    );
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    // Doorbell.
    m.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Ring head advanced (submission consumed), but fence should not be completed yet.
    assert_eq!(m.read_physical_u32(ring_gpa + 24), 1);

    let completed_fence = mmio_read_u64_pair(
        &mut m,
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO),
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI),
    );
    assert_eq!(completed_fence, 0);

    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);

    // Advance time to just before the next vblank and confirm the fence is still pending.
    let clock = m.platform_clock().expect("pc platform enabled");
    let vblank_period_ns =
        m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS));
    assert_ne!(vblank_period_ns, 0);

    clock.advance_ns(u64::from(vblank_period_ns) - 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    let completed_fence = mmio_read_u64_pair(
        &mut m,
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO),
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI),
    );
    assert_eq!(completed_fence, 0);

    // Advance to the vblank edge; the vsynced present should now complete.
    clock.advance_ns(1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    let completed_fence = mmio_read_u64_pair(
        &mut m,
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO),
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI),
    );
    assert_eq!(completed_fence, signal_fence);

    // Fence page written.
    assert_eq!(
        m.read_physical_u32(fence_gpa + 0),
        ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        m.read_physical_u32(fence_gpa + 4),
        pci::AEROGPU_ABI_VERSION_U32
    );
    assert_eq!(m.read_physical_u64(fence_gpa + 8), signal_fence);

    // IRQ status latched once the fence completes.
    let irq_status = m.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, pci::AEROGPU_IRQ_FENCE);
}

