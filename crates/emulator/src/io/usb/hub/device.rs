use crate::io::usb::core::{AttachedUsbDevice, UsbInResult};
use crate::io::usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
    UsbHubAttachError,
};

use super::UsbHub;
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
const USB_REQUEST_GET_INTERFACE: u8 = 0x0a;
const USB_REQUEST_SET_INTERFACE: u8 = 0x0b;

const USB_FEATURE_ENDPOINT_HALT: u16 = 0;

const HUB_FEATURE_C_HUB_LOCAL_POWER: u16 = 0;
const HUB_FEATURE_C_HUB_OVER_CURRENT: u16 = 1;

const HUB_PORT_FEATURE_ENABLE: u16 = 1;
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

const DEFAULT_HUB_NUM_PORTS: usize = 4;

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
        self.connected = true;
        self.connect_change = true;
        self.set_enabled(false);
    }

    fn detach(&mut self) {
        self.device = None;
        if self.connected {
            self.connected = false;
            self.connect_change = true;
        }
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
    ports: Vec<HubPort>,
    interrupt_bitmap_len: usize,
    interrupt_ep_halted: bool,
    config_descriptor: Vec<u8>,
    hub_descriptor: Vec<u8>,
}

impl UsbHubDevice {
    pub fn new() -> Self {
        Self::new_with_ports(DEFAULT_HUB_NUM_PORTS)
    }

