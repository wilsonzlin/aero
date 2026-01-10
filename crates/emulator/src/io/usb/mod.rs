pub mod core;
pub mod hid;
pub mod hub;
pub mod uhci;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupPacket {
    pub bm_request_type: u8,
    pub b_request: u8,
    pub w_value: u16,
    pub w_index: u16,
    pub w_length: u16,
}

impl SetupPacket {
    pub fn from_bytes(bytes: [u8; 8]) -> Self {
        Self {
            bm_request_type: bytes[0],
            b_request: bytes[1],
            w_value: u16::from_le_bytes([bytes[2], bytes[3]]),
            w_index: u16::from_le_bytes([bytes[4], bytes[5]]),
            w_length: u16::from_le_bytes([bytes[6], bytes[7]]),
        }
    }

    pub fn descriptor_type(&self) -> u8 {
        (self.w_value >> 8) as u8
    }

    pub fn descriptor_index(&self) -> u8 {
        (self.w_value & 0x00ff) as u8
    }

    pub fn request_direction(&self) -> RequestDirection {
        if (self.bm_request_type & 0x80) != 0 {
            RequestDirection::DeviceToHost
        } else {
            RequestDirection::HostToDevice
        }
    }

    pub fn request_type(&self) -> RequestType {
        match (self.bm_request_type >> 5) & 0x03 {
            0 => RequestType::Standard,
            1 => RequestType::Class,
            2 => RequestType::Vendor,
            _ => RequestType::Reserved,
        }
    }

    pub fn recipient(&self) -> RequestRecipient {
        match self.bm_request_type & 0x1f {
            0 => RequestRecipient::Device,
            1 => RequestRecipient::Interface,
            2 => RequestRecipient::Endpoint,
            3 => RequestRecipient::Other,
            _ => RequestRecipient::Reserved,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestDirection {
    HostToDevice,
    DeviceToHost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestType {
    Standard,
    Class,
    Vendor,
    Reserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestRecipient {
    Device,
    Interface,
    Endpoint,
    Other,
    Reserved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlResponse {
    Data(Vec<u8>),
    Ack,
    Stall,
}

pub trait UsbDeviceModel {
    fn get_device_descriptor(&self) -> &'static [u8];
    fn get_config_descriptor(&self) -> &'static [u8];
    fn get_hid_report_descriptor(&self) -> &'static [u8];

    /// Resets device state due to a USB bus reset (e.g. PORTSC reset).
    ///
    /// Implementations should return to an unconfigured, default-address state.
    fn reset(&mut self) {}

    /// Handles a USB control transfer SETUP packet.
    ///
    /// For OUT requests, `data_stage` contains the payload provided by the host.
    /// For IN requests, `data_stage` is typically `None`.
    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse;

    /// Polls an interrupt IN endpoint and returns the next queued report (if any).
    ///
    /// Returning `None` indicates the endpoint would NAK (no data available).
    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>>;
}
