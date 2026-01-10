use std::collections::HashMap;

pub trait PortIoDevice {
    fn read(&mut self, port: u16, size: u8) -> u32;
    fn write(&mut self, port: u16, size: u8, value: u32);

    /// Reset the device back to its power-on state.
    fn reset(&mut self) {}
}

pub struct IoPortBus {
    devices: HashMap<u16, Box<dyn PortIoDevice>>,
}

impl IoPortBus {
    pub fn new() -> Self {
        Self {
            devices: HashMap::new(),
        }
    }

    pub fn register(&mut self, port: u16, device: Box<dyn PortIoDevice>) {
        self.devices.insert(port, device);
    }

    pub fn read(&mut self, port: u16, size: u8) -> u32 {
        self.devices
            .get_mut(&port)
            .map(|d| d.read(port, size))
            .unwrap_or_else(|| match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => 0xFFFF_FFFF,
            })
    }

    pub fn write(&mut self, port: u16, size: u8, value: u32) {
        if let Some(device) = self.devices.get_mut(&port) {
            device.write(port, size, value);
        }
    }

    pub fn read_u8(&mut self, port: u16) -> u8 {
        self.read(port, 1) as u8
    }

    pub fn write_u8(&mut self, port: u16, value: u8) {
        self.write(port, 1, value as u32);
    }

    pub fn reset(&mut self) {
        for dev in self.devices.values_mut() {
            dev.reset();
        }
    }
}

impl Default for IoPortBus {
    fn default() -> Self {
        Self::new()
    }
}
