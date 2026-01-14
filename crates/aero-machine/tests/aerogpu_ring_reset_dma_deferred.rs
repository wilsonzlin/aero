use aero_devices::pci::profile::AEROGPU_BAR0_INDEX;
use aero_machine::{Machine, MachineConfig};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
use pretty_assertions::assert_eq;

#[test]
fn aerogpu_ring_reset_dma_is_deferred_until_bus_mastering_is_enabled() {
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

    let mut vm = Machine::new(cfg).unwrap();

    let bdf = vm
        .aerogpu_bdf()
        .expect("AeroGPU device should be present");
    let bar0 = vm
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");

    // Enable PCI MMIO decode but leave bus mastering disabled.
    {
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command((cfg.command() | (1 << 1)) & !(1 << 2));
    }

    // Ring header: put head behind tail so we can observe the reset DMA synchronization.
    let ring_gpa = 0x10000u64;
    let fence_gpa = 0x20000u64;

    let entry_count = 8u32;
    let entry_stride_bytes = ring::AerogpuSubmitDesc::SIZE_BYTES as u32;
    let ring_size_bytes =
        ring::AerogpuRingHeader::SIZE_BYTES as u32 + entry_count * entry_stride_bytes;

    vm.write_physical_u32(ring_gpa, ring::AEROGPU_RING_MAGIC);
    vm.write_physical_u32(ring_gpa + 4, pci::AEROGPU_ABI_VERSION_U32);
    vm.write_physical_u32(ring_gpa + 8, ring_size_bytes);
    vm.write_physical_u32(ring_gpa + 12, entry_count);
    vm.write_physical_u32(ring_gpa + 16, entry_stride_bytes);
    vm.write_physical_u32(ring_gpa + 20, 0); // flags
    vm.write_physical_u32(ring_gpa + 24, 1); // head
    vm.write_physical_u32(ring_gpa + 28, 3); // tail

    // Dirty the fence page so we can ensure the reset overwrites once DMA is enabled.
    vm.write_physical_u32(fence_gpa, 0xDEAD_BEEF);
    vm.write_physical_u64(fence_gpa + 8, 999);

    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );
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

    // Request a ring reset while DMA is disabled.
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_RESET | pci::AEROGPU_RING_CONTROL_ENABLE,
    );

    // Tick once with COMMAND.BME clear: DMA must not run yet.
    vm.process_aerogpu();
    assert_eq!(vm.read_physical_u32(ring_gpa + 24), 1);
    assert_eq!(vm.read_physical_u32(fence_gpa), 0xDEAD_BEEF);
    assert_eq!(vm.read_physical_u64(fence_gpa + 8), 999);

    // Enable bus mastering: the pending reset DMA should complete.
    {
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command(cfg.command() | (1 << 2));
    }
    vm.process_aerogpu();
    assert_eq!(vm.read_physical_u32(ring_gpa + 24), 3);
    assert_eq!(
        vm.read_physical_u32(fence_gpa),
        ring::AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(vm.read_physical_u64(fence_gpa + 8), 0);
}
