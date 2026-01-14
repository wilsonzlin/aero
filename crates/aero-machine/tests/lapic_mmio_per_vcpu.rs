use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptInput, MsiMessage, MsiTrigger, PlatformInterruptMode, PlatformInterrupts,
};
use std::cell::RefCell;
use std::rc::Rc;

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

fn assert_ioapic_delivers_to_apic(
    interrupts: &Rc<RefCell<PlatformInterrupts>>,
    cpu_count: u8,
    dest_apic_id: u8,
    vector: u8,
) {
    let gsi = 1u32;
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // GSI1 -> vector, level-triggered, unmasked, destination APIC ID `dest_apic_id`.
        program_ioapic_entry(
            &mut *ints,
            gsi,
            u32::from(vector) | (1 << 15),
            u32::from(dest_apic_id) << 24,
        );

        ints.raise_irq(InterruptInput::Gsi(gsi));
    }

    assert_eq!(
        interrupts.borrow().get_pending_for_apic(dest_apic_id),
        Some(vector)
    );
    for apic_id in 0..cpu_count {
        if apic_id == dest_apic_id {
            continue;
        }
        assert_eq!(interrupts.borrow().get_pending_for_apic(apic_id), None);
    }

    // Typical level-triggered behavior: device deasserts the line before EOI.
    {
        let mut ints = interrupts.borrow_mut();
        ints.acknowledge_for_apic(dest_apic_id, vector);
        ints.lower_irq(InterruptInput::Gsi(gsi));
        ints.eoi_for_apic(dest_apic_id, vector);
    }

    for apic_id in 0..cpu_count {
        assert_eq!(interrupts.borrow().get_pending_for_apic(apic_id), None);
    }
}

fn msi_message(dest_apic_id: u8, vector: u8) -> MsiMessage {
    MsiMessage {
        // xAPIC physical destination encoding.
        address: 0xFEE0_0000u64 | (u64::from(dest_apic_id) << 12),
        data: vector as u16,
    }
}

fn assert_msi_delivers_to_apic(
    interrupts: &Rc<RefCell<PlatformInterrupts>>,
    cpu_count: u8,
    dest_apic_id: u8,
    vector: u8,
) {
    // MSI delivery is APIC-local; make sure we're in APIC mode so `get_pending_for_apic` observes
    // LAPIC IRR state rather than the legacy PIC.
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    let mut sink = interrupts.clone();
    sink.trigger_msi(msi_message(dest_apic_id, vector));

    assert_eq!(
        interrupts.borrow().get_pending_for_apic(dest_apic_id),
        Some(vector)
    );
    for apic_id in 0..cpu_count {
        if apic_id == dest_apic_id {
            continue;
        }
        assert_eq!(interrupts.borrow().get_pending_for_apic(apic_id), None);
    }

    {
        let mut ints = interrupts.borrow_mut();
        ints.acknowledge_for_apic(dest_apic_id, vector);
        ints.eoi_for_apic(dest_apic_id, vector);
    }

    for apic_id in 0..cpu_count {
        assert_eq!(interrupts.borrow().get_pending_for_apic(apic_id), None);
    }
}

#[test]
fn lapic_mmio_is_routed_per_vcpu() {
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

    // LAPIC ID register (REG_ID) is at offset 0x20; the APIC ID is in bits 24..31.
    let id0 = m.read_lapic_u32(0, 0x20) >> 24;
    let id1 = m.read_lapic_u32(1, 0x20) >> 24;
    assert_eq!(id0, 0);
    assert_eq!(id1, 1);
}

#[test]
fn lapic_mmio_cpu_ids_persist_after_machine_reset() {
    let cfg = MachineConfig {
        cpu_count: 4,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    for cpu in 0..4 {
        let id = m.read_lapic_u32(cpu, 0x20) >> 24;
        assert_eq!(id, cpu as u32);
    }

    let interrupts = m.platform_interrupts().unwrap();
    assert_ioapic_delivers_to_apic(&interrupts, 4, 3, 0x44);
    assert_msi_delivers_to_apic(&interrupts, 4, 3, 0x45);

    // Ensure that `Machine::reset()` does not accidentally collapse a multi-LAPIC topology back to a
    // single BSP-only interrupt complex.
    m.reset();

    for cpu in 0..4 {
        let id = m.read_lapic_u32(cpu, 0x20) >> 24;
        assert_eq!(id, cpu as u32);
    }

    assert_ioapic_delivers_to_apic(&interrupts, 4, 3, 0x44);
    assert_msi_delivers_to_apic(&interrupts, 4, 3, 0x45);
}
