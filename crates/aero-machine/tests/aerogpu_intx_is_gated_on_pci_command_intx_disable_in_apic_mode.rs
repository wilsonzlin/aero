use aero_devices::pci::profile::AEROGPU;
use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode};
use aero_protocol::aerogpu::aerogpu_pci as proto;
use pretty_assertions::assert_eq;

fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    low: u32,
    high: u32,
) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn aerogpu_intx_is_gated_on_pci_command_intx_disable_in_apic_mode() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal/deterministic for interrupt assertions.
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
    let pci_intx = m
        .pci_intx_router()
        .expect("pc platform should provide PCI INTx router");
    let interrupts = m
        .platform_interrupts()
        .expect("pc platform should provide PlatformInterrupts");

    let bdf = AEROGPU.bdf;
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

    // Switch into APIC mode and route the AeroGPU GSI to a known vector.
    const VECTOR: u8 = 0x60;
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        let polarity_low = gsi == 9 || (10..=13).contains(&gsi) || gsi >= 16;
        let mut low = u32::from(VECTOR) | (1 << 15); // level-triggered
        if polarity_low {
            low |= 1 << 13; // active-low
        }
        program_ioapic_entry(&mut ints, gsi, low, 0);

        // Drain any stale pending vectors (defensive).
        while let Some(vec) = InterruptController::get_pending(&*ints) {
            InterruptController::acknowledge(&mut *ints, vec);
            InterruptController::eoi(&mut *ints, vec);
        }
    }

    // Enable MMIO decoding, bus mastering, and set COMMAND.INTX_DISABLE.
    let bar0_base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should provide PCI config ports");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU config function should exist when enable_aerogpu=true");
        // COMMAND.MEM | COMMAND.BME | COMMAND.INTX_DISABLE
        cfg.set_command((1 << 1) | (1 << 2) | (1 << 10));
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };
    assert_ne!(bar0_base, 0, "AeroGPU BAR0 should be assigned by BIOS POST");

    // Trigger a vblank IRQ while INTx delivery is disabled in the PCI command register.
    m.write_physical_u32(
        bar0_base + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_ENABLE),
        1,
    );
    let period_ns = u64::from(m.read_physical_u32(
        bar0_base + u64::from(proto::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS),
    ));
    assert_ne!(period_ns, 0, "test requires vblank pacing to be active");
    m.write_physical_u32(
        bar0_base + u64::from(proto::AEROGPU_MMIO_REG_IRQ_ENABLE),
        proto::AEROGPU_IRQ_SCANOUT_VBLANK,
    );

    // Advance to the next vblank edge so the device latches the vblank IRQ.
    m.tick_platform(period_ns);

    let irq_status = m.read_physical_u32(bar0_base + u64::from(proto::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_ne!(
        irq_status & proto::AEROGPU_IRQ_SCANOUT_VBLANK,
        0,
        "expected vblank IRQ to latch in IRQ_STATUS even while INTx is disabled"
    );

    // With INTX_DISABLE set, polling should not deliver a platform interrupt vector.
    m.poll_pci_intx_lines();
    assert_eq!(
        InterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // Clear COMMAND.INTX_DISABLE. The already-latched IRQ should now assert INTx and be routed via
    // the IOAPIC.
    {
        let pci_cfg = m.pci_config_ports().unwrap();
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg.bus_mut().device_config_mut(bdf).unwrap();
        let command = cfg.command();
        cfg.set_command(command & !(1 << 10));
    }
    m.poll_pci_intx_lines();
    assert_eq!(
        InterruptController::get_pending(&*interrupts.borrow()),
        Some(VECTOR)
    );
}
