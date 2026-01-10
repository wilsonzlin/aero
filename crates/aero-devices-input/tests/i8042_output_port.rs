use std::cell::{Cell, RefCell};
use std::rc::Rc;

use aero_devices_input::{I8042Controller, SystemControlSink};

#[derive(Clone)]
struct TestSysCtrl {
    a20_events: Rc<RefCell<Vec<bool>>>,
    reset_count: Rc<Cell<u32>>,
}

impl SystemControlSink for TestSysCtrl {
    fn set_a20(&mut self, enabled: bool) {
        self.a20_events.borrow_mut().push(enabled);
    }

    fn request_reset(&mut self) {
        self.reset_count.set(self.reset_count.get() + 1);
    }
}

#[test]
fn write_output_port_toggles_a20_and_invokes_callback() {
    let a20_events = Rc::new(RefCell::new(Vec::new()));
    let reset_count = Rc::new(Cell::new(0));

    let mut i8042 = I8042Controller::new();
    i8042.set_system_control_sink(Box::new(TestSysCtrl {
        a20_events: a20_events.clone(),
        reset_count: reset_count.clone(),
    }));

    // Enable A20 (bit 1), keep reset deasserted (bit 0 = 1).
    i8042.write_port(0x64, 0xD1);
    i8042.write_port(0x60, 0x03);

    assert_eq!(&*a20_events.borrow(), &[true]);
    assert_eq!(reset_count.get(), 0);

    // Disable A20.
    i8042.write_port(0x64, 0xD1);
    i8042.write_port(0x60, 0x01);

    assert_eq!(&*a20_events.borrow(), &[true, false]);
    assert_eq!(reset_count.get(), 0);
}

#[test]
fn reset_bit_assertion_requests_reset() {
    let a20_events = Rc::new(RefCell::new(Vec::new()));
    let reset_count = Rc::new(Cell::new(0));

    let mut i8042 = I8042Controller::new();
    i8042.set_system_control_sink(Box::new(TestSysCtrl {
        a20_events: a20_events.clone(),
        reset_count: reset_count.clone(),
    }));

    // Assert reset (bit 0 is active-low).
    i8042.write_port(0x64, 0xD1);
    i8042.write_port(0x60, 0x00);

    assert_eq!(reset_count.get(), 1);
    assert!(a20_events.borrow().is_empty());
}

#[test]
fn read_output_port_returns_last_written_value() {
    let mut i8042 = I8042Controller::new();

    i8042.write_port(0x64, 0xD1);
    i8042.write_port(0x60, 0xAB);

    i8042.write_port(0x64, 0xD0);
    assert_eq!(i8042.read_port(0x60), 0xAB);
}

