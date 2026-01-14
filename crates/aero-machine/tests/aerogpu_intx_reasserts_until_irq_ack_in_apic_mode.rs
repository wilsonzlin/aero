use aero_devices::pci::profile::AEROGPU;
use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode};
use aero_protocol::aerogpu::{aerogpu_pci as pci, aerogpu_ring as ring};
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
fn aerogpu_intx_reasserts_until_irq_ack_in_apic_mode() {
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

    // Switch to APIC/IOAPIC delivery and route the AeroGPU GSI to a deterministic vector.
    const VECTOR: u8 = 0x60;
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // Match the IOAPIC's default PC wiring assumptions when programming the redirection entry.
        let polarity_low = gsi == 9 || (10..=13).contains(&gsi) || gsi >= 16;
        let mut low = u32::from(VECTOR) | (1 << 15); // level-triggered
        if polarity_low {
            low |= 1 << 13; // active-low
        }
        program_ioapic_entry(&mut ints, gsi, low, 0);

        // Drain any stale pending vectors.
        while let Some(vec) = InterruptController::get_pending(&*ints) {
            InterruptController::acknowledge(&mut *ints, vec);
            InterruptController::eoi(&mut *ints, vec);
        }
    }

    // Enable AeroGPU BAR0 MMIO decode + bus mastering so the ring transport can DMA.
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

    // Minimal ring + fence submission that signals fence=42 and requests an IRQ.
    let ring_gpa: u64 = 0x10000;
    let fence_gpa: u64 = 0x20000;

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

    // Submit descriptor in slot 0.
    let desc_gpa = ring_gpa + ring::AerogpuRingHeader::SIZE_BYTES as u64;
    m.write_physical_u32(desc_gpa + 0, ring::AerogpuSubmitDesc::SIZE_BYTES as u32); // desc_size_bytes
    m.write_physical_u32(desc_gpa + 4, 0); // flags
    m.write_physical_u64(desc_gpa + 48, 42); // signal_fence

    // Program BAR0 registers.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_LO),
        fence_gpa as u32,
    );
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FENCE_GPA_HI),
        (fence_gpa >> 32) as u32,
    );
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
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE,
    );

    // Ring doorbell and let the device process the submission.
    m.write_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_DOORBELL), 1);
    m.process_aerogpu();
    m.poll_pci_intx_lines();

    // Initial IOAPIC delivery.
    {
        let mut ints = interrupts.borrow_mut();
        assert_eq!(InterruptController::get_pending(&*ints), Some(VECTOR));
        InterruptController::acknowledge(&mut *ints, VECTOR);

        // If the guest issues an EOI without clearing the device's interrupt cause, the IOAPIC
        // should re-deliver the level-triggered interrupt (Remote-IRR is cleared on EOI).
        InterruptController::eoi(&mut *ints, VECTOR);
        assert_eq!(InterruptController::get_pending(&*ints), Some(VECTOR));
        InterruptController::acknowledge(&mut *ints, VECTOR);
    }

    // Clear the device's IRQ status bit and sync INTx line levels so the IOAPIC input pin is
    // deasserted before EOI.
    m.write_physical_u32(
        bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ACK),
        pci::AEROGPU_IRQ_FENCE,
    );
    m.poll_pci_intx_lines();

    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    assert_eq!(irq_status & pci::AEROGPU_IRQ_FENCE, 0);
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "expected INTx line to deassert after IRQ_ACK"
    );

    // Now EOI should *not* cause redelivery because the line is no longer asserted.
    {
        let mut ints = interrupts.borrow_mut();
        InterruptController::eoi(&mut *ints, VECTOR);
        assert_eq!(InterruptController::get_pending(&*ints), None);
    }
}

