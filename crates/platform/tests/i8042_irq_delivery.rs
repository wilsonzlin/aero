use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::i8042::I8042Ports;
use aero_platform::interrupts::{
    InterruptController, IoApicRedirectionEntry, PlatformInterruptMode, PlatformInterrupts, TriggerMode,
};

#[test]
fn i8042_keyboard_irq1_delivers_pic_vector() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.set_mode(PlatformInterruptMode::LegacyPic);
    }

    let i8042 = I8042Ports::new();
    i8042.connect_irqs_to_platform_interrupts(interrupts.clone());
    let ctrl = i8042.controller();

    ctrl.borrow_mut().inject_browser_key("KeyA", true);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x21));

    // Mimic the guest interrupt handler: read scancode then EOI.
    ctrl.borrow_mut().read_port(0x60);
    {
        let mut ints = interrupts.borrow_mut();
        ints.acknowledge(0x21);
        ints.eoi(0x21);
    }

    ctrl.borrow_mut().inject_browser_key("KeyB", true);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x21));
}

#[test]
fn i8042_mouse_irq12_delivers_pic_vector() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.set_mode(PlatformInterruptMode::LegacyPic);
    }

    let i8042 = I8042Ports::new();
    i8042.connect_irqs_to_platform_interrupts(interrupts.clone());
    let ctrl = i8042.controller();

    // Enable mouse IRQs in the i8042 command byte (bit 1).
    ctrl.borrow_mut().write_port(0x64, 0x60);
    ctrl.borrow_mut().write_port(0x60, 0x47);

    // Enable mouse reporting (0xF4) via the i8042 write-to-mouse command (0xD4).
    ctrl.borrow_mut().write_port(0x64, 0xD4);
    ctrl.borrow_mut().write_port(0x60, 0xF4);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x2C));

    // Consume the ACK and clear the interrupt.
    ctrl.borrow_mut().read_port(0x60);
    {
        let mut ints = interrupts.borrow_mut();
        ints.acknowledge(0x2C);
        ints.eoi(0x2C);
    }

    // Mouse motion should now produce packets and IRQ12 edges.
    ctrl.borrow_mut().inject_mouse_motion(10, 0, 0);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x2C));
}

#[test]
fn i8042_keyboard_irq1_delivers_ioapic_vector() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        let mut entry = IoApicRedirectionEntry::fixed(0x31, 0);
        entry.masked = false;
        entry.trigger = TriggerMode::Edge;
        ints.ioapic_mut().set_entry(1, entry);
    }

    let i8042 = I8042Ports::new();
    i8042.connect_irqs_to_platform_interrupts(interrupts.clone());
    i8042.controller().borrow_mut().inject_browser_key("KeyA", true);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x31));
}

#[test]
fn i8042_mouse_irq12_delivers_ioapic_vector() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        let mut entry = IoApicRedirectionEntry::fixed(0x3C, 0);
        entry.masked = false;
        entry.trigger = TriggerMode::Edge;
        ints.ioapic_mut().set_entry(12, entry);
    }

    let i8042 = I8042Ports::new();
    i8042.connect_irqs_to_platform_interrupts(interrupts.clone());
    let ctrl = i8042.controller();

    // Enable mouse IRQs in the i8042 command byte (bit 1).
    ctrl.borrow_mut().write_port(0x64, 0x60);
    ctrl.borrow_mut().write_port(0x60, 0x47);

    // Enable mouse reporting (0xF4) via the i8042 write-to-mouse command (0xD4).
    ctrl.borrow_mut().write_port(0x64, 0xD4);
    ctrl.borrow_mut().write_port(0x60, 0xF4);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x3C));
}

