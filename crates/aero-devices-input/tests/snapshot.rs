use std::cell::{Cell, RefCell};
use std::rc::Rc;

use aero_devices_input::I8042Controller;
use aero_devices_input::SystemControlSink;
use aero_io_snapshot::io::state::IoSnapshot;

#[derive(Clone)]
struct TestSysCtrl {
    a20: Rc<Cell<bool>>,
    events: Rc<RefCell<Vec<bool>>>,
}

impl SystemControlSink for TestSysCtrl {
    fn set_a20(&mut self, enabled: bool) {
        self.a20.set(enabled);
        self.events.borrow_mut().push(enabled);
    }

    fn request_reset(&mut self) {}

    fn a20_enabled(&self) -> Option<bool> {
        Some(self.a20.get())
    }
}

#[test]
fn i8042_snapshot_roundtrip_preserves_pending_bytes() {
    let mut dev = I8042Controller::new();
    dev.inject_browser_key("KeyA", true);
    dev.inject_browser_key("KeyA", false);

    let snap = dev.save_state();

    let mut restored = I8042Controller::new();
    restored.load_state(&snap).unwrap();

    assert_eq!(restored.read_port(0x60), 0x1e);
    assert_eq!(restored.read_port(0x60), 0x9e);
    assert_eq!(restored.read_port(0x60), 0x00);
}

#[test]
fn i8042_snapshot_roundtrip_preserves_output_port_and_pending_write() {
    let mut dev = I8042Controller::new();

    // Set an initial output-port value.
    dev.write_port(0x64, 0xD1);
    dev.write_port(0x60, 0x03);

    // Leave an in-flight "write output port" pending write.
    dev.write_port(0x64, 0xD1);

    let snap = dev.save_state();

    let mut restored = I8042Controller::new();
    restored.load_state(&snap).unwrap();

    // Verify output port preserved.
    restored.write_port(0x64, 0xD0);
    assert_eq!(restored.read_port(0x60), 0x03);

    // Verify pending write preserved and targets the output port.
    restored.write_port(0x60, 0x01);
    restored.write_port(0x64, 0xD0);
    assert_eq!(restored.read_port(0x60), 0x01);
}

#[test]
fn i8042_snapshot_restore_resynchronizes_a20_line_when_sys_ctrl_attached() {
    let a20 = Rc::new(Cell::new(true));
    let events = Rc::new(RefCell::new(Vec::new()));

    let mut dev = I8042Controller::new();
    dev.set_system_control_sink(Box::new(TestSysCtrl {
        a20: a20.clone(),
        events: events.clone(),
    }));

    // Save a snapshot with A20 disabled in the controller output port.
    dev.write_port(0x64, 0xD1);
    dev.write_port(0x60, 0x01);
    let snap = dev.save_state();

    // Simulate restoring into an environment where the platform A20 line is currently enabled.
    a20.set(true);
    events.borrow_mut().clear();

    let mut restored = I8042Controller::new();
    restored.set_system_control_sink(Box::new(TestSysCtrl {
        a20: a20.clone(),
        events: events.clone(),
    }));
    restored.load_state(&snap).unwrap();

    assert!(!a20.get());
    assert_eq!(&*events.borrow(), &[false]);
}
