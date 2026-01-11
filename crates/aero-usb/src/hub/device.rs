use core::any::Any;

use crate::hub::UsbHub;
use crate::usb::{SetupPacket, UsbDevice, UsbHandshake};

extern crate alloc;

use alloc::vec::Vec;

const USB_DESCRIPTOR_TYPE_DEVICE: u8 = 0x01;
const USB_DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;
const USB_DESCRIPTOR_TYPE_STRING: u8 = 0x03;
const USB_DESCRIPTOR_TYPE_INTERFACE: u8 = 0x04;
const USB_DESCRIPTOR_TYPE_ENDPOINT: u8 = 0x05;
const USB_DESCRIPTOR_TYPE_HUB: u8 = 0x29;

const USB_REQUEST_GET_STATUS: u8 = 0x00;
const USB_REQUEST_CLEAR_FEATURE: u8 = 0x01;
const USB_REQUEST_SET_FEATURE: u8 = 0x03;
const USB_REQUEST_SET_ADDRESS: u8 = 0x05;
const USB_REQUEST_GET_DESCRIPTOR: u8 = 0x06;
const USB_REQUEST_GET_CONFIGURATION: u8 = 0x08;
const USB_REQUEST_SET_CONFIGURATION: u8 = 0x09;
const USB_REQUEST_GET_INTERFACE: u8 = 0x0a;
const USB_REQUEST_SET_INTERFACE: u8 = 0x0b;

const USB_FEATURE_ENDPOINT_HALT: u16 = 0;
const USB_FEATURE_DEVICE_REMOTE_WAKEUP: u16 = 1;

const HUB_FEATURE_C_HUB_LOCAL_POWER: u16 = 0;
const HUB_FEATURE_C_HUB_OVER_CURRENT: u16 = 1;

const HUB_PORT_FEATURE_ENABLE: u16 = 1;
const HUB_PORT_FEATURE_SUSPEND: u16 = 2;
const HUB_PORT_FEATURE_RESET: u16 = 4;
const HUB_PORT_FEATURE_POWER: u16 = 8;
const HUB_PORT_FEATURE_C_PORT_CONNECTION: u16 = 16;
const HUB_PORT_FEATURE_C_PORT_ENABLE: u16 = 17;
const HUB_PORT_FEATURE_C_PORT_SUSPEND: u16 = 18;
const HUB_PORT_FEATURE_C_PORT_OVER_CURRENT: u16 = 19;
const HUB_PORT_FEATURE_C_PORT_RESET: u16 = 20;

const HUB_PORT_STATUS_CONNECTION: u16 = 1 << 0;
const HUB_PORT_STATUS_ENABLE: u16 = 1 << 1;
const HUB_PORT_STATUS_SUSPEND: u16 = 1 << 2;
const HUB_PORT_STATUS_RESET: u16 = 1 << 4;
const HUB_PORT_STATUS_POWER: u16 = 1 << 8;

const HUB_PORT_CHANGE_CONNECTION: u16 = 1 << 0;
const HUB_PORT_CHANGE_ENABLE: u16 = 1 << 1;
const HUB_PORT_CHANGE_SUSPEND: u16 = 1 << 2;
const HUB_PORT_CHANGE_RESET: u16 = 1 << 4;

const HUB_INTERRUPT_IN_EP_ADDR: u8 = 0x81;
const HUB_INTERRUPT_IN_EP_NUM: u8 = 1;

const DEFAULT_HUB_NUM_PORTS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ep0Stage {
    Idle,
    DataIn,
    DataOut,
    StatusIn,
    StatusOut,
}

#[derive(Debug)]
struct Ep0Control {
    stage: Ep0Stage,
    setup: Option<SetupPacket>,
    in_data: Vec<u8>,
    in_offset: usize,
    out_expected: usize,
    out_data: Vec<u8>,
    stalled: bool,
}

impl Ep0Control {
    fn new() -> Self {
        Self {
            stage: Ep0Stage::Idle,
            setup: None,
            in_data: Vec::new(),
            in_offset: 0,
            out_expected: 0,
            out_data: Vec::new(),
            stalled: false,
        }
    }

