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

#[derive(Clone)]
struct QuerySysCtrl {
    a20: Rc<Cell<bool>>,
    a20_events: Rc<RefCell<Vec<bool>>>,
    reset_count: Rc<Cell<u32>>,
}

impl SystemControlSink for QuerySysCtrl {
    fn set_a20(&mut self, enabled: bool) {
        self.a20.set(enabled);
        self.a20_events.borrow_mut().push(enabled);
    }

    fn request_reset(&mut self) {
        self.reset_count.set(self.reset_count.get() + 1);
    }

    fn a20_enabled(&self) -> Option<bool> {
        Some(self.a20.get())
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

#[test]
fn output_port_a20_bit_tracks_sink_query_and_write_resynchronizes() {
    let a20 = Rc::new(Cell::new(false));
    let a20_events = Rc::new(RefCell::new(Vec::new()));
    let reset_count = Rc::new(Cell::new(0));

    let mut i8042 = I8042Controller::new();
    i8042.set_system_control_sink(Box::new(QuerySysCtrl {
        a20: a20.clone(),
        a20_events: a20_events.clone(),
        reset_count: reset_count.clone(),
    }));

    // Enable A20 via i8042 output-port write.
    i8042.write_port(0x64, 0xD1);
    i8042.write_port(0x60, 0x03);
    assert!(a20.get());
    assert_eq!(&*a20_events.borrow(), &[true]);

    // External path (e.g. port 0x92) disables A20 without touching the i8042 latch.
    a20.set(false);

    // i8042 output-port reads should reflect the current line state.
    i8042.write_port(0x64, 0xD0);
    assert_eq!(i8042.read_port(0x60), 0x01);

    // Re-writing the output port should re-enable A20 even if the controller's
    // internal latch already has the bit set.
    i8042.write_port(0x64, 0xD1);
    i8042.write_port(0x60, 0x03);
    assert!(a20.get());
    assert_eq!(&*a20_events.borrow(), &[true, true]);

    assert_eq!(reset_count.get(), 0);
}
