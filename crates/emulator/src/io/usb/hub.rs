use crate::io::usb::core::AttachedUsbDevice;
use crate::io::usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};

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

const USB_DESCRIPTOR_TYPE_DEVICE: u8 = 0x01;
const USB_DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;
const USB_DESCRIPTOR_TYPE_STRING: u8 = 0x03;
const USB_DESCRIPTOR_TYPE_INTERFACE: u8 = 0x04;
const USB_DESCRIPTOR_TYPE_ENDPOINT: u8 = 0x05;
const USB_DESCRIPTOR_TYPE_HUB: u8 = 0x29;

const USB_REQUEST_GET_STATUS: u8 = 0x00;
const USB_REQUEST_CLEAR_FEATURE: u8 = 0x01;
const USB_REQUEST_SET_FEATURE: u8 = 0x03;
const USB_REQUEST_GET_DESCRIPTOR: u8 = 0x06;
const USB_REQUEST_GET_CONFIGURATION: u8 = 0x08;
const USB_REQUEST_SET_CONFIGURATION: u8 = 0x09;

const HUB_PORT_FEATURE_RESET: u16 = 4;
const HUB_PORT_FEATURE_POWER: u16 = 8;
const HUB_PORT_FEATURE_C_PORT_CONNECTION: u16 = 16;
const HUB_PORT_FEATURE_C_PORT_ENABLE: u16 = 17;
const HUB_PORT_FEATURE_C_PORT_RESET: u16 = 20;

const HUB_PORT_STATUS_CONNECTION: u16 = 1 << 0;
const HUB_PORT_STATUS_ENABLE: u16 = 1 << 1;
const HUB_PORT_STATUS_RESET: u16 = 1 << 4;
const HUB_PORT_STATUS_POWER: u16 = 1 << 8;

const HUB_PORT_CHANGE_CONNECTION: u16 = 1 << 0;
const HUB_PORT_CHANGE_ENABLE: u16 = 1 << 1;
const HUB_PORT_CHANGE_RESET: u16 = 1 << 4;

const HUB_INTERRUPT_IN_EP: u8 = 0x81;

const HUB_NUM_PORTS: usize = 4;
const HUB_CHANGE_BITMAP_LEN: usize = (HUB_NUM_PORTS + 1 + 7) / 8;

struct HubPort {
    device: Option<AttachedUsbDevice>,
    connected: bool,
    connect_change: bool,
    enabled: bool,
    enable_change: bool,
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
            powered: false,
            reset: false,
            reset_countdown_ms: 0,
            reset_change: false,
        }
    }

    fn attach(&mut self, model: Box<dyn UsbDeviceModel>) {
        self.device = Some(AttachedUsbDevice::new(model));
        if !self.connected {
            self.connected = true;
        }
        self.connect_change = true;
        if self.enabled {
            self.enabled = false;
            self.enable_change = true;
        }
    }

    fn detach(&mut self) {
        self.device = None;
        if self.connected {
            self.connected = false;
            self.connect_change = true;
        }
        if self.enabled {
            self.enabled = false;
            self.enable_change = true;
        }
    }

    fn set_powered(&mut self, powered: bool) {
        if powered == self.powered {
            return;
        }
        self.powered = powered;
        if !self.powered {
            if self.enabled {
                self.enabled = false;
                self.enable_change = true;
            }
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

        if self.enabled {
            self.enabled = false;
            self.enable_change = true;
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
                    self.enabled = true;
                    self.enable_change = true;
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
        if self.reset_change {
            ch |= HUB_PORT_CHANGE_RESET;
        }
        ch
    }

    fn has_change(&self) -> bool {
        self.connect_change || self.enable_change || self.reset_change
    }
}

/// External USB 1.1 hub device (class 0x09).
///
/// This is a USB device which, once enumerated, exposes downstream ports and forwards packets
/// based on the downstream device address. The UHCI controller uses topology-aware routing to
/// locate devices attached behind hubs.
pub struct UsbHubDevice {
    configuration: u8,
    ports: [HubPort; HUB_NUM_PORTS],
}

impl UsbHubDevice {
    pub fn new() -> Self {
        Self {
            configuration: 0,
            ports: std::array::from_fn(|_| HubPort::new()),
        }
    }

    pub fn attach(&mut self, port: u8, model: Box<dyn UsbDeviceModel>) {
        if port == 0 {
            return;
        }
        let idx = (port - 1) as usize;
        if idx >= self.ports.len() {
            return;
        }
        self.ports[idx].attach(model);
    }

    #[allow(dead_code)]
    pub fn detach(&mut self, port: u8) {
        if port == 0 {
            return;
        }
        let idx = (port - 1) as usize;
        if idx >= self.ports.len() {
            return;
        }
        self.ports[idx].detach();
    }

    fn port_mut(&mut self, port: u16) -> Option<&mut HubPort> {
        if port == 0 {
            return None;
        }
        let idx = (port - 1) as usize;
        self.ports.get_mut(idx)
    }

    fn string_descriptor(&self, index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, USB_DESCRIPTOR_TYPE_STRING, 0x09, 0x04]), // en-US
            1 => Some(build_string_descriptor_utf16le("Aero")),
            2 => Some(build_string_descriptor_utf16le("Aero USB Hub")),
            _ => None,
        }
    }
}