    fn begin(&mut self, setup: SetupPacket) {
        self.setup = Some(setup);
        self.in_data.clear();
        self.in_offset = 0;
        self.out_expected = 0;
        self.out_data.clear();
        self.stalled = false;

        if setup.length == 0 {
            self.stage = Ep0Stage::StatusIn;
            return;
        }

        if setup.request_type & 0x80 != 0 {
            self.stage = Ep0Stage::DataIn;
        } else {
            self.stage = Ep0Stage::DataOut;
            self.out_expected = setup.length as usize;
        }
    }

    fn setup(&self) -> SetupPacket {
        self.setup.expect("control transfer missing SETUP")
    }
}

fn hub_bitmap_len(num_ports: usize) -> usize {
    (num_ports + 1 + 7) / 8
}

fn build_hub_config_descriptor(interrupt_bitmap_len: usize) -> Vec<u8> {
    let w_max_packet_size = u16::try_from(interrupt_bitmap_len)
        .expect("interrupt bitmap length fits in u16")
        .to_le_bytes();

    // Config(9) + Interface(9) + Endpoint(7) = 25 bytes
    vec![
        // Configuration descriptor
        0x09, // bLength
        USB_DESCRIPTOR_TYPE_CONFIGURATION,
        25,
        0x00, // wTotalLength
        0x01, // bNumInterfaces
        0x01, // bConfigurationValue
        0x00, // iConfiguration
        0xa0, // bmAttributes (bus powered + remote wake)
        50,   // bMaxPower (100mA)
        // Interface descriptor
        0x09, // bLength
        USB_DESCRIPTOR_TYPE_INTERFACE,
        0x00, // bInterfaceNumber
        0x00, // bAlternateSetting
        0x01, // bNumEndpoints
        0x09, // bInterfaceClass (Hub)
        0x00, // bInterfaceSubClass
        0x00, // bInterfaceProtocol
        0x00, // iInterface
        // Endpoint descriptor (Interrupt IN)
        0x07, // bLength
        USB_DESCRIPTOR_TYPE_ENDPOINT,
        HUB_INTERRUPT_IN_EP_ADDR, // bEndpointAddress
        0x03,                     // bmAttributes (Interrupt)
        w_max_packet_size[0],
        w_max_packet_size[1], // wMaxPacketSize
        0x0c,                 // bInterval
    ]
}

fn build_hub_descriptor(num_ports: usize) -> Vec<u8> {
    // USB 2.0 hub class spec 11.23.2.1:
    // - bits 0..=1: power switching mode (01b = per-port).
    // - bits 3..=4: over-current protection mode (10b = no over-current reporting).
    const HUB_W_HUB_CHARACTERISTICS: u16 = 0x0011;

    let bitmap_len = hub_bitmap_len(num_ports);
    let mut port_pwr_ctrl_mask = vec![0u8; bitmap_len];
    for port in 1..=num_ports {
        let byte = port / 8;
        let bit = port % 8;
        port_pwr_ctrl_mask[byte] |= 1u8 << bit;
    }

    let mut desc = Vec::with_capacity(7 + 2 * bitmap_len);
    desc.push((7 + 2 * bitmap_len) as u8); // bLength
    desc.push(USB_DESCRIPTOR_TYPE_HUB);
    desc.push(num_ports as u8); // bNbrPorts
    desc.extend_from_slice(&HUB_W_HUB_CHARACTERISTICS.to_le_bytes()); // wHubCharacteristics
    desc.push(0x01); // bPwrOn2PwrGood (2ms)
    desc.push(0x00); // bHubContrCurrent
    desc.extend(core::iter::repeat(0u8).take(bitmap_len)); // DeviceRemovable
    desc.extend_from_slice(&port_pwr_ctrl_mask); // PortPwrCtrlMask
    desc
}

fn string_descriptor_utf16le(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + s.len() * 2);
    out.push(0); // bLength placeholder
    out.push(USB_DESCRIPTOR_TYPE_STRING);
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out[0] = out.len() as u8;
    out
}

fn string_descriptor_langid(langid: u16) -> [u8; 4] {
    let [l0, l1] = langid.to_le_bytes();
    [4, USB_DESCRIPTOR_TYPE_STRING, l0, l1]
}

