use core::{any::Any, fmt};

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

    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

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
