use crate::io::usb::core::AttachedUsbDevice;
use crate::io::usb::UsbDeviceModel;

use super::UsbTopologyError;

struct Port {
    device: Option<AttachedUsbDevice>,
    connected: bool,
    connect_change: bool,
    enabled: bool,
    enable_change: bool,
    reset: bool,
    reset_countdown_ms: u8,
    suspended: bool,
    resuming: bool,
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
            suspended: false,
            resuming: false,
        }
    }

    fn read_portsc(&self) -> u16 {
        const CCS: u16 = 1 << 0;
        const CSC: u16 = 1 << 1;
        const PED: u16 = 1 << 2;
        const PEDC: u16 = 1 << 3;
        const LS_J_FS: u16 = 0b01 << 4;
        const LSDA: u16 = 1 << 8;
        const PR: u16 = 1 << 9;
        const SUSP: u16 = 1 << 12;
        const RESUME: u16 = 1 << 13;

        let mut v = 0u16;
        if self.connected {
            v |= CCS;
            if !self.reset {
                v |= LS_J_FS;
            }
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
        if self.suspended {
            v |= SUSP;
        }
        if self.resuming {
            v |= RESUME;
        }
        v
    }

    fn write_portsc(&mut self, value: u16) {
        const CSC: u16 = 1 << 1;
        const PED: u16 = 1 << 2;
        const PEDC: u16 = 1 << 3;
        const PR: u16 = 1 << 9;
        const SUSP: u16 = 1 << 12;
        const RESUME: u16 = 1 << 13;

        // Write-1-to-clear status change bits.
        if value & CSC != 0 {
            self.connect_change = false;
        }
        if value & PEDC != 0 {
            self.enable_change = false;
        }

        // Port reset: model a 50ms reset and reset attached device state.
        if value & PR != 0 && !self.reset {
            self.reset = true;
            self.reset_countdown_ms = 50;
            self.suspended = false;
            self.resuming = false;
            if let Some(dev) = self.device.as_mut() {
                dev.reset();
            }
            if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }
        }

        // Port enable (read/write).
        //
        // While in reset, the port enable bit reads as 0 and writes are ignored until
        // the reset sequence completes.
        if !self.reset {
            let want_enabled = value & PED != 0;
            // Hardware only allows enabling a port when a device is actually present.
            if want_enabled {
                if self.connected && !self.enabled {
                    self.enabled = true;
                    self.enable_change = true;
                }
            } else if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }

            self.suspended = value & SUSP != 0;
            self.resuming = value & RESUME != 0;
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

    pub fn bus_reset(&mut self) {
        for p in &mut self.ports {
            p.suspended = false;
            p.resuming = false;
            if let Some(dev) = p.device.as_mut() {
                dev.reset();
            }
        }
    }

    pub fn attach(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        let p = &mut self.ports[port];
        p.device = Some(AttachedUsbDevice::new(model));
        p.suspended = false;
        p.resuming = false;
        if !p.connected {
            p.connected = true;
        }
        p.connect_change = true;
        // Connecting a new device effectively disables the port until the host performs
        // the reset/enable sequence.
        if p.enabled {
            p.enabled = false;
            p.enable_change = true;
        }
    }

    pub fn detach(&mut self, port: usize) {
        let p = &mut self.ports[port];
        p.device = None;
        p.suspended = false;
        p.resuming = false;
        if p.connected {
            p.connected = false;
            p.connect_change = true;
        }
        if p.enabled {
            p.enabled = false;
            p.enable_change = true;
        }
    }

    /// Attach a new USB device model at `path`.
    ///
    /// `path` is a list of port numbers describing a walk from the UHCI root hub to a downstream
    /// hub port:
    ///
    /// - `path[0]` is the *root hub* port index (0-based).
    /// - `path[1..]` are *downstream hub* port numbers (1-based, as in USB descriptors).
    ///
    /// The final element selects the port to attach to; all preceding hops must already have a
    /// device attached and must be USB hubs.
    pub fn attach_at_path(
        &mut self,
        path: &[u8],
        model: Box<dyn UsbDeviceModel>,
    ) -> Result<(), UsbTopologyError> {
        let Some((&root_port, rest)) = path.split_first() else {
            return Err(UsbTopologyError::EmptyPath);
        };
        let Some(p) = self.ports.get_mut(root_port as usize) else {
            return Err(UsbTopologyError::PortOutOfRange {
                depth: 0,
                port: root_port as usize,
                num_ports: self.ports.len(),
            });
        };

        // If only a root port is provided, attach directly to the root hub.
        if rest.is_empty() {
            if p.device.is_some() {
                return Err(UsbTopologyError::PortOccupied {
                    depth: 0,
                    port: root_port as usize,
                });
            }
            p.device = Some(AttachedUsbDevice::new(model));
            p.suspended = false;
            p.resuming = false;
            if !p.connected {
                p.connected = true;
            }
            p.connect_change = true;
            // Connecting a new device effectively disables the port until the host performs
            // the reset/enable sequence.
            if p.enabled {
                p.enabled = false;
                p.enable_change = true;
            }
            return Ok(());
        }

        let Some(root_dev) = p.device.as_mut() else {
            return Err(UsbTopologyError::NoDeviceAtPort {
                depth: 0,
                port: root_port as usize,
            });
        };

        let (&leaf_port, hub_path) = rest.split_last().expect("rest is non-empty");
        let mut hub_dev = root_dev;
        for (depth, &hop) in hub_path.iter().enumerate() {
            let depth = depth + 1;
            hub_dev = hub_dev
                .model_mut()
                .hub_port_device_mut(hop)
                .map_err(|e| e.with_depth(depth))?;
        }
        let leaf_depth = path.len() - 1;
        hub_dev
            .model_mut()
            .hub_attach_device(leaf_port, model)
            .map_err(|e| e.with_depth(leaf_depth))
    }

    /// Detach any USB device model at `path`.
    ///
    /// Path semantics match [`RootHub::attach_at_path`].
    pub fn detach_at_path(&mut self, path: &[u8]) -> Result<(), UsbTopologyError> {
        let Some((&root_port, rest)) = path.split_first() else {
            return Err(UsbTopologyError::EmptyPath);
        };
        let Some(p) = self.ports.get_mut(root_port as usize) else {
            return Err(UsbTopologyError::PortOutOfRange {
                depth: 0,
                port: root_port as usize,
                num_ports: self.ports.len(),
            });
        };

        // If only a root port is provided, detach directly from the root hub.
        if rest.is_empty() {
            if p.device.is_none() {
                return Err(UsbTopologyError::NoDeviceAtPort {
                    depth: 0,
                    port: root_port as usize,
                });
            }
            p.device = None;
            p.suspended = false;
            p.resuming = false;
            if p.connected {
                p.connected = false;
                p.connect_change = true;
            }
            if p.enabled {
                p.enabled = false;
                p.enable_change = true;
            }
            return Ok(());
        }

        let Some(root_dev) = p.device.as_mut() else {
            return Err(UsbTopologyError::NoDeviceAtPort {
                depth: 0,
                port: root_port as usize,
            });
        };

        let (&leaf_port, hub_path) = rest.split_last().expect("rest is non-empty");
        let mut hub_dev = root_dev;
        for (depth, &hop) in hub_path.iter().enumerate() {
            let depth = depth + 1;
            hub_dev = hub_dev
                .model_mut()
                .hub_port_device_mut(hop)
                .map_err(|e| e.with_depth(depth))?;
        }
        let leaf_depth = path.len() - 1;
        hub_dev
            .model_mut()
            .hub_detach_device(leaf_port)
            .map_err(|e| e.with_depth(leaf_depth))
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
            if !p.enabled {
                continue;
            }
            if let Some(dev) = p.device.as_mut() {
                dev.tick_1ms();
            }
        }
    }

    pub fn force_enable_for_tests(&mut self, port: usize) {
        let p = &mut self.ports[port];
        p.enabled = true;
        p.enable_change = true;
    }

    pub fn device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
        for p in &mut self.ports {
            if !p.enabled {
                continue;
            }
            if let Some(dev) = p.device.as_mut() {
                if let Some(found) = dev.device_mut_for_address(address) {
                    return Some(found);
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