struct HubPort {
    device: Option<Box<dyn UsbDevice>>,
    connected: bool,
    connect_change: bool,
    enabled: bool,
    enable_change: bool,
    suspended: bool,
    suspend_change: bool,
    powered: bool,
    reset: bool,
    reset_countdown_ms: u8,
    reset_change: bool,
}

impl HubPort {
    fn new() -> Self {
        Self {
            device: None,
            connected: false,
            connect_change: false,
            enabled: false,
            enable_change: false,
            suspended: false,
            suspend_change: false,
            powered: false,
            reset: false,
            reset_countdown_ms: 0,
            reset_change: false,
        }
    }

    fn attach(&mut self, mut device: Box<dyn UsbDevice>) {
        device.reset();
        self.device = Some(device);
        self.connected = true;
        self.connect_change = true;
        self.suspended = false;
        self.suspend_change = false;
        self.set_enabled(false);
    }

    fn detach(&mut self) {
        self.device = None;
        if self.connected {
            self.connected = false;
            self.connect_change = true;
        }
        self.suspended = false;
        self.suspend_change = false;
        self.set_enabled(false);
    }

    fn set_powered(&mut self, powered: bool) {
        if powered == self.powered {
            return;
        }
        self.powered = powered;
        if !self.powered {
            // Losing VBUS effectively power-cycles the downstream device.
            if let Some(dev) = self.device.as_mut() {
                dev.reset();
            }
            self.suspended = false;
            self.suspend_change = false;
            self.set_enabled(false);
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            if !(self.powered && self.connected) {
                return;
            }
            if self.reset {
                return;
            }
        }

        if enabled != self.enabled {
            self.enabled = enabled;
            self.enable_change = true;
        }

        if !self.enabled {
            self.suspended = false;
            self.suspend_change = false;
        }
    }

    fn set_suspended(&mut self, suspended: bool) {
        if suspended {
            if !(self.enabled && self.powered && self.connected) {
                return;
            }
            if self.reset {
                return;
            }
        }

        if suspended != self.suspended {
            self.suspended = suspended;
            self.suspend_change = true;
        }
    }

    fn start_reset(&mut self) {
        if self.reset {
            return;
        }
        self.reset = true;
        self.reset_countdown_ms = 50;
        self.reset_change = false;

        if let Some(dev) = self.device.as_mut() {
            dev.reset();
        }

        self.suspended = false;
        self.suspend_change = false;

        if self.enabled {
            self.set_enabled(false);
        }
    }

    fn tick_1ms(&mut self) {
        if self.reset {
            self.reset_countdown_ms = self.reset_countdown_ms.saturating_sub(1);
            if self.reset_countdown_ms == 0 {
                self.reset = false;
                self.reset_change = true;

                let should_enable = self.powered && self.connected;
                if should_enable && !self.enabled {
                    self.set_enabled(true);
                }
            }
        }
    }

    fn port_status(&self) -> u16 {
        let mut st = 0u16;
        if self.connected {
            st |= HUB_PORT_STATUS_CONNECTION;
        }
        if self.enabled {
            st |= HUB_PORT_STATUS_ENABLE;
        }
        if self.suspended {
            st |= HUB_PORT_STATUS_SUSPEND;
        }
        if self.reset {
            st |= HUB_PORT_STATUS_RESET;
        }
        if self.powered {
            st |= HUB_PORT_STATUS_POWER;
        }
        st
    }

    fn port_change(&self) -> u16 {
        let mut ch = 0u16;
        if self.connect_change {
            ch |= HUB_PORT_CHANGE_CONNECTION;
        }
        if self.enable_change {
            ch |= HUB_PORT_CHANGE_ENABLE;
        }
        if self.suspend_change {
            ch |= HUB_PORT_CHANGE_SUSPEND;
        }
        if self.reset_change {
            ch |= HUB_PORT_CHANGE_RESET;
        }
        ch
    }

    fn has_change(&self) -> bool {
        self.connect_change || self.enable_change || self.suspend_change || self.reset_change
    }
}

