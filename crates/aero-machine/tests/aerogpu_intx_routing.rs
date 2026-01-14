use aero_devices::pci::{profile, PciInterruptPin};
use aero_devices_gpu::regs::{irq_bits, mmio};
use aero_machine::{Machine, MachineConfig};

#[test]
fn aerogpu_intx_level_routes_into_platform_interrupts_and_deasserts_on_ack() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal and deterministic for interrupt routing tests.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_uhci: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");

    let bdf = profile::AEROGPU.bdf;
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("aerogpu PCI config missing");

        // Ensure MMIO decoding behaves like real PCI hardware (COMMAND.MEM + COMMAND.BME).
        cfg.set_command((1 << 1) | (1 << 2));

        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };
    assert_ne!(
        bar0_base, 0,
        "expected AeroGPU BAR0 base assigned by BIOS POST"
    );

    // Enable vblank delivery and scanout so `tick_platform` can latch a vblank IRQ into IRQ_STATUS.
    m.write_physical_u32(bar0_base + mmio::SCANOUT0_ENABLE, 1);
    m.write_physical_u32(bar0_base + mmio::IRQ_ENABLE, irq_bits::SCANOUT_VBLANK);

    let period_ns = u64::from(m.read_physical_u32(
        bar0_base + mmio::SCANOUT0_VBLANK_PERIOD_NS,
    ));
    assert_ne!(period_ns, 0, "test requires vblank pacing to be active");

    // Advance to the next vblank edge so the device latches the vblank IRQ, then synchronize INTx
    // sources into the platform interrupt controller.
    m.tick_platform(period_ns);
    m.poll_pci_intx_lines();

    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to assert GSI {gsi}"
    );

    // Verify COMMAND.INTX_DISABLE gating through the canonical PCI command register.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("aerogpu PCI config missing");
        cfg.set_command(cfg.command() | (1 << 10));
    }
    m.poll_pci_intx_lines();
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to be gated by COMMAND.INTX_DISABLE"
    );

    // Re-enable INTx. Since IRQ_STATUS is still set, the line should reassert.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("aerogpu PCI config missing");
        cfg.set_command(cfg.command() & !(1 << 10));
    }
    m.poll_pci_intx_lines();
    assert!(
        interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to reassert after clearing COMMAND.INTX_DISABLE"
    );

    // ACK the vblank interrupt and ensure INTx deasserts.
    m.write_physical_u32(bar0_base + mmio::IRQ_ACK, irq_bits::SCANOUT_VBLANK);
    m.poll_pci_intx_lines();

    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "expected AeroGPU INTx to deassert after IRQ_ACK"
    );
}
