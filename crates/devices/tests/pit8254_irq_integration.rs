use aero_devices::pit8254::{Pit8254, PIT_CH0, PIT_CMD};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

fn program_mode2(pit: &mut Pit8254, divisor: u16) {
    pit.port_write(PIT_CMD, 1, 0x34);
    pit.port_write(PIT_CH0, 1, u32::from(divisor & 0x00FF));
    pit.port_write(PIT_CH0, 1, u32::from(divisor >> 8));
}

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn irq0_callback_is_invoked() {
    let mut pit = Pit8254::new();
    let seen = Arc::new(AtomicU64::new(0));
    let seen_clone = Arc::clone(&seen);
    pit.connect_irq0(move || {
        seen_clone.fetch_add(1, Ordering::Relaxed);
    });

    // Program channel 0: mode2, lobyte/hibyte, divisor=3.
    program_mode2(&mut pit, 3);

    pit.advance_ticks(9);
    assert_eq!(seen.load(Ordering::Relaxed), 3);
}

#[test]
fn pit_irq0_routes_to_pic_in_legacy_mode() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(0, false);
    }

    let mut pit = Pit8254::new();
    pit.connect_irq0_to_platform_interrupts(interrupts.clone());
    program_mode2(&mut pit, 4);

    pit.advance_ticks(4);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x20));

    interrupts.borrow_mut().acknowledge(0x20);
    interrupts.borrow_mut().eoi(0x20);

    pit.advance_ticks(4);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x20));
}

#[test]
fn pit_irq0_respects_isa_irq_override_in_apic_mode() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        // Simulate an MADT ISO remapping IRQ0 -> GSI2.
        ints.set_isa_irq_override(0, 2);
        program_ioapic_entry(&mut ints, 2, 0x31, 0);
    }

    let mut pit = Pit8254::new();
    pit.connect_irq0_to_platform_interrupts(interrupts.clone());
    program_mode2(&mut pit, 5);

    pit.advance_ticks(5);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x31));
    interrupts.borrow_mut().acknowledge(0x31);
    interrupts.borrow_mut().eoi(0x31);

    pit.advance_ticks(5);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x31));
}