impl Default for UsbHubDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbHubDevice {
    fn get_device_descriptor(&self) -> &'static [u8] {
        &HUB_DEVICE_DESCRIPTOR
    }

    fn get_config_descriptor(&self) -> &'static [u8] {
        &HUB_CONFIG_DESCRIPTOR
    }

    fn get_hid_report_descriptor(&self) -> &'static [u8] {
        &[]
    }

    fn reset(&mut self) {
        self.configuration = 0;

        for port in &mut self.ports {
            port.enabled = false;
            port.enable_change = false;
            port.powered = false;
            port.reset = false;
            port.reset_countdown_ms = 0;
            port.reset_change = false;

            port.connected = port.device.is_some();
            port.connect_change = port.connected;

            if let Some(dev) = port.device.as_mut() {
                dev.reset();
            }
        }
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        match (setup.request_type(), setup.recipient()) {
            (RequestType::Standard, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0, 0], setup.w_length))
                }
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost {
                        return ControlResponse::Stall;
                    }
                    let desc_type = setup.descriptor_type();
                    let desc_index = setup.descriptor_index();
                    let data = match desc_type {
                        USB_DESCRIPTOR_TYPE_DEVICE => Some(self.get_device_descriptor().to_vec()),
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => {
                            Some(self.get_config_descriptor().to_vec())
                        }
                        USB_DESCRIPTOR_TYPE_STRING => self.string_descriptor(desc_index),
                        _ => None,
                    };
                    data.map(|v| ControlResponse::Data(clamp_response(v, setup.w_length)))
                        .unwrap_or(ControlResponse::Stall)
                }
                USB_REQUEST_SET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_index != 0
                        || setup.w_length != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let config = (setup.w_value & 0x00ff) as u8;
                    if config > 1 {
                        return ControlResponse::Stall;
                    }
                    self.configuration = config;
                    ControlResponse::Ack
                }
                USB_REQUEST_GET_CONFIGURATION => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![self.configuration], setup.w_length))
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Class, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    if setup.descriptor_type() != USB_DESCRIPTOR_TYPE_HUB {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(
                        HUB_DESCRIPTOR.to_vec(),
                        setup.w_length,
                    ))
                }
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0, 0, 0, 0], setup.w_length))
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Class, RequestRecipient::Other) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                        || setup.w_length != 4
                    {
                        return ControlResponse::Stall;
                    }
                    let Some(port) = self.port_mut(setup.w_index) else {
                        return ControlResponse::Stall;
                    };
                    let st = port.port_status().to_le_bytes();
                    let ch = port.port_change().to_le_bytes();
                    ControlResponse::Data(clamp_response(
                        vec![st[0], st[1], ch[0], ch[1]],
                        setup.w_length,
                    ))
                }
                USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_length != 0 {
                        return ControlResponse::Stall;
                    }
                    let Some(port) = self.port_mut(setup.w_index) else {
                        return ControlResponse::Stall;
                    };
                    match setup.w_value {
                        HUB_PORT_FEATURE_POWER => {
                            port.set_powered(true);
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_RESET => {
                            port.start_reset();
                            ControlResponse::Ack
                        }
                        _ => ControlResponse::Stall,
                    }
                }
                USB_REQUEST_CLEAR_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_length != 0 {
                        return ControlResponse::Stall;
                    }
                    let Some(port) = self.port_mut(setup.w_index) else {
                        return ControlResponse::Stall;
                    };
                    match setup.w_value {
                        HUB_PORT_FEATURE_C_PORT_CONNECTION => {
                            port.connect_change = false;
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_C_PORT_ENABLE => {
                            port.enable_change = false;
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_C_PORT_RESET => {
                            port.reset_change = false;
                            ControlResponse::Ack
                        }
                        _ => ControlResponse::Stall,
                    }
                }
                _ => ControlResponse::Stall,
            },
            _ => ControlResponse::Stall,
        }
    }

    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>> {
        if ep != HUB_INTERRUPT_IN_EP || self.configuration == 0 {
            return None;
        }

        let mut bitmap = vec![0u8; HUB_CHANGE_BITMAP_LEN];
        for (idx, port) in self.ports.iter().enumerate() {
            if !port.has_change() {
                continue;
            }
            let bit = idx + 1;
            bitmap[bit / 8] |= 1u8 << (bit % 8);
        }
        bitmap.iter().any(|&b| b != 0).then_some(bitmap)
    }

    fn tick_1ms(&mut self) {
        for port in &mut self.ports {
            port.tick_1ms();
            if !port.enabled {
                continue;
            }
            if let Some(dev) = port.device.as_mut() {
                dev.tick_1ms();
            }
        }
    }

    fn child_device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
        for port in &mut self.ports {
            if !(port.enabled && port.powered) {
                continue;
            }
            if let Some(dev) = port.device.as_mut() {
                if let Some(found) = dev.device_mut_for_address(address) {
                    return Some(found);
                }
            }
        }
        None
    }
}