/// External USB 1.1 hub device (class 0x09).
///
/// This is a USB device which, once enumerated, exposes downstream ports and forwards packets
/// based on downstream device addresses. [`crate::usb::UsbBus`] uses topology-aware routing to
/// locate devices attached behind hubs.
pub struct UsbHubDevice {
    address: u8,
    pending_address: Option<u8>,
    configuration: u8,
    pending_configuration: Option<u8>,
    remote_wakeup_enabled: bool,
    ports: Vec<HubPort>,
    interrupt_bitmap_len: usize,
    interrupt_ep_halted: bool,
    config_descriptor: Vec<u8>,
    hub_descriptor: Vec<u8>,
    ep0: Ep0Control,
}

impl UsbHubDevice {
    pub fn new() -> Self {
        Self::new_with_ports(DEFAULT_HUB_NUM_PORTS)
    }

    pub fn with_port_count(num_ports: u8) -> Self {
        Self::new_with_ports(num_ports as usize)
    }

    pub fn new_with_ports(num_ports: usize) -> Self {
        assert!(
            (1..=u8::MAX as usize).contains(&num_ports),
            "hub port count must be 1..=255"
        );

        let interrupt_bitmap_len = hub_bitmap_len(num_ports);
        Self {
            address: 0,
            pending_address: None,
            configuration: 0,
            pending_configuration: None,
            remote_wakeup_enabled: false,
            ports: (0..num_ports).map(|_| HubPort::new()).collect(),
            interrupt_bitmap_len,
            interrupt_ep_halted: false,
            config_descriptor: build_hub_config_descriptor(interrupt_bitmap_len),
            hub_descriptor: build_hub_descriptor(num_ports),
            ep0: Ep0Control::new(),
        }
    }

    /// Attaches `device` to downstream hub port `port` (1-based, as per USB hub spec).
    pub fn attach(&mut self, port: u8, device: Box<dyn UsbDevice>) {
        if port == 0 {
            return;
        }
        let idx = (port - 1) as usize;
        if let Some(p) = self.ports.get_mut(idx) {
            p.attach(device);
        }
    }

    /// Detaches any device from downstream hub port `port` (1-based).
    pub fn detach(&mut self, port: u8) {
        if port == 0 {
            return;
        }
        let idx = (port - 1) as usize;
        if let Some(p) = self.ports.get_mut(idx) {
            p.detach();
        }
    }

    fn port_mut(&mut self, port: u16) -> Option<&mut HubPort> {
        if port == 0 {
            return None;
        }
        let idx = (port - 1) as usize;
        self.ports.get_mut(idx)
    }

