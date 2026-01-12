use aero_devices::pci::profile::{SATA_AHCI_ICH9, USB_UHCI_PIIX3};
use aero_pc_platform::{PcPlatform, ResetEvent};
use aero_platform::interrupts::InterruptController;

fn mmio_read_u32(mem: &mut aero_platform::memory::MemoryBus, addr: u64) -> u32 {
    let mut buf = [0u8; 4];
    mem.read_physical(addr, &mut buf);
    u32::from_le_bytes(buf)
}

fn mmio_write_u32(mem: &mut aero_platform::memory::MemoryBus, addr: u64, value: u32) {
    mem.write_physical(addr, &value.to_le_bytes());
}

fn mmio_read_u64(mem: &mut aero_platform::memory::MemoryBus, addr: u64) -> u64 {
    let mut buf = [0u8; 8];
    mem.read_physical(addr, &mut buf);
    u64::from_le_bytes(buf)
}

fn mmio_write_u64(mem: &mut aero_platform::memory::MemoryBus, addr: u64, value: u64) {
    mem.write_physical(addr, &value.to_le_bytes());
}

fn rtc_read_reg(p: &mut PcPlatform, reg: u8) -> u8 {
    p.io.write_u8(0x70, reg);
    p.io.read_u8(0x71)
}

fn rtc_write_reg(p: &mut PcPlatform, reg: u8, value: u8) {
    p.io.write_u8(0x70, reg);
    p.io.write_u8(0x71, value);
}

fn pci_cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn pci_read_u16(p: &mut PcPlatform, bdf: aero_devices::pci::PciBdf, offset: u8) -> u16 {
    p.io.write(0xCF8, 4, pci_cfg_addr(bdf.bus, bdf.device, bdf.function, offset));
    p.io.read(0xCFC + u16::from(offset & 0x3), 2) as u16
}

fn pci_read_u32(p: &mut PcPlatform, bdf: aero_devices::pci::PciBdf, offset: u8) -> u32 {
    p.io.write(0xCF8, 4, pci_cfg_addr(bdf.bus, bdf.device, bdf.function, offset));
    p.io.read(0xCFC, 4)
}

fn pci_write_u16(p: &mut PcPlatform, bdf: aero_devices::pci::PciBdf, offset: u8, value: u16) {
    p.io.write(0xCF8, 4, pci_cfg_addr(bdf.bus, bdf.device, bdf.function, offset));
    p.io
        .write(0xCFC + u16::from(offset & 0x3), 2, value as u32);
}

fn pci_write_u32(p: &mut PcPlatform, bdf: aero_devices::pci::PciBdf, offset: u8, value: u32) {
    p.io.write(0xCF8, 4, pci_cfg_addr(bdf.bus, bdf.device, bdf.function, offset));
    p.io.write(0xCFC, 4, value);
}

fn ioapic_read_reg(p: &mut PcPlatform, reg: u32) -> u32 {
    mmio_write_u32(&mut p.memory, aero_interrupts::apic::IOAPIC_MMIO_BASE, reg);
    mmio_read_u32(
        &mut p.memory,
        aero_interrupts::apic::IOAPIC_MMIO_BASE + 0x10,
    )
}