fn clamp_response(mut data: Vec<u8>, setup_w_length: u16) -> Vec<u8> {
    let requested = setup_w_length as usize;
    if data.len() > requested {
        data.truncate(requested);
    }
    data
}

fn build_string_descriptor_utf16le(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + s.len() * 2);
    out.push(0); // bLength filled in later
    out.push(USB_DESCRIPTOR_TYPE_STRING);
    for ch in s.encode_utf16() {
        out.extend_from_slice(&ch.to_le_bytes());
    }
    out[0] = out.len() as u8;
    out
}

static HUB_DEVICE_DESCRIPTOR: [u8; 18] = [
    0x12, // bLength
    USB_DESCRIPTOR_TYPE_DEVICE,
    0x10,
    0x01, // bcdUSB (1.10)
    0x09, // bDeviceClass (Hub)
    0x00, // bDeviceSubClass
    0x01, // bDeviceProtocol (Full-speed hub)
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

// Config(9) + Interface(9) + Endpoint(7) = 25 bytes
static HUB_CONFIG_DESCRIPTOR: [u8; 25] = [
    // Configuration descriptor
    0x09, // bLength
    USB_DESCRIPTOR_TYPE_CONFIGURATION,
    25,
    0x00, // wTotalLength
    0x01, // bNumInterfaces
    0x01, // bConfigurationValue
    0x00, // iConfiguration
    0x80, // bmAttributes (bus powered)
    0x00, // bMaxPower (0mA self-powered not modelled)
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
    HUB_INTERRUPT_IN_EP, // bEndpointAddress
    0x03,               // bmAttributes (Interrupt)
    HUB_CHANGE_BITMAP_LEN as u8,
    0x00, // wMaxPacketSize
    0x0c, // bInterval
];

static HUB_DESCRIPTOR: [u8; 9] = [
    0x09,                 // bLength
    USB_DESCRIPTOR_TYPE_HUB,
    HUB_NUM_PORTS as u8, // bNbrPorts
    0x00,
    0x00, // wHubCharacteristics
    0x01, // bPwrOn2PwrGood (2ms)
    0x00, // bHubContrCurrent
    0x00, // DeviceRemovable
    0xff, // PortPwrCtrlMask
];
