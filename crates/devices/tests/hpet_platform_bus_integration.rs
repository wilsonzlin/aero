use aero_devices::clock::ManualClock;
use aero_devices::hpet::{Hpet, HpetMmio, HPET_MMIO_BASE, HPET_MMIO_SIZE};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};
use memory::Bus;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn hpet_mmio_is_accessible_via_memory_bus_and_delivers_interrupt_via_ioapic() {
    let clock = ManualClock::new();

    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Route GSI2 (HPET timer0 legacy route) -> vector 0x42.
    let gsi = 2u32;
    let vector = 0x42u8;
    let redir_low_index = 0x10u8 + (2 * gsi as u8);
    let redir_high_index = redir_low_index + 1;

    interrupts
        .borrow_mut()
        .ioapic_mmio_write(0x00, redir_low_index as u32);
    interrupts
        .borrow_mut()
        .ioapic_mmio_write(0x10, vector as u32);

    interrupts
        .borrow_mut()
        .ioapic_mmio_write(0x00, redir_high_index as u32);
    interrupts.borrow_mut().ioapic_mmio_write(0x10, 0);

    let hpet = Hpet::new_default(clock.clone());
    let hpet_mmio = HpetMmio::new(hpet, interrupts.clone());

    let mut bus = Bus::new(0x4000);
    bus.map_mmio(HPET_MMIO_BASE, HPET_MMIO_SIZE, Box::new(hpet_mmio));

    // Enable HPET.
    bus.write(HPET_MMIO_BASE + 0x010, 8, 1);

    // Enable Timer0 interrupt and arm comparator at tick=3.
    let timer0_cfg = bus.read(HPET_MMIO_BASE + 0x100, 8);
    bus.write(HPET_MMIO_BASE + 0x100, 8, timer0_cfg | (1 << 2));
    bus.write(HPET_MMIO_BASE + 0x108, 8, 3);

    clock.advance_ns(300);
    let _ = bus.read(HPET_MMIO_BASE + 0x0F0, 8);

    assert_eq!(interrupts.borrow().get_pending(), Some(vector));
    assert_ne!(bus.read(HPET_MMIO_BASE + 0x020, 8) & 1, 0);

    // Clear timer0 interrupt status (W1C).
    bus.write(HPET_MMIO_BASE + 0x020, 8, 1);
    assert_eq!(bus.read(HPET_MMIO_BASE + 0x020, 8) & 1, 0);
}
