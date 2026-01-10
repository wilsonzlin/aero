use std::cell::{Cell, RefCell};
use std::rc::Rc;

use aero_devices_input::{I8042Controller, SystemControlSink};
use aero_platform::chipset::A20GateHandle;
use aero_platform::io::PortIoDevice;
use aero_platform::Platform;

struct I8042Port {
    ctrl: Rc<RefCell<I8042Controller>>,
    port: u16,
}

impl PortIoDevice for I8042Port {
    fn read(&mut self, _port: u16, _size: u8) -> u32 {
        self.ctrl.borrow_mut().read_port(self.port) as u32
    }

    fn write(&mut self, _port: u16, _size: u8, value: u32) {
        self.ctrl
            .borrow_mut()
            .write_port(self.port, value as u8);
    }
}

#[derive(Clone)]
struct PlatformSysCtrl {
    a20: A20GateHandle,
    reset_count: Rc<Cell<u32>>,
}

impl SystemControlSink for PlatformSysCtrl {
    fn set_a20(&mut self, enabled: bool) {
        self.a20.set_enabled(enabled);
    }

    fn request_reset(&mut self) {
        self.reset_count.set(self.reset_count.get() + 1);
    }
}

#[test]
fn i8042_output_port_toggles_a20_gate_in_platform_memory() {
    let mut platform = Platform::new(2 * 1024 * 1024);

    let reset_count = Rc::new(Cell::new(0u32));
    let i8042 = Rc::new(RefCell::new(I8042Controller::new()));
    i8042.borrow_mut().set_system_control_sink(Box::new(PlatformSysCtrl {
        a20: platform.chipset.a20(),
        reset_count: reset_count.clone(),
    }));

    platform
        .io
        .register(0x60, Box::new(I8042Port { ctrl: i8042.clone(), port: 0x60 }));
    platform
        .io
        .register(0x64, Box::new(I8042Port { ctrl: i8042.clone(), port: 0x64 }));

    // Before enabling A20, 0x1_00000 aliases 0x0.
    platform.memory.write_u8(0x0, 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xAA);

    // Enable A20 via i8042 output port write: set bit 1 while keeping reset
    // deasserted (bit 0 = 1).
    platform.io.write_u8(0x64, 0xD1);
    platform.io.write_u8(0x60, 0x03);

    platform.memory.write_u8(0x1_00000, 0xBB);
    assert_eq!(platform.memory.read_u8(0x0), 0xAA);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0xBB);

    assert_eq!(reset_count.get(), 0);
}

