use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::i8042::{register_i8042, PlatformIrqSink, I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_devices_input::I8042Controller;
use aero_platform::interrupts::InterruptController;
use aero_platform::interrupts::PlatformInterrupts;
use aero_platform::io::IoPortBus;

#[test]
fn i8042_keyboard_irq1_delivers_pic_vector_and_set1_scancode() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts.borrow_mut().pic_mut().set_offsets(0x20, 0x28);

    let i8042 = Rc::new(RefCell::new(I8042Controller::new()));
    i8042
        .borrow_mut()
        .set_irq_sink(Box::new(PlatformIrqSink::new(interrupts.clone())));

    let mut bus = IoPortBus::new();
    register_i8042(&mut bus, i8042.clone());

    // Inject a key press. Default i8042 command byte enables translation (Set-2 -> Set-1)
    // and IRQ1, so the host should observe a PIC vector for IRQ1 and the guest should
    // read a Set-1 scancode.
    i8042.borrow_mut().inject_browser_key("KeyA", true);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x21));
    interrupts.borrow_mut().acknowledge(0x21);

    // IRQ handler reads from the i8042 data port.
    assert_eq!(bus.read_u8(I8042_DATA_PORT), 0x1E); // 'A' make in Set 1

    interrupts.borrow_mut().eoi(0x21);

    // Inject key release.
    i8042.borrow_mut().inject_browser_key("KeyA", false);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x21));
    interrupts.borrow_mut().acknowledge(0x21);
    assert_eq!(bus.read_u8(I8042_DATA_PORT), 0x9E); // break = make | 0x80
    interrupts.borrow_mut().eoi(0x21);
}

#[test]
fn i8042_mouse_irq12_delivers_pic_vector_and_mouse_status_bit() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    interrupts.borrow_mut().pic_mut().set_offsets(0x20, 0x28);

    let i8042 = Rc::new(RefCell::new(I8042Controller::new()));
    i8042
        .borrow_mut()
        .set_irq_sink(Box::new(PlatformIrqSink::new(interrupts.clone())));

    let mut bus = IoPortBus::new();
    register_i8042(&mut bus, i8042.clone());

    // Enable mouse reporting without enabling IRQ12 yet (avoid spurious interrupts
    // from the command ACK).
    bus.write_u8(I8042_STATUS_PORT, 0xD4);
    bus.write_u8(I8042_DATA_PORT, 0xF4);
    assert_eq!(bus.read_u8(I8042_DATA_PORT), 0xFA);

    // Enable IRQ12 in the command byte (bit 1).
    bus.write_u8(I8042_STATUS_PORT, 0x60);
    bus.write_u8(I8042_DATA_PORT, 0x47); // IRQ1+IRQ12 + translation

    // Inject a small motion packet (3 bytes for a standard mouse).
    i8042.borrow_mut().inject_mouse_motion(1, 0, 0);

    let mut packet = Vec::new();
    for _ in 0..3 {
        assert_eq!(interrupts.borrow().get_pending(), Some(0x2C));
        interrupts.borrow_mut().acknowledge(0x2C);

        // Status register must indicate "mouse output buffer full".
        let status = bus.read_u8(I8042_STATUS_PORT);
        assert_ne!(status & 0x20, 0);

        packet.push(bus.read_u8(I8042_DATA_PORT));
        interrupts.borrow_mut().eoi(0x2C);
    }

    assert_eq!(packet, vec![0x08, 0x01, 0x00]);
    assert_eq!(interrupts.borrow().get_pending(), None);
}
