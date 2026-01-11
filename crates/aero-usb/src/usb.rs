use core::{any::Any, fmt};

use crate::hub::UsbHub;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbSpeed {
    Full,
    Low,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbPid {
    Setup,
    In,
    Out,
}

impl UsbPid {
    pub fn from_u8(pid: u8) -> Option<Self> {
        match pid {
            0x2D => Some(Self::Setup),
            0x69 => Some(Self::In),
            0xE1 => Some(Self::Out),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbHandshake {
    /// Transaction completed successfully.
    Ack { bytes: usize },
    /// Device is temporarily unable to respond.
    Nak,
    /// Endpoint halted.
    Stall,
    /// No response (timeout).
    Timeout,
}

impl UsbHandshake {
    pub fn is_complete(self) -> bool {
        matches!(
            self,
            UsbHandshake::Ack { .. } | UsbHandshake::Stall | UsbHandshake::Timeout
        )
    }
}

#[derive(Clone, Copy)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl fmt::Debug for SetupPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetupPacket")
            .field("request_type", &format_args!("{:#04x}", self.request_type))
            .field("request", &format_args!("{:#04x}", self.request))
            .field("value", &format_args!("{:#06x}", self.value))
            .field("index", &format_args!("{:#06x}", self.index))
            .field("length", &self.length)
            .finish()
    }
}

impl SetupPacket {
    pub fn parse(bytes: [u8; 8]) -> Self {
        Self {
            request_type: bytes[0],
            request: bytes[1],
            value: u16::from_le_bytes([bytes[2], bytes[3]]),
            index: u16::from_le_bytes([bytes[4], bytes[5]]),
            length: u16::from_le_bytes([bytes[6], bytes[7]]),
        }
    }
}

pub trait UsbDevice {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn as_hub(&self) -> Option<&dyn UsbHub> {
        None
    }

    fn as_hub_mut(&mut self) -> Option<&mut dyn UsbHub> {
        None
    }

    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

    /// Advances device state by 1ms.
    ///
    /// This is primarily used for hub port reset timers and for propagating time through nested
    /// hub topologies. Most device models do not require timers and can rely on the default no-op
    /// implementation.
    fn tick_1ms(&mut self) {}

    fn reset(&mut self);

    fn address(&self) -> u8;

    fn handle_setup(&mut self, setup: SetupPacket);

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake;

    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> UsbHandshake;
}

pub struct UsbPort {
    pub connected: bool,
    pub enabled: bool,
    pub device: Option<Box<dyn UsbDevice>>,
}

impl UsbPort {
    fn empty() -> Self {
        Self {
            connected: false,
            enabled: false,
            device: None,
        }
    }
}

pub struct UsbBus {
    ports: Vec<UsbPort>,
}

impl UsbBus {
    pub fn new(num_ports: usize) -> Self {
        Self {
            ports: (0..num_ports).map(|_| UsbPort::empty()).collect(),
        }
    }

    pub fn port(&self, idx: usize) -> Option<&UsbPort> {
        self.ports.get(idx)
    }

    pub fn port_mut(&mut self, idx: usize) -> Option<&mut UsbPort> {
        self.ports.get_mut(idx)
    }

    pub fn connect(&mut self, idx: usize, mut device: Box<dyn UsbDevice>) {
        device.reset();
        let port = self
            .ports
            .get_mut(idx)
            .unwrap_or_else(|| panic!("invalid port index {idx}"));
        port.connected = true;
        port.enabled = false;
        port.device = Some(device);
    }

    pub fn disconnect(&mut self, idx: usize) {
        let port = self
            .ports
            .get_mut(idx)
            .unwrap_or_else(|| panic!("invalid port index {idx}"));
        port.connected = false;
        port.enabled = false;
        port.device = None;
    }

    pub fn reset_port(&mut self, idx: usize) {
        if let Some(port) = self.ports.get_mut(idx) {
            // USB port reset temporarily disables the port; it is re-enabled once the hub/host
            // controller completes the reset sequence.
            port.enabled = false;
            if let Some(dev) = port.device.as_mut() {
                dev.reset();
            }
        }
    }

    pub fn tick_1ms(&mut self) {
        for port in &mut self.ports {
            if !port.connected || !port.enabled {
                continue;
            }
            let Some(dev) = port.device.as_mut() else {
                continue;
            };
            dev.tick_1ms();
        }
    }

    /// Attaches `device` at a topology path (root port → hub port → ...).
    ///
    /// The first element of `path` addresses a root port and is zero-based (like
    /// [`UsbBus::connect`]). Subsequent elements address hub ports and are **1-based** (matching
    /// USB hub port numbering used by hub class requests).
    pub fn attach_at_path(&mut self, path: &[usize], device: Box<dyn UsbDevice>) {
        let Some((&root, rest)) = path.split_first() else {
            panic!("USB topology path must not be empty");
        };

        if rest.is_empty() {
            self.connect(root, device);
            return;
        }

        let port = self
            .ports
            .get_mut(root)
            .unwrap_or_else(|| panic!("invalid port index {root}"));
        let Some(root_dev) = port.device.as_mut() else {
            panic!("no device attached at root port {root}");
        };

        let mut current: &mut dyn UsbDevice = root_dev.as_mut();
        for (depth, &hub_port) in rest[..rest.len() - 1].iter().enumerate() {
            let hub = current
                .as_hub_mut()
                .unwrap_or_else(|| panic!("device at depth {depth} is not a USB hub"));
            let hub_idx = hub_port
                .checked_sub(1)
                .unwrap_or_else(|| panic!("hub port numbers are 1-based (got 0 at depth {depth})"));
            let num_ports = hub.num_ports();
            if hub_idx >= num_ports {
                panic!("invalid hub port {hub_port} at depth {depth} (hub has {num_ports} ports)");
            }
            current = hub.downstream_device_mut(hub_idx).unwrap_or_else(|| {
                panic!("no device attached at hub port {hub_port} (depth {depth})")
            });
        }

        let hub = current
            .as_hub_mut()
            .unwrap_or_else(|| panic!("device at depth {} is not a USB hub", rest.len() - 1));
        let last_port = rest[rest.len() - 1];
        let hub_idx = last_port.checked_sub(1).unwrap_or_else(|| {
            panic!(
                "hub port numbers are 1-based (got 0 at depth {})",
                rest.len() - 1
            )
        });
        let num_ports = hub.num_ports();
        if hub_idx >= num_ports {
            panic!(
                "invalid hub port {last_port} at depth {} (hub has {num_ports} ports)",
                rest.len() - 1
            );
        }
        hub.attach_downstream(hub_idx, device);
    }

    /// Detaches any device attached at the given topology `path`.
    ///
    /// See [`UsbBus::attach_at_path`] for path numbering conventions.
    pub fn detach_at_path(&mut self, path: &[usize]) {
        let Some((&root, rest)) = path.split_first() else {
            panic!("USB topology path must not be empty");
        };

        if rest.is_empty() {
            self.disconnect(root);
            return;
        }

        let port = self
            .ports
            .get_mut(root)
            .unwrap_or_else(|| panic!("invalid port index {root}"));
        let Some(root_dev) = port.device.as_mut() else {
            panic!("no device attached at root port {root}");
        };

        let mut current: &mut dyn UsbDevice = root_dev.as_mut();
        for (depth, &hub_port) in rest[..rest.len() - 1].iter().enumerate() {
            let hub = current
                .as_hub_mut()
                .unwrap_or_else(|| panic!("device at depth {depth} is not a USB hub"));
            let hub_idx = hub_port
                .checked_sub(1)
                .unwrap_or_else(|| panic!("hub port numbers are 1-based (got 0 at depth {depth})"));
            let num_ports = hub.num_ports();
            if hub_idx >= num_ports {
                panic!("invalid hub port {hub_port} at depth {depth} (hub has {num_ports} ports)");
            }
            current = hub.downstream_device_mut(hub_idx).unwrap_or_else(|| {
                panic!("no device attached at hub port {hub_port} (depth {depth})")
            });
        }

        let hub = current
            .as_hub_mut()
            .unwrap_or_else(|| panic!("device at depth {} is not a USB hub", rest.len() - 1));
        let last_port = rest[rest.len() - 1];
        let hub_idx = last_port.checked_sub(1).unwrap_or_else(|| {
            panic!(
                "hub port numbers are 1-based (got 0 at depth {})",
                rest.len() - 1
            )
        });
        let num_ports = hub.num_ports();
        if hub_idx >= num_ports {
            panic!(
                "invalid hub port {last_port} at depth {} (hub has {num_ports} ports)",
                rest.len() - 1
            );
        }
        hub.detach_downstream(hub_idx);
    }

    pub fn device_mut_for_address(&mut self, addr: u8) -> Option<&mut dyn UsbDevice> {
        self.find_device_mut(addr)
    }

    fn find_device_mut(&mut self, addr: u8) -> Option<&mut dyn UsbDevice> {
        for port in &mut self.ports {
            if !port.connected || !port.enabled {
                continue;
            }
            let Some(dev) = port.device.as_mut() else {
                continue;
            };
            if dev.address() == addr {
                return Some(dev.as_mut());
            }
            if let Some(hub) = dev.as_hub_mut() {
                if let Some(found) = hub.downstream_device_mut_for_address(addr) {
                    return Some(found);
                }
            }
        }
        None
    }

    pub fn handle_setup(&mut self, addr: u8, setup: SetupPacket) -> UsbHandshake {
        let Some(dev) = self.find_device_mut(addr) else {
            return UsbHandshake::Timeout;
        };
        dev.handle_setup(setup);
        UsbHandshake::Ack { bytes: 8 }
    }

    pub fn handle_out(&mut self, addr: u8, ep: u8, data: &[u8]) -> UsbHandshake {
        let Some(dev) = self.find_device_mut(addr) else {
            return UsbHandshake::Timeout;
        };
        dev.handle_out(ep, data)
    }

    pub fn handle_in(&mut self, addr: u8, ep: u8, buf: &mut [u8]) -> UsbHandshake {
        let Some(dev) = self.find_device_mut(addr) else {
            return UsbHandshake::Timeout;
        };
        dev.handle_in(ep, buf)
    }
}
