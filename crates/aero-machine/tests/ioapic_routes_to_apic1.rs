use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::PlatformInterruptMode;

#[test]
fn ioapic_redirection_destination_apic1_delivers_to_lapic1() {
    let cfg = MachineConfig {
        cpu_count: 2,
        enable_pc_platform: true,
        // Keep the machine minimal; this test only needs the interrupt controller complex.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Enable APIC mode so IOAPIC routes into LAPICs (not legacy PIC).
    let interrupts = m
        .platform_interrupts()
        .expect("PC platform should be enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    let gsi = 10u32;
    let vector = 0x40u8;

    // Route GSI10 -> vector 0x40, unmasked, edge-triggered, destination APIC ID 1.
    //
    // GSI10 is part of the default PCI INTx wiring and is active-low, so program the IOAPIC
    // polarity bit accordingly (bit 13).
    let low = u32::from(vector) | (1 << 13);
    let high = 1u32 << 24; // destination APIC ID in bits 56..63 of the redirection entry.

    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;

    // Program the IOAPIC via the guest-visible MMIO window.
    m.write_physical_u32(IOAPIC_MMIO_BASE, redtbl_low);
    m.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, low);
    m.write_physical_u32(IOAPIC_MMIO_BASE, redtbl_high);
    m.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, high);

    // Sanity check: no pending interrupts before assertion.
    assert_eq!(interrupts.borrow().get_pending_for_apic(0), None);
    assert_eq!(interrupts.borrow().get_pending_for_apic(1), None);

    // Assert then deassert the GSI. For an edge-triggered entry, this should result in a single
    // interrupt delivery to the destination LAPIC.
    m.raise_gsi(gsi);
    m.lower_gsi(gsi);

    assert_eq!(
        interrupts.borrow().get_pending_for_apic(0),
        None,
        "IOAPIC incorrectly routed the interrupt to LAPIC0 (BSP)"
    );
    assert_eq!(
        interrupts.borrow().get_pending_for_apic(1),
        Some(vector),
        "IOAPIC did not route the interrupt to LAPIC1 (APIC ID 1)"
    );
}