    pub fn new_with_ports(num_ports: usize) -> Self {
        assert!(
            (1..=u8::MAX as usize).contains(&num_ports),
            "hub port count must be 1..=255"
        );

        let interrupt_bitmap_len = hub_bitmap_len(num_ports);
        Self {
            configuration: 0,
            ports: (0..num_ports).map(|_| HubPort::new()).collect(),
            interrupt_bitmap_len,
            interrupt_ep_halted: false,
            config_descriptor: build_hub_config_descriptor(interrupt_bitmap_len),
            hub_descriptor: build_hub_descriptor(num_ports),
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
    fn hub_port_count(&self) -> Option<u8> {
        u8::try_from(self.ports.len()).ok()
    }

    fn hub_attach_device(
        &mut self,
        port: u8,
        model: Box<dyn UsbDeviceModel>,
    ) -> Result<(), UsbHubAttachError> {
        if port == 0 {
            return Err(UsbHubAttachError::InvalidPort);
        }
        let idx = (port - 1) as usize;
        let Some(p) = self.ports.get_mut(idx) else {
            return Err(UsbHubAttachError::InvalidPort);
        };
        if p.device.is_some() {
            return Err(UsbHubAttachError::PortOccupied);
        }
        p.attach(model);
        Ok(())
    }

    fn hub_detach_device(&mut self, port: u8) -> Result<(), UsbHubAttachError> {
        if port == 0 {
            return Err(UsbHubAttachError::InvalidPort);
        }
        let idx = (port - 1) as usize;
        let Some(p) = self.ports.get_mut(idx) else {
            return Err(UsbHubAttachError::InvalidPort);
        };
        if p.device.is_none() {
            return Err(UsbHubAttachError::NoDevice);
        }
        p.detach();
        Ok(())
    }

    fn hub_port_device_mut(&mut self, port: u8) -> Result<&mut AttachedUsbDevice, UsbHubAttachError> {
        if port == 0 {
            return Err(UsbHubAttachError::InvalidPort);
        }
        let idx = (port - 1) as usize;
        let Some(p) = self.ports.get_mut(idx) else {
            return Err(UsbHubAttachError::InvalidPort);
        };
        p.device.as_mut().ok_or(UsbHubAttachError::NoDevice)
    }

    fn reset(&mut self) {
        self.configuration = 0;
        self.interrupt_ep_halted = false;

        for port in &mut self.ports {
            let was_enabled = port.enabled;

            // Preserve physical attachment, but clear any host-visible state that is invalidated
            // by an upstream bus reset.
            port.connected = port.device.is_some();
            port.connect_change = port.connected;

            port.enabled = false;
            // An upstream bus reset disables downstream ports; flag a change if the port was
            // previously enabled so the host can notice once the hub is reconfigured.
            port.enable_change = was_enabled;
            port.reset = false;
            port.reset_countdown_ms = 0;
            port.reset_change = false;
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
                        USB_DESCRIPTOR_TYPE_DEVICE => Some(HUB_DEVICE_DESCRIPTOR.to_vec()),
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => Some(self.config_descriptor.clone()),
                        USB_DESCRIPTOR_TYPE_STRING => self.string_descriptor(desc_index),
                        // Accept hub descriptor fetch as both a class request (the common case) and
                        // a standard request. Some host stacks probe descriptor type 0x29 using a
                        // standard GET_DESCRIPTOR despite it being class-specific.
                        USB_DESCRIPTOR_TYPE_HUB => Some(self.hub_descriptor.clone()),
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
            (RequestType::Standard, RequestRecipient::Interface) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    // Hub has a single interface (0). Interface GET_STATUS has no defined flags.
                    if setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0, 0], setup.w_length))
                }
                USB_REQUEST_GET_INTERFACE => {
                    if setup.request_direction() != RequestDirection::DeviceToHost || setup.w_value != 0 {
                        return ControlResponse::Stall;
                    }
                    if setup.w_index != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Data(clamp_response(vec![0], setup.w_length))
                }
                USB_REQUEST_SET_INTERFACE => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_length != 0 {
                        return ControlResponse::Stall;
                    }
                    if setup.w_index != 0 || setup.w_value != 0 {
                        return ControlResponse::Stall;
                    }
                    ControlResponse::Ack
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Standard, RequestRecipient::Endpoint) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
                    {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    if ep != HUB_INTERRUPT_IN_EP {
                        return ControlResponse::Stall;
                    }
                    let status: u16 = u16::from(self.interrupt_ep_halted);
                    ControlResponse::Data(clamp_response(status.to_le_bytes().to_vec(), setup.w_length))
                }
                USB_REQUEST_CLEAR_FEATURE | USB_REQUEST_SET_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice || setup.w_length != 0 {
                        return ControlResponse::Stall;
                    }
                    if setup.w_value != USB_FEATURE_ENDPOINT_HALT {
                        return ControlResponse::Stall;
                    }
                    let ep = (setup.w_index & 0x00ff) as u8;
                    if ep != HUB_INTERRUPT_IN_EP {
                        return ControlResponse::Stall;
                    }
                    self.interrupt_ep_halted = setup.b_request == USB_REQUEST_SET_FEATURE;
                    ControlResponse::Ack
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
                    ControlResponse::Data(clamp_response(self.hub_descriptor.clone(), setup.w_length))
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
                USB_REQUEST_CLEAR_FEATURE => {
                    if setup.request_direction() != RequestDirection::HostToDevice
                        || setup.w_length != 0
                        || setup.w_index != 0
                    {
                        return ControlResponse::Stall;
                    }
                    match setup.w_value {
                        HUB_FEATURE_C_HUB_LOCAL_POWER | HUB_FEATURE_C_HUB_OVER_CURRENT => ControlResponse::Ack,
                        _ => ControlResponse::Stall,
                    }
                }
                _ => ControlResponse::Stall,
            },
            (RequestType::Class, RequestRecipient::Other) => match setup.b_request {
                USB_REQUEST_GET_STATUS => {
                    if setup.request_direction() != RequestDirection::DeviceToHost
                        || setup.w_value != 0
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
                        HUB_PORT_FEATURE_ENABLE => {
                            port.set_enabled(true);
                            ControlResponse::Ack
                        }
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
                        HUB_PORT_FEATURE_ENABLE => {
                            port.set_enabled(false);
                            ControlResponse::Ack
                        }
                        HUB_PORT_FEATURE_POWER => {
                            port.set_powered(false);
                            ControlResponse::Ack
                        }
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

    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        if ep_addr != HUB_INTERRUPT_IN_EP {
            return UsbInResult::Stall;
        }
        if self.configuration == 0 {
            return UsbInResult::Nak;
        }
        if self.interrupt_ep_halted {
            return UsbInResult::Stall;
        }

        let mut bitmap = vec![0u8; self.interrupt_bitmap_len];
        for (idx, port) in self.ports.iter().enumerate() {
            if !port.has_change() {
                continue;
            }
            let bit = idx + 1;
            bitmap[bit / 8] |= 1u8 << (bit % 8);
        }
        if bitmap.iter().any(|&b| b != 0) {
            UsbInResult::Data(bitmap)
        } else {
            UsbInResult::Nak
        }
    }

    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>> {
        if ep != HUB_INTERRUPT_IN_EP || self.configuration == 0 || self.interrupt_ep_halted {
            return None;
        }

        let mut bitmap = vec![0u8; self.interrupt_bitmap_len];
        for (idx, port) in self.ports.iter().enumerate() {
            if !port.has_change() {
                continue;
            }
            let bit = idx + 1;
            bitmap[bit / 8] |= 1u8 << (bit % 8);
        }
        bitmap.iter().any(|&b| b != 0).then_some(bitmap)
    }

    fn as_hub(&self) -> Option<&dyn UsbHub> {
        Some(self)
    }

    fn as_hub_mut(&mut self) -> Option<&mut dyn UsbHub> {
        Some(self)
    }
}

impl UsbHub for UsbHubDevice {
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

    fn downstream_device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
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

    fn downstream_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice> {
        self.ports.get_mut(port)?.device.as_mut()
    }

    fn attach_downstream(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        if let Some(p) = self.ports.get_mut(port) {
            p.attach(model);
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

fn hub_bitmap_len(num_ports: usize) -> usize {
    (num_ports + 1 + 7) / 8
}

fn build_hub_config_descriptor(interrupt_bitmap_len: usize) -> Vec<u8> {
    let max_packet_size: u16 = interrupt_bitmap_len
        .try_into()
        .expect("interrupt bitmap length fits in u16");
    let w_max_packet_size = max_packet_size.to_le_bytes();

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
        0x80, // bmAttributes (bus powered)
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
        HUB_INTERRUPT_IN_EP, // bEndpointAddress
        0x03,               // bmAttributes (Interrupt)
        w_max_packet_size[0],
        w_max_packet_size[1], // wMaxPacketSize
        0x0c, // bInterval
    ]
}

fn build_hub_descriptor(num_ports: usize) -> Vec<u8> {
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
    desc.extend(std::iter::repeat(0u8).take(bitmap_len)); // DeviceRemovable
    desc.extend_from_slice(&port_pwr_ctrl_mask); // PortPwrCtrlMask
    desc
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
