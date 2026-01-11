use crate::io::usb::core::AttachedUsbDevice;
use crate::io::usb::hub::UsbTopologyError;
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
        const LS_J_FS: u16 = 0b01 << 4;
        const LSDA: u16 = 1 << 8;
        const PR: u16 = 1 << 9;

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

    pub fn attach_at_path(
        &mut self,
        path: &[usize],
        model: Box<dyn UsbDeviceModel>,
    ) -> Result<(), UsbTopologyError> {
        let Some((&root_port, rest)) = path.split_first() else {
            return Err(UsbTopologyError::EmptyPath);
        };

        if root_port >= self.ports.len() {
            return Err(UsbTopologyError::PortOutOfRange {
                depth: 0,
                port: root_port,
                num_ports: self.ports.len(),
            });
        }

        if rest.is_empty() {
            self.attach(root_port, model);
            return Ok(());
        }

        let mut cur = self.ports[root_port]
            .device
            .as_mut()
            .ok_or(UsbTopologyError::NoDeviceAtPort {
                depth: 0,
                port: root_port,
            })?;

        for (depth, &port) in rest.iter().take(rest.len() - 1).enumerate() {
            let hub_depth = depth;
            let hub_port = if hub_depth == 0 {
                root_port
            } else {
                rest[hub_depth - 1]
            };

            let next = {
                let Some(hub) = cur.as_hub_mut() else {
                    return Err(UsbTopologyError::NotAHub {
                        depth: hub_depth,
                        port: hub_port,
                    });
                };

                if port >= hub.num_ports() {
                    return Err(UsbTopologyError::PortOutOfRange {
                        depth: depth + 1,
                        port,
                        num_ports: hub.num_ports(),
                    });
                }

                hub.downstream_device_mut(port)
                    .ok_or(UsbTopologyError::NoDeviceAtPort {
                        depth: depth + 1,
                        port,
                    })?
            };
            cur = next;
        }

        let parent_depth = path.len() - 2;
        let parent_port = if parent_depth == 0 {
            root_port
        } else {
            rest[parent_depth - 1]
        };
        let Some(hub) = cur.as_hub_mut() else {
            return Err(UsbTopologyError::NotAHub {
                depth: parent_depth,
                port: parent_port,
            });
        };

        let final_port = *rest.last().expect("rest is non-empty");
        if final_port >= hub.num_ports() {
            return Err(UsbTopologyError::PortOutOfRange {
                depth: path.len() - 1,
                port: final_port,
                num_ports: hub.num_ports(),
            });
        }

        hub.attach_downstream(final_port, model);
        Ok(())
    }

    pub fn detach_at_path(&mut self, path: &[usize]) -> Result<(), UsbTopologyError> {
        let Some((&root_port, rest)) = path.split_first() else {
            return Err(UsbTopologyError::EmptyPath);
        };

        if root_port >= self.ports.len() {
            return Err(UsbTopologyError::PortOutOfRange {
                depth: 0,
                port: root_port,
                num_ports: self.ports.len(),
            });
        }

        if rest.is_empty() {
            self.detach(root_port);
            return Ok(());
        }

        let mut cur = self.ports[root_port]
            .device
            .as_mut()
            .ok_or(UsbTopologyError::NoDeviceAtPort {
                depth: 0,
                port: root_port,
            })?;

        for (depth, &port) in rest.iter().take(rest.len() - 1).enumerate() {
            let hub_depth = depth;
            let hub_port = if hub_depth == 0 {
                root_port
            } else {
                rest[hub_depth - 1]
            };

            let next = {
                let Some(hub) = cur.as_hub_mut() else {
                    return Err(UsbTopologyError::NotAHub {
                        depth: hub_depth,
                        port: hub_port,
                    });
                };

                if port >= hub.num_ports() {
                    return Err(UsbTopologyError::PortOutOfRange {
                        depth: depth + 1,
                        port,
                        num_ports: hub.num_ports(),
                    });
                }

                hub.downstream_device_mut(port)
                    .ok_or(UsbTopologyError::NoDeviceAtPort {
                        depth: depth + 1,
                        port,
                    })?
            };
            cur = next;
        }

        let parent_depth = path.len() - 2;
        let parent_port = if parent_depth == 0 {
            root_port
        } else {
            rest[parent_depth - 1]
        };
        let Some(hub) = cur.as_hub_mut() else {
            return Err(UsbTopologyError::NotAHub {
                depth: parent_depth,
                port: parent_port,
            });
        };

        let final_port = *rest.last().expect("rest is non-empty");
        if final_port >= hub.num_ports() {
            return Err(UsbTopologyError::PortOutOfRange {
                depth: path.len() - 1,
                port: final_port,
                num_ports: hub.num_ports(),
            });
        }

        hub.detach_downstream(final_port);
        Ok(())
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
