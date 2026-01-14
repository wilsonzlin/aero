use crate::{PortIO as _, VgaDevice};
use aero_platform::io::PortIoDevice;
use std::cell::RefCell;
use std::rc::Rc;

/// [`aero_platform::io::PortIoDevice`] adapter for [`VgaDevice`].
///
/// This can be registered on an [`aero_platform::io::IoPortBus`] to expose the VGA/VBE port space
/// (for example `0x3B0..0x3DF` and `0x01CE..0x01CF`).
pub struct VgaPortIoDevice {
    pub dev: Rc<RefCell<VgaDevice>>,
}

impl PortIoDevice for VgaPortIoDevice {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        self.dev.borrow_mut().port_read(port, size as usize)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        self.dev.borrow_mut().port_write(port, size as usize, value);
    }
}
