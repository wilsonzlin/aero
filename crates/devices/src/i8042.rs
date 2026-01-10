use std::cell::RefCell;
use std::rc::Rc;

use aero_devices_input::I8042Controller;
use aero_platform::io::PortIoDevice;

/// PS/2 i8042 controller exposed as two port-mapped [`PortIoDevice`] handles.
///
/// The controller uses two I/O ports:
/// - `0x60`: data port
/// - `0x64`: status/command port
///
/// [`aero_platform::io::IoPortBus`] currently routes by exact port number, so we
/// expose one device instance per port that shares the same underlying
/// controller.
#[derive(Clone)]
pub struct I8042Ports {
    inner: Rc<RefCell<I8042Controller>>,
}

impl I8042Ports {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(I8042Controller::new())),
        }
    }

    /// Returns a cloneable handle to the shared controller for host-side input injection.
    pub fn controller(&self) -> Rc<RefCell<I8042Controller>> {
        self.inner.clone()
    }

    pub fn port60(&self) -> I8042Port {
        I8042Port {
            inner: self.inner.clone(),
        }
    }

    pub fn port64(&self) -> I8042Port {
        I8042Port {
            inner: self.inner.clone(),
        }
    }
}

impl Default for I8042Ports {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct I8042Port {
    inner: Rc<RefCell<I8042Controller>>,
}

impl PortIoDevice for I8042Port {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let byte = self.inner.borrow_mut().read_port(port);
        match size {
            1 => byte as u32,
            2 => u16::from_le_bytes([byte, byte]) as u32,
            4 => u32::from_le_bytes([byte, byte, byte, byte]),
            _ => byte as u32,
        }
    }

    fn write(&mut self, port: u16, _size: u8, value: u32) {
        self.inner
            .borrow_mut()
            .write_port(port, (value & 0xFF) as u8);
    }
}
