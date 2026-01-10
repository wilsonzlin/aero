use std::cell::RefCell;
use std::rc::Rc;

use aero_devices_input::{I8042Controller, IrqSink};

#[derive(Clone)]
struct TestIrqSink {
    irqs: Rc<RefCell<Vec<u8>>>,
}

impl IrqSink for TestIrqSink {
    fn raise_irq(&mut self, irq: u8) {
        self.irqs.borrow_mut().push(irq);
    }
}

#[test]
fn i8042_command_d2_writes_keyboard_output_buffer_and_raises_irq1() {
    let irqs = Rc::new(RefCell::new(Vec::new()));
    let mut i8042 = I8042Controller::new();
    i8042.set_irq_sink(Box::new(TestIrqSink { irqs: irqs.clone() }));

    // 0xD2: next data byte should appear as keyboard output (OBF set, AUX clear).
    i8042.write_port(0x64, 0xD2);
    i8042.write_port(0x60, 0xAA);

    assert_eq!(&*irqs.borrow(), &[1]);

    let status = i8042.read_port(0x64);
    assert_ne!(status & 0x01, 0, "output buffer should be full");
    assert_eq!(status & 0x20, 0, "AUX bit should be clear for keyboard data");

    assert_eq!(i8042.read_port(0x60), 0xAA);
    assert_eq!(i8042.read_port(0x64) & 0x01, 0, "output buffer should be empty after read");
}

#[test]
fn i8042_command_d3_writes_mouse_output_buffer_and_can_raise_irq12() {
    let irqs = Rc::new(RefCell::new(Vec::new()));
    let mut i8042 = I8042Controller::new();
    i8042.set_irq_sink(Box::new(TestIrqSink { irqs: irqs.clone() }));

    // Default command byte enables IRQ1 but not IRQ12. Verify no IRQ is raised yet.
    i8042.write_port(0x64, 0xD3);
    i8042.write_port(0x60, 0xBB);
    assert!(irqs.borrow().is_empty(), "IRQ12 should be gated by the command byte");

    let status = i8042.read_port(0x64);
    assert_ne!(status & 0x01, 0, "output buffer should be full");
    assert_ne!(status & 0x20, 0, "AUX bit should be set for mouse data");
    assert_eq!(i8042.read_port(0x60), 0xBB);

    // Enable IRQ12 (bit 1) and try again.
    i8042.write_port(0x64, 0x60);
    i8042.write_port(0x60, 0x47);

    i8042.write_port(0x64, 0xD3);
    i8042.write_port(0x60, 0xCC);

    assert_eq!(&*irqs.borrow(), &[12]);
    assert_ne!(i8042.read_port(0x64) & 0x20, 0);
    assert_eq!(i8042.read_port(0x60), 0xCC);
}

