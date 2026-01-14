use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

fn read_mmio_u64(m: &mut Machine, base: u64, lo: u32, hi: u32) -> u64 {
    let lo = m.read_physical_u32(base + u64::from(lo));
    let hi = m.read_physical_u32(base + u64::from(hi));
    u64::from(lo) | (u64::from(hi) << 32)
}

#[test]
fn aerogpu_snapshot_preserves_vblank_seq_and_completed_fence() {
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg.clone()).unwrap();
    let bdf = vm.aerogpu_bdf().expect("AeroGPU device should be present");
    let bar0 = vm
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    // Enable PCI MMIO decode + bus mastering so BAR0 accesses route to the device and it can DMA
    // ring/fence state.
    {
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command(cfg.command() | (1 << 1) | (1 << 2));
    }

    // ---------------------------------------------------------------------
    // 1) Drive the ring transport so COMPLETED_FENCE becomes non-zero.
    // ---------------------------------------------------------------------
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;
    let cmd_gpa = 0x30000u64;
    vm.write_physical(cmd_gpa, &[0xDE, 0xAD, 0xBE, 0xEF]);

    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    // Ring header (head=0, tail=1).
    vm.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    vm.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    vm.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    vm.write_physical_u32(ring_gpa + 12, entry_count);
    vm.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    vm.write_physical_u32(ring_gpa + 20, 0); // flags
    vm.write_physical_u32(ring_gpa + 24, 0); // head
    vm.write_physical_u32(ring_gpa + 28, 1); // tail

    // Submit desc in slot 0.
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    let signal_fence = 0x1234_5678_9ABC_DEF0u64;
    vm.write_physical_u32(desc_gpa, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    vm.write_physical_u32(desc_gpa + 4, 0); // flags
    vm.write_physical_u32(desc_gpa + 8, 0); // context_id
    vm.write_physical_u32(desc_gpa + 12, ring::AEROGPU_ENGINE_0); // engine_id
    vm.write_physical_u64(desc_gpa + 16, cmd_gpa);
    vm.write_physical_u32(desc_gpa + 24, 4); // cmd_size_bytes
    vm.write_physical_u32(desc_gpa + 28, 0);
    vm.write_physical_u64(desc_gpa + 32, 0); // alloc_table_gpa
    vm.write_physical_u32(desc_gpa + 40, 0); // alloc_table_size_bytes
    vm.write_physical_u32(desc_gpa + 44, 0);
    vm.write_physical_u64(desc_gpa + 48, signal_fence);
    vm.write_physical_u64(desc_gpa + 56, 0);

    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_LO),
        ring_gpa as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_GPA_HI),
        (ring_gpa >> 32) as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        ring_size_bytes,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );

    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    vm.process_aerogpu();

    // Drain submissions so the VM is in a quiescent state before snapshotting.
    let subs = vm.aerogpu_drain_submissions();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].signal_fence, signal_fence);

    let completed_fence_before = read_mmio_u64(
        &mut vm,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence_before, signal_fence);

    // ---------------------------------------------------------------------
    // 2) Advance vblank state so VBLANK_SEQ/VBLANK_TIME become non-zero.
    // ---------------------------------------------------------------------
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    let vblank_period_before =
        vm.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS));
    assert!(vblank_period_before > 0);

    // Advance enough time to cross one vblank interval.
    vm.tick_platform(u64::from(vblank_period_before));

    let vblank_seq_before = read_mmio_u64(
        &mut vm,
        bar0,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI,
    );
    assert!(vblank_seq_before > 0);

    let vblank_time_before = read_mmio_u64(
        &mut vm,
        bar0,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI,
    );
    assert!(vblank_time_before > 0);

    // ---------------------------------------------------------------------
    // Snapshot + restore.
    // ---------------------------------------------------------------------
    let snap = vm.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.reset();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let bdf = restored
        .aerogpu_bdf()
        .expect("AeroGPU device should be present");
    let bar0 = restored
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    let completed_fence_after = read_mmio_u64(
        &mut restored,
        bar0,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );
    assert_eq!(completed_fence_after, completed_fence_before);

    let vblank_seq_after = read_mmio_u64(
        &mut restored,
        bar0,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI,
    );
    assert_eq!(vblank_seq_after, vblank_seq_before);

    let vblank_time_after = read_mmio_u64(
        &mut restored,
        bar0,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI,
    );
    assert_eq!(vblank_time_after, vblank_time_before);

    let vblank_period_after = restored
        .read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS));
    assert_eq!(vblank_period_after, vblank_period_before);
}
