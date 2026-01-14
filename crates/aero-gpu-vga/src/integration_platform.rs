use crate::{PortIO as _, VgaDevice};
use aero_platform::io::PortIoDevice;
use std::cell::RefCell;
use std::rc::Rc;

/// [`aero_platform::io::PortIoDevice`] adapter for [`VgaDevice`].
///
/// This can be registered on an [`aero_platform::io::IoPortBus`] to expose the VGA/VBE port space
/// (for example [`crate::VGA_LEGACY_IO_START`]..=[`crate::VGA_LEGACY_IO_END`] and
/// [`crate::VBE_DISPI_IO_START`]..=[`crate::VBE_DISPI_IO_END`]).
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
