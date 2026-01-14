mod aerogpu_intx_helpers;

use aero_devices::pci::profile::AEROGPU;
use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode};
use aero_protocol::aerogpu::aerogpu_pci as proto;
use pretty_assertions::assert_eq;

use aerogpu_intx_helpers::{ioapic_default_polarity_low, program_ioapic_entry};

#[test]
fn aerogpu_intx_delivers_ioapic_vector_in_apic_mode() {
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

    // Route the AeroGPU GSI to a deterministic APIC vector, and switch the platform interrupt
    // delivery mode away from the legacy PIC.
    const VECTOR: u8 = 0x60;
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // Mirror the IOAPIC's default PC wiring assumptions (active-low for SCI, PCI INTx, and GSIs
        // >= 16). The IOAPIC model applies both board wiring and guest-programmable polarity.
        let polarity_low = ioapic_default_polarity_low(gsi);
        let mut low = u32::from(VECTOR) | (1 << 15); // level-triggered
        if polarity_low {
            low |= 1 << 13; // active-low
        }
        program_ioapic_entry(&mut ints, gsi, low, 0);

        // Ensure the LAPIC has no stale pending vectors before the IRQ we trigger below.
        while let Some(vec) = InterruptController::get_pending(&*ints) {
            InterruptController::acknowledge(&mut *ints, vec);
            InterruptController::eoi(&mut *ints, vec);
        }
    }

    // Enable PCI MMIO decoding so BAR0 MMIO access is routed to the device model.
    let bar0_base = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should provide PCI config ports");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU config function should exist when enable_aerogpu=true");
        // COMMAND.MEM | COMMAND.BME
        cfg.set_command((1 << 1) | (1 << 2));
        cfg.bar_range(0).expect("AeroGPU BAR0 missing").base
    };
    assert_ne!(bar0_base, 0, "AeroGPU BAR0 should be assigned by BIOS POST");

    // Enable scanout to start vblank scheduling, then enable vblank IRQ delivery.
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

    // Advance to the next vblank edge so the device latches the vblank IRQ, then synchronize INTx
    // sources into the platform interrupt controller.
    m.tick_platform(period_ns);
    m.poll_pci_intx_lines();

    assert_eq!(
        InterruptController::get_pending(&*interrupts.borrow()),
        Some(VECTOR)
    );
}
