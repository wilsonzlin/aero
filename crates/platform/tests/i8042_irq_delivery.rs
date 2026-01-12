use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::i8042::{I8042Ports, I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

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
    ctrl.borrow_mut().read_port(I8042_DATA_PORT);
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
    ctrl.borrow_mut().write_port(I8042_STATUS_PORT, 0x60);
    ctrl.borrow_mut().write_port(I8042_DATA_PORT, 0x47);

    // Enable mouse reporting (0xF4) via the i8042 write-to-mouse command (0xD4).
    ctrl.borrow_mut().write_port(I8042_STATUS_PORT, 0xD4);
    ctrl.borrow_mut().write_port(I8042_DATA_PORT, 0xF4);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x2C));

    // Consume the ACK and clear the interrupt.
    ctrl.borrow_mut().read_port(I8042_DATA_PORT);
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
        program_ioapic_entry(&mut ints, 1, 0x31, 0);
    }

    let i8042 = I8042Ports::new();
    i8042.connect_irqs_to_platform_interrupts(interrupts.clone());
    i8042
        .controller()
        .borrow_mut()
        .inject_browser_key("KeyA", true);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x31));
}

#[test]
fn i8042_mouse_irq12_delivers_ioapic_vector() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_entry(&mut ints, 12, 0x3C, 0);
    }

    let i8042 = I8042Ports::new();
    i8042.connect_irqs_to_platform_interrupts(interrupts.clone());
    let ctrl = i8042.controller();

    // Enable mouse IRQs in the i8042 command byte (bit 1).
    ctrl.borrow_mut().write_port(I8042_STATUS_PORT, 0x60);
    ctrl.borrow_mut().write_port(I8042_DATA_PORT, 0x47);

    // Enable mouse reporting (0xF4) via the i8042 write-to-mouse command (0xD4).
    ctrl.borrow_mut().write_port(I8042_STATUS_PORT, 0xD4);
    ctrl.borrow_mut().write_port(I8042_DATA_PORT, 0xF4);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x3C));
}
