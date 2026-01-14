use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_cmd as cmd, aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

fn read_mmio_u64(m: &mut Machine, base: u64, lo: u32, hi: u32) -> u64 {
    let lo = m.read_physical_u32(base + u64::from(lo));
    let hi = m.read_physical_u32(base + u64::from(hi));
    u64::from(lo) | (u64::from(hi) << 32)
}

#[test]
fn aerogpu_snapshot_preserves_pending_vsync_fence_until_next_vblank() {
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();
    let bdf = src
        .aerogpu_bdf()
        .expect("AeroGPU device should be present");
    let bar0 = src
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    // Enable PCI MMIO decode + bus mastering so BAR0 accesses route to the device and it can DMA.
    {
        let pci_cfg = src.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command(cfg.command() | (1 << 1) | (1 << 2));
    }

    // Guest allocations.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;
    let scanout_fb_gpa = 0x40000u64;

    // Minimal command stream containing a vsynced PRESENT.
    let stream_size_bytes =
        (cmd::AerogpuCmdStreamHeader::SIZE_BYTES + cmd::AerogpuCmdPresent::SIZE_BYTES) as u32;
    let mut cmd_stream = vec![0u8; stream_size_bytes as usize];
    cmd_stream[0..4].copy_from_slice(&cmd::AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    cmd_stream[4..8].copy_from_slice(&pci::AEROGPU_ABI_VERSION_U32.to_le_bytes());
    cmd_stream[8..12].copy_from_slice(&stream_size_bytes.to_le_bytes());
    cmd_stream[12..16].copy_from_slice(&(cmd::AerogpuCmdStreamFlags::None as u32).to_le_bytes());

    let present_off = cmd::AerogpuCmdStreamHeader::SIZE_BYTES;
    cmd_stream[present_off..present_off + 4]
        .copy_from_slice(&(cmd::AerogpuCmdOpcode::Present as u32).to_le_bytes());
    cmd_stream[present_off + 4..present_off + 8]
        .copy_from_slice(&(cmd::AerogpuCmdPresent::SIZE_BYTES as u32).to_le_bytes());
    cmd_stream[present_off + 8..present_off + 12].copy_from_slice(&0u32.to_le_bytes()); // scanout_id
    cmd_stream[present_off + 12..present_off + 16]
        .copy_from_slice(&cmd::AEROGPU_PRESENT_FLAG_VSYNC.to_le_bytes());

    src.write_physical(cmd_gpa, &cmd_stream);

    // Ring header (head=0, tail=1).
    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    src.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    src.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    src.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    src.write_physical_u32(ring_gpa + 12, entry_count);
    src.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    src.write_physical_u32(ring_gpa + 20, 0); // flags
    src.write_physical_u32(ring_gpa + 24, 0); // head
    src.write_physical_u32(ring_gpa + 28, 1); // tail

    // Submit desc in slot 0.
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 0xCAFE_BABE_DEAD_BEEFu64;
    src.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    src.write_physical_u32(desc_gpa + 4, ring::AEROGPU_SUBMIT_FLAG_PRESENT); // flags
    src.write_physical_u32(desc_gpa + 8, 0); // context_id
    src.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    src.write_physical_u64(desc_gpa + 16, cmd_gpa);
    src.write_physical_u32(desc_gpa + 24, stream_size_bytes); // cmd_size_bytes
    src.write_physical_u32(desc_gpa + 28, 0);
    src.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    src.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    src.write_physical_u32(desc_gpa + 44, 0);
    src.write_physical_u64(desc_gpa + 48, signal_fence);
    src.write_physical_u64(desc_gpa + 56, 0);

    // Program BAR0 ring + fence registers.
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        ring_size_bytes,
    );
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );

    // Enable fence IRQ delivery.
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Enable scanout0 so vblank ticks advance.
    src.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 1);
    src.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 1);
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        4,
    );
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        scanout_fb_gpa as u32,
    );
    src.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (scanout_fb_gpa >> 32) as u32,
    );
    src.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);

    // Doorbell; this consumes the ring entry and queues a vblank-paced fence completion.
    src.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    src.process_aerogpu();
    src.poll_pci_intx_lines();

    // Fence should not be completed yet (vsync-paced until next vblank).
    assert_eq!(
        read_mmio_u64(
            &mut src,
            bar0,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
        ),
        0
    );

    // Advance time to just before the next vblank edge.
    let period_ns =
        src.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS));
    assert_ne!(period_ns, 0);
    // Advance deterministic machine time; this also advances AeroGPU's internal vblank timebase.
    src.tick_platform(u64::from(period_ns) - 1);
    src.process_aerogpu();
    src.poll_pci_intx_lines();

    // Snapshot while the fence is still pending.
    let snap = src.take_snapshot_full().unwrap();

    let mut dst = Machine::new(cfg).unwrap();
    dst.restore_snapshot_bytes(&snap).unwrap();

    let bdf = dst
        .aerogpu_bdf()
        .expect("AeroGPU device should be present");
    let bar0 = dst
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    // Ensure the vblank deadline is re-seeded at the restored timebase before advancing to the
    // boundary, otherwise `tick_vblank` would schedule the *next* period and skip the pending edge.
    dst.process_aerogpu();
    dst.poll_pci_intx_lines();

    assert_eq!(
        read_mmio_u64(
            &mut dst,
            bar0,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
        ),
        0
    );

    // Advance to the vblank edge; the fence should complete after restore.
    // Advance to the vblank edge in the same way the device observes time.
    dst.tick_platform(1);
    dst.process_aerogpu();
    dst.poll_pci_intx_lines();

    assert_eq!(
        read_mmio_u64(
            &mut dst,
            bar0,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
            pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
        ),
        signal_fence
    );
}
