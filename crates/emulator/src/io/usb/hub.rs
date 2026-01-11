use crate::io::usb::core::AttachedUsbDevice;
use crate::io::usb::UsbDeviceModel;

struct Port {
    device: Option<AttachedUsbDevice>,
    connected: bool,
    connect_change: bool,
    enabled: bool,
    enable_change: bool,
    reset: bool,
    reset_countdown_ms: u8,
}

impl Port {
    fn new() -> Self {
        Self {
            device: None,
            connected: false,
            connect_change: false,
            enabled: false,
            enable_change: false,
            reset: false,
            reset_countdown_ms: 0,
        }
    }

    fn read_portsc(&self) -> u16 {
        const CCS: u16 = 1 << 0;
        const CSC: u16 = 1 << 1;
        const PED: u16 = 1 << 2;
        const PEDC: u16 = 1 << 3;
        const LSDA: u16 = 1 << 8;
        const PR: u16 = 1 << 9;

        let mut v = 0u16;
        if self.connected {
            v |= CCS;
        }
        if self.connect_change {
            v |= CSC;
        }
        if self.enabled {
            v |= PED;
        }
        if self.enable_change {
            v |= PEDC;
        }
        // Low-speed not modelled yet; current HID models are full-speed.
        let _ = LSDA;
        if self.reset {
            v |= PR;
        }
        v
    }

    fn write_portsc(&mut self, value: u16) {
        const CSC: u16 = 1 << 1;
        const PED: u16 = 1 << 2;
        const PEDC: u16 = 1 << 3;
        const PR: u16 = 1 << 9;

        // Write-1-to-clear status change bits.
        if value & CSC != 0 {
            self.connect_change = false;
        }
        if value & PEDC != 0 {
            self.enable_change = false;
        }

        // Port enable (read/write).
        let new_enabled = value & PED != 0;
        if new_enabled != self.enabled {
            self.enabled = new_enabled;
            self.enable_change = true;
        }

        // Port reset: model a 50ms reset and reset attached device state.
        if value & PR != 0 && !self.reset {
            self.reset = true;
            self.reset_countdown_ms = 50;
            if let Some(dev) = self.device.as_mut() {
                dev.reset();
            }
            if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }
        }
    }

    fn tick_1ms(&mut self) {
        if self.reset {
            self.reset_countdown_ms = self.reset_countdown_ms.saturating_sub(1);
            if self.reset_countdown_ms == 0 {
                self.reset = false;
                if self.connected && !self.enabled {
                    self.enabled = true;
                    self.enable_change = true;
                }
            }
        }
    }
}

/// UHCI "root hub" exposed via PORTSC registers.
pub struct RootHub {
    ports: [Port; 2],
}

impl RootHub {
    pub fn new() -> Self {
        Self {
            ports: [Port::new(), Port::new()],
        }
    }

    pub fn attach(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        let p = &mut self.ports[port];
        p.device = Some(AttachedUsbDevice::new(model));
        p.connected = true;
        p.connect_change = true;
    }

    pub fn detach(&mut self, port: usize) {
        let p = &mut self.ports[port];
        p.device = None;
        if p.connected {
            p.connected = false;
            p.connect_change = true;
        }
        if p.enabled {
            p.enabled = false;
            p.enable_change = true;
        }
    }

    pub fn read_portsc(&self, port: usize) -> u16 {
        self.ports[port].read_portsc()
    }

    pub fn write_portsc(&mut self, port: usize, value: u16) {
        self.ports[port].write_portsc(value);
    }

    pub fn tick_1ms(&mut self) {
        for p in &mut self.ports {
            p.tick_1ms();
        }
    }

    pub fn force_enable_for_tests(&mut self, port: usize) {
        let p = &mut self.ports[port];
        p.enabled = true;
        p.enable_change = true;
    }

    pub fn device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
        if address == 0 {
            for p in &mut self.ports {
                if !p.enabled {
                    continue;
                }
                if let Some(dev) = p.device.as_mut() {
                    if dev.address() == 0 {
                        return Some(dev);
                    }
                }
            }
            return None;
        }

        for p in &mut self.ports {
            if !p.enabled {
                continue;
            }
            if let Some(dev) = p.device.as_mut() {
                if dev.address() == address {
                    return Some(dev);
                }
            }
        }
        None
    }
}

impl Default for RootHub {
    fn default() -> Self {
        Self::new()
    }
}
