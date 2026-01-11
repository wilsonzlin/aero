pub mod vbe;
pub mod vga;

use self::{vbe::VbeDevice, vga::VgaDevice};

#[derive(Debug, Clone, Default)]
pub struct VideoDevice {
    pub vga: VgaDevice,
    pub vbe: VbeDevice,
}

impl VideoDevice {
    pub fn new() -> Self {
        Self {
            vga: VgaDevice::new(),
            vbe: VbeDevice::new(),
        }
    }
}