    fn finalize_control(&mut self) {
        if let Some(addr) = self.pending_address.take() {
            self.address = addr;
        }
        if let Some(cfg) = self.pending_configuration.take() {
            self.configuration = cfg;
        }
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(string_descriptor_langid(0x0409).to_vec()), // en-US
            1 => Some(string_descriptor_utf16le("Aero")),
            2 => Some(string_descriptor_utf16le("Aero USB Hub")),
            _ => None,
        }
    }

    fn handle_setup_inner(&mut self, setup: SetupPacket) -> Option<Vec<u8>> {
        match (setup.request_type, setup.request) {
            (0x80, USB_REQUEST_GET_STATUS) => {
                if setup.value != 0 || setup.index != 0 {
                    return None;
                }
                // USB 2.0 spec 9.4.5: bit1 is Remote Wakeup.
                let status: u16 = u16::from(self.remote_wakeup_enabled) << 1;
                Some(status.to_le_bytes().to_vec())
            }
            (0x81, USB_REQUEST_GET_STATUS) => {
                // Hub has a single interface (0). Interface GET_STATUS has no defined flags.
                (setup.index == 0 && setup.value == 0).then_some(vec![0, 0])
            }
            (0x82, USB_REQUEST_GET_STATUS) => {
                if setup.value != 0 {
                    return None;
                }
                let ep = (setup.index & 0x00ff) as u8;
                if ep != HUB_INTERRUPT_IN_EP_ADDR {
                    return None;
                }
                let status: u16 = u16::from(self.interrupt_ep_halted);
                Some(status.to_le_bytes().to_vec())
            }
            (0x80, USB_REQUEST_GET_DESCRIPTOR) => {
                let desc_type = (setup.value >> 8) as u8;
                let desc_index = (setup.value & 0x00ff) as u8;
                match desc_type {
                    USB_DESCRIPTOR_TYPE_DEVICE => Some(HUB_DEVICE_DESCRIPTOR.to_vec()),
                    USB_DESCRIPTOR_TYPE_CONFIGURATION => Some(self.config_descriptor.clone()),
                    USB_DESCRIPTOR_TYPE_STRING => self.string_descriptor(desc_index),
                    // Some host stacks probe descriptor type 0x29 using a standard GET_DESCRIPTOR
                    // despite it being class-specific.
                    USB_DESCRIPTOR_TYPE_HUB => Some(self.hub_descriptor.clone()),
                    _ => None,
                }
            }
            (0x80, USB_REQUEST_GET_CONFIGURATION) => Some(vec![self.configuration]),
            (0x81, USB_REQUEST_GET_INTERFACE) => (setup.value == 0 && setup.index == 0)
                .then_some(vec![0u8]),
            // --- Hub class requests ---
            (0xa0, USB_REQUEST_GET_DESCRIPTOR) => {
                if setup.index != 0 {
                    return None;
                }
                if (setup.value >> 8) as u8 != USB_DESCRIPTOR_TYPE_HUB {
                    return None;
                }
                Some(self.hub_descriptor.clone())
            }
            (0xa0, USB_REQUEST_GET_STATUS) => {
                if setup.value != 0 || setup.index != 0 {
                    return None;
                }
                Some(vec![0, 0, 0, 0])
            }
            (0xa3, USB_REQUEST_GET_STATUS) => {
                if setup.value != 0 {
                    return None;
                }
                let port = self.port_mut(setup.index)?;
                let st = port.port_status().to_le_bytes();
                let ch = port.port_change().to_le_bytes();
                Some(vec![st[0], st[1], ch[0], ch[1]])
            }
            _ => None,
        }
    }

    fn handle_no_data_request(&mut self, setup: SetupPacket) -> bool {
        match (setup.request_type, setup.request) {
            // --- Standard device requests ---
            (0x00, USB_REQUEST_SET_ADDRESS) => {
                if setup.value > 127 || setup.index != 0 {
                    return false;
                }
                self.pending_address = Some((setup.value & 0x007f) as u8);
                true
            }
            (0x00, USB_REQUEST_SET_CONFIGURATION) => {
                if setup.index != 0 {
                    return false;
                }
                let cfg = (setup.value & 0x00ff) as u8;
                if cfg > 1 {
                    return false;
                }
                self.pending_configuration = Some(cfg);
                true
            }
            (0x00, USB_REQUEST_CLEAR_FEATURE) => {
                if setup.index != 0 {
                    return false;
                }
                if setup.value != USB_FEATURE_DEVICE_REMOTE_WAKEUP {
                    return false;
                }
                self.remote_wakeup_enabled = false;
                true
            }
            (0x00, USB_REQUEST_SET_FEATURE) => {
                if setup.index != 0 {
                    return false;
                }
                if setup.value != USB_FEATURE_DEVICE_REMOTE_WAKEUP {
                    return false;
                }
                self.remote_wakeup_enabled = true;
                true
            }
            (0x01, USB_REQUEST_SET_INTERFACE) => setup.value == 0 && setup.index == 0,
            // Endpoint halt controls interrupt endpoint polling.
            (0x02, USB_REQUEST_CLEAR_FEATURE) => {
                if setup.value != USB_FEATURE_ENDPOINT_HALT {
                    return false;
                }
                let ep = (setup.index & 0x00ff) as u8;
                if ep != HUB_INTERRUPT_IN_EP_ADDR {
                    return false;
                }
                self.interrupt_ep_halted = false;
                true
            }
            (0x02, USB_REQUEST_SET_FEATURE) => {
                if setup.value != USB_FEATURE_ENDPOINT_HALT {
                    return false;
                }
                let ep = (setup.index & 0x00ff) as u8;
                if ep != HUB_INTERRUPT_IN_EP_ADDR {
                    return false;
                }
                self.interrupt_ep_halted = true;
                true
            }
            // --- Hub class requests ---
            (0x20, USB_REQUEST_CLEAR_FEATURE) => matches!(
                setup.value,
                HUB_FEATURE_C_HUB_LOCAL_POWER | HUB_FEATURE_C_HUB_OVER_CURRENT
            ),
            (0x23, USB_REQUEST_SET_FEATURE) => {
                let port = match self.port_mut(setup.index) {
                    Some(p) => p,
                    None => return false,
                };
                match setup.value {
                    HUB_PORT_FEATURE_ENABLE => {
                        port.set_enabled(true);
                        true
                    }
                    HUB_PORT_FEATURE_SUSPEND => {
                        port.set_suspended(true);
                        true
                    }
                    HUB_PORT_FEATURE_POWER => {
                        port.set_powered(true);
                        true
                    }
                    HUB_PORT_FEATURE_RESET => {
                        port.start_reset();
                        true
                    }
                    _ => false,
                }
            }
            (0x23, USB_REQUEST_CLEAR_FEATURE) => {
                let port = match self.port_mut(setup.index) {
                    Some(p) => p,
                    None => return false,
                };
                match setup.value {
                    HUB_PORT_FEATURE_ENABLE => {
                        port.set_enabled(false);
                        true
                    }
                    HUB_PORT_FEATURE_SUSPEND => {
                        port.set_suspended(false);
                        true
                    }
                    HUB_PORT_FEATURE_POWER => {
                        port.set_powered(false);
                        true
                    }
                    HUB_PORT_FEATURE_C_PORT_CONNECTION => {
                        port.connect_change = false;
                        true
                    }
                    HUB_PORT_FEATURE_C_PORT_ENABLE => {
                        port.enable_change = false;
                        true
                    }
                    HUB_PORT_FEATURE_C_PORT_SUSPEND => {
                        port.suspend_change = false;
                        true
                    }
                    HUB_PORT_FEATURE_C_PORT_OVER_CURRENT => true,
                    HUB_PORT_FEATURE_C_PORT_RESET => {
                        port.reset_change = false;
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn handle_interrupt_in(&mut self, buf: &mut [u8]) -> UsbHandshake {
        if self.configuration == 0 {
            return UsbHandshake::Nak;
        }
        if self.interrupt_ep_halted {
            return UsbHandshake::Stall;
        }

        let mut any_change = false;
        let len = self.interrupt_bitmap_len.min(buf.len());
        buf[..len].fill(0);

        for (idx, port) in self.ports.iter().enumerate() {
            if !port.has_change() {
                continue;
            }
            any_change = true;
            let bit = idx + 1;
            let byte = bit / 8;
            let bit_pos = bit % 8;
            if byte < len {
                buf[byte] |= 1u8 << bit_pos;
            }
        }

        if any_change {
            UsbHandshake::Ack { bytes: len }
        } else {
            UsbHandshake::Nak
        }
    }
}

impl Default for UsbHubDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDevice for UsbHubDevice {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn as_hub(&self) -> Option<&dyn UsbHub> {
        Some(self)
    }

    fn as_hub_mut(&mut self) -> Option<&mut dyn UsbHub> {
        Some(self)
    }

    fn tick_1ms(&mut self) {
        UsbHub::tick_1ms(self);
    }

    fn reset(&mut self) {
        self.address = 0;
        self.pending_address = None;
        self.configuration = 0;
        self.pending_configuration = None;
        self.remote_wakeup_enabled = false;
        self.interrupt_ep_halted = false;
        self.ep0 = Ep0Control::new();

        for port in &mut self.ports {
            let was_enabled = port.enabled;

            // Preserve physical attachment, but clear any host-visible state that is invalidated by
            // an upstream bus reset.
            port.connected = port.device.is_some();
            port.connect_change = port.connected;

            port.enabled = false;
            // An upstream bus reset disables downstream ports; flag a change if the port was
            // previously enabled so the host can notice once the hub is reconfigured.
            port.enable_change = was_enabled;
            port.suspended = false;
            port.suspend_change = false;
            port.reset = false;
            port.reset_countdown_ms = 0;
            port.reset_change = false;

            if let Some(dev) = port.device.as_mut() {
                dev.reset();
            }
        }
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn handle_setup(&mut self, setup: SetupPacket) {
        self.ep0.begin(setup);

        let supported = if setup.length == 0 {
            self.handle_no_data_request(setup)
        } else if setup.request_type & 0x80 != 0 {
            if let Some(mut data) = self.handle_setup_inner(setup) {
                data.truncate(setup.length as usize);
                self.ep0.in_data = data;
                true
            } else {
                false
            }
        } else {
            false
        };

        if !supported {
            self.ep0.stalled = true;
        }
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataOut => {
                self.ep0.out_data.extend_from_slice(data);
                if self.ep0.out_data.len() >= self.ep0.out_expected {
                    let setup = self.ep0.setup();
                    let _ = self.handle_no_data_request(setup);
                    self.ep0.stage = Ep0Stage::StatusIn;
                }
                UsbHandshake::Ack { bytes: data.len() }
            }
            Ep0Stage::StatusOut => {
                self.ep0.stage = Ep0Stage::Idle;
                self.ep0.setup = None;
                self.finalize_control();
                UsbHandshake::Ack { bytes: 0 }
            }
            _ => UsbHandshake::Nak,
        }
    }

    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> UsbHandshake {
        if ep == HUB_INTERRUPT_IN_EP_NUM {
            return self.handle_interrupt_in(buf);
        }

        if ep != 0 {
            return UsbHandshake::Stall;
        }
        if self.ep0.stalled {
            return UsbHandshake::Stall;
        }

        match self.ep0.stage {
            Ep0Stage::DataIn => {
                let remaining = self.ep0.in_data.len().saturating_sub(self.ep0.in_offset);
                let len = buf.len().min(remaining);
                buf[..len].copy_from_slice(
                    &self.ep0.in_data[self.ep0.in_offset..self.ep0.in_offset + len],
                );
                self.ep0.in_offset += len;
                if self.ep0.in_offset >= self.ep0.in_data.len() {
                    self.ep0.stage = Ep0Stage::StatusOut;
                }
                UsbHandshake::Ack { bytes: len }
            }
            Ep0Stage::StatusIn => {
                self.ep0.stage = Ep0Stage::Idle;
                self.ep0.setup = None;
                self.finalize_control();
                UsbHandshake::Ack { bytes: 0 }
            }
            _ => UsbHandshake::Nak,
        }
    }
}

impl UsbHub for UsbHubDevice {
    fn tick_1ms(&mut self) {
        for port in &mut self.ports {
            port.tick_1ms();

            if !(port.enabled && port.powered) {
                continue;
            }
            if let Some(dev) = port.device.as_mut() {
                dev.tick_1ms();
            }
        }
    }

    fn downstream_device_mut_for_address(&mut self, address: u8) -> Option<&mut dyn UsbDevice> {
        for port in &mut self.ports {
            if !(port.enabled && port.powered) {
                continue;
            }
            let Some(dev) = port.device.as_mut() else {
                continue;
            };
            if dev.address() == address {
                return Some(dev.as_mut());
            }
            if let Some(hub) = dev.as_hub_mut() {
                if let Some(found) = hub.downstream_device_mut_for_address(address) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn downstream_device_mut(&mut self, port: usize) -> Option<&mut dyn UsbDevice> {
        Some(self.ports.get_mut(port)?.device.as_mut()?.as_mut())
    }

    fn attach_downstream(&mut self, port: usize, device: Box<dyn UsbDevice>) {
        if let Some(p) = self.ports.get_mut(port) {
            p.attach(device);
        }
    }

    fn detach_downstream(&mut self, port: usize) {
        if let Some(p) = self.ports.get_mut(port) {
            p.detach();
        }
    }

    fn num_ports(&self) -> usize {
        self.ports.len()
    }
}

static HUB_DEVICE_DESCRIPTOR: [u8; 18] = [
    0x12, // bLength
    USB_DESCRIPTOR_TYPE_DEVICE,
    0x10,
    0x01, // bcdUSB (1.10)
    0x09, // bDeviceClass (Hub)
    0x00, // bDeviceSubClass
    0x00, // bDeviceProtocol (Full-speed hub)
    0x40, // bMaxPacketSize0 (64)
    0x34,
    0x12, // idVendor (0x1234)
    0x02,
    0x00, // idProduct (0x0002)
    0x00,
    0x01, // bcdDevice (1.00)
    0x01, // iManufacturer
    0x02, // iProduct
    0x00, // iSerialNumber
    0x01, // bNumConfigurations
];