#[test]
fn reset_is_deterministic_and_preserves_ram_allocation() {
    let ram_size = 8 * 1024 * 1024;

    let mut plat = PcPlatform::new(ram_size);

    // Capture the underlying guest RAM allocation address so we can verify it is preserved.
    let ram_ptr_before = plat
        .memory
        .ram()
        .get_slice(0, 1)
        .expect("DenseMemory should expose contiguous slices")
        .as_ptr();

    // Generate a reset event so we can ensure `reset()` clears it.
    plat.io.write_u8(
        aero_devices::reset_ctrl::RESET_CTRL_PORT,
        aero_devices::reset_ctrl::RESET_CTRL_RESET_VALUE,
    );
    assert_eq!(plat.take_reset_events(), vec![ResetEvent::System]);
    plat.io.write_u8(
        aero_devices::reset_ctrl::RESET_CTRL_PORT,
        aero_devices::reset_ctrl::RESET_CTRL_RESET_VALUE,
    );

    // Mutate chipset A20 via port 0x92.
    plat.io.write_u8(0x92, 0x02);

    // Mutate IMCR state (switch to APIC mode).
    plat.io.write_u8(0x22, 0x70);
    plat.io.write_u8(0x23, 0x01);

    // Program PIT channel 0 (mode 2, divisor 0x2211).
    plat.io.write_u8(0x43, 0x34);
    plat.io.write_u8(0x40, 0x11);
    plat.io.write_u8(0x40, 0x22);

    // Program RTC Status B (enable update-ended interrupts).
    rtc_write_reg(&mut plat, 0x0B, 0x02 | 0x10);

    // Enable HPET and bump the main counter.
    mmio_write_u64(
        &mut plat.memory,
        aero_devices::hpet::HPET_MMIO_BASE + 0x10,
        0x1,
    );
    mmio_write_u64(
        &mut plat.memory,
        aero_devices::hpet::HPET_MMIO_BASE + 0xF0,
        0x1234,
    );

    // Mutate i8042 command byte (disable IRQs/translation).
    plat.io.write_u8(0x64, 0x60);
    plat.io.write_u8(0x60, 0x00);

    // Mutate ACPI enable state via SMI_CMD.
    plat.io.write_u8(aero_devices::acpi_pm::DEFAULT_SMI_CMD_PORT, 0xA0);

    // Mutate PCI config mechanism and AHCI config space.
    let ahci_bdf = SATA_AHCI_ICH9.bdf;
    // Disable memory decoding.
    pci_write_u16(&mut plat, ahci_bdf, 0x04, 0x0000);
    // Stomp BAR5 (MMIO).
    pci_write_u32(&mut plat, ahci_bdf, 0x24, 0xDEAD_0000);
    // Leave a non-zero CF8 latch value behind.
    plat.io.write(0xCF8, 4, 0x8000_0000);

    // Mutate PCI INTx router bookkeeping by asserting INTA# and leaving it asserted.
    let uhci_bdf = USB_UHCI_PIIX3.bdf;
    plat.pci_intx.assert_intx(
        uhci_bdf,
        aero_devices::pci::PciInterruptPin::IntA,
        &mut *plat.interrupts.borrow_mut(),
    );

    // Advance time so reset must restore the deterministic clock baseline.
    plat.tick(5_000);

    plat.reset();

    // RAM allocation must be preserved.
    let ram_ptr_after = plat
        .memory
        .ram()
        .get_slice(0, 1)
        .expect("DenseMemory should expose contiguous slices")
        .as_ptr();
    assert_eq!(ram_ptr_before, ram_ptr_after);

    // Reset should clear reset events.
    assert!(plat.take_reset_events().is_empty());

    // Compare observable I/O/MMIO state against a freshly constructed platform.
    let mut fresh = PcPlatform::new(ram_size);

    // Chipset A20 reset.
    assert_eq!(plat.io.read_u8(0x92), fresh.io.read_u8(0x92));

    // IMCR reset (legacy PIC mode).
    plat.io.write_u8(0x22, 0x70);
    fresh.io.write_u8(0x22, 0x70);
    assert_eq!(plat.io.read_u8(0x23), fresh.io.read_u8(0x23));

    // PIT reset: channel reads should match.
    assert_eq!(plat.io.read_u8(0x40), fresh.io.read_u8(0x40));

    // RTC reset: index port and Status B should match.
    assert_eq!(plat.io.read_u8(0x70), fresh.io.read_u8(0x70));
    assert_eq!(rtc_read_reg(&mut plat, 0x0B), rtc_read_reg(&mut fresh, 0x0B));

    // i8042 reset: status port and command byte should match.
    assert_eq!(plat.io.read_u8(0x64), fresh.io.read_u8(0x64));
    plat.io.write_u8(0x64, 0x20);
    fresh.io.write_u8(0x64, 0x20);
    assert_eq!(plat.io.read_u8(0x60), fresh.io.read_u8(0x60));

    // ACPI PM reset: PM1a_CNT should match (ACPI disabled).
    assert_eq!(
        plat.io.read(aero_devices::acpi_pm::DEFAULT_PM1A_CNT_BLK, 2),
        fresh
            .io
            .read(aero_devices::acpi_pm::DEFAULT_PM1A_CNT_BLK, 2)
    );

    // LAPIC: Spurious Interrupt Vector Register should match reset value (enabled, vector 0xFF).
    assert_eq!(
        mmio_read_u32(&mut plat.memory, aero_interrupts::apic::LAPIC_MMIO_BASE + 0xF0),
        mmio_read_u32(
            &mut fresh.memory,
            aero_interrupts::apic::LAPIC_MMIO_BASE + 0xF0
        )
    );

    // IOAPIC: ID and version registers should match.
    assert_eq!(ioapic_read_reg(&mut plat, 0x00), ioapic_read_reg(&mut fresh, 0x00));
    assert_eq!(ioapic_read_reg(&mut plat, 0x01), ioapic_read_reg(&mut fresh, 0x01));

    // HPET reset: key registers should match.
    // Enable A20 so the HPET base does not alias with the IOAPIC base (differs by bit20).
    plat.io.write_u8(0x92, 0x02);
    fresh.io.write_u8(0x92, 0x02);
    assert_eq!(
        mmio_read_u64(
            &mut plat.memory,
            aero_devices::hpet::HPET_MMIO_BASE + 0x10
        ),
        mmio_read_u64(
            &mut fresh.memory,
            aero_devices::hpet::HPET_MMIO_BASE + 0x10
        )
    );
    assert_eq!(
        mmio_read_u64(
            &mut plat.memory,
            aero_devices::hpet::HPET_MMIO_BASE + 0xF0
        ),
        mmio_read_u64(
            &mut fresh.memory,
            aero_devices::hpet::HPET_MMIO_BASE + 0xF0
        )
    );

    // Disable A20 again so the legacy 1MiB wrap behaviour is observable below.
    plat.io.write_u8(0x92, 0x00);

    // PCI config: CF8 latch + AHCI BAR5 + AHCI command register should match a fresh BIOS POST.
    assert_eq!(plat.io.read(0xCF8, 4), fresh.io.read(0xCF8, 4));
    assert_eq!(
        pci_read_u16(&mut plat, ahci_bdf, 0x04),
        pci_read_u16(&mut fresh, ahci_bdf, 0x04)
    );
    assert_eq!(
        pci_read_u32(&mut plat, ahci_bdf, 0x24),
        pci_read_u32(&mut fresh, ahci_bdf, 0x24)
    );

    // Verify A20 gating behavior is restored (1MiB wrap when disabled).
    plat.memory.write_u8(0, 0x11);
    plat.memory.write_u8(0x0010_0000, 0x22);
    assert_eq!(plat.memory.read_u8(0), 0x22);

    // Verify PCI INTx router bookkeeping does not retain prior assertion state.
    //
    // If the router were not reset, `assert_intx` would see the source already asserted (from the
    // pre-reset assertion above) and would not re-drive the platform GSI, leaving the PIC with no
    // pending IRQ despite the call.
    assert_eq!(plat.interrupts.borrow().get_pending(), None);
    let gsi = plat
        .pci_intx
        .gsi_for_intx(uhci_bdf, aero_devices::pci::PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("PCI INTx GSI should fit in an ISA IRQ number");
    {
        let mut interrupts = plat.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false); // cascade
        interrupts.pic_mut().set_masked(irq, false);
    }
    assert_eq!(
        plat.interrupts.borrow().pic().get_pending_vector(),
        None,
        "PIC should have no pending IRQs after reset"
    );
    plat.pci_intx.assert_intx(
        uhci_bdf,
        aero_devices::pci::PciInterruptPin::IntA,
        &mut *plat.interrupts.borrow_mut(),
    );
    let pending_vec = plat
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("asserting INTA# after reset should generate a pending PIC interrupt");
    let pending_irq = plat
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending_vec)
        .expect("pending PIC vector should decode back to an IRQ number");
    assert_eq!(pending_irq, irq);
}
