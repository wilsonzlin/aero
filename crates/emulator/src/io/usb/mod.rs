pub mod core;
pub mod descriptor_fixups;
pub mod hid;
pub mod hub;
pub mod passthrough;
pub mod uhci;

pub use core::{UsbInResult, UsbOutResult};

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
    Nak,
    Stall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbHubAttachError {
    NotAHub,
    InvalidPort,
    PortOccupied,
    NoDevice,
}

impl std::fmt::Display for UsbHubAttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbHubAttachError::NotAHub => write!(f, "device is not a USB hub"),
            UsbHubAttachError::InvalidPort => write!(f, "invalid hub port"),
            UsbHubAttachError::PortOccupied => write!(f, "hub port is already occupied"),
            UsbHubAttachError::NoDevice => write!(f, "no device attached to hub port"),
        }
    }
}

impl std::error::Error for UsbHubAttachError {}

pub trait UsbDeviceModel {
    /// Returns the number of downstream ports if this device is a hub.
    fn hub_port_count(&self) -> Option<u8> {
        self.as_hub()
            .and_then(|hub| u8::try_from(hub.num_ports()).ok())
    }

    /// Attach a device model to the specified downstream port (1-based).
    fn hub_attach_device(
        &mut self,
        port: u8,
        model: Box<dyn UsbDeviceModel>,
    ) -> Result<(), UsbHubAttachError> {
        let Some(hub) = self.as_hub_mut() else {
            return Err(UsbHubAttachError::NotAHub);
        };

        let port = port
            .checked_sub(1)
            .ok_or(UsbHubAttachError::InvalidPort)? as usize;
        if port >= hub.num_ports() {
            return Err(UsbHubAttachError::InvalidPort);
        }
        if hub.downstream_device_mut(port).is_some() {
            return Err(UsbHubAttachError::PortOccupied);
        }
        hub.attach_downstream(port, model);
        Ok(())
    }

    /// Detach any device model from the specified downstream port (1-based).
    fn hub_detach_device(&mut self, port: u8) -> Result<(), UsbHubAttachError> {
        let Some(hub) = self.as_hub_mut() else {
            return Err(UsbHubAttachError::NotAHub);
        };

        let port = port
            .checked_sub(1)
            .ok_or(UsbHubAttachError::InvalidPort)? as usize;
        if port >= hub.num_ports() {
            return Err(UsbHubAttachError::InvalidPort);
        }
        if hub.downstream_device_mut(port).is_none() {
            return Err(UsbHubAttachError::NoDevice);
        }
        hub.detach_downstream(port);
        Ok(())
    }

    /// Return the attached device on a downstream port, regardless of whether the port is
    /// powered/enabled.
    ///
    /// This is used by host-side topology management (e.g. `RootHub::attach_at_path`) to
    /// traverse nested hubs by port-number without relying on concrete downcasting.
    #[doc(hidden)]
    fn hub_port_device_mut(
        &mut self,
        port: u8,
    ) -> Result<&mut crate::io::usb::core::AttachedUsbDevice, UsbHubAttachError> {
        let Some(hub) = self.as_hub_mut() else {
            return Err(UsbHubAttachError::NotAHub);
        };

        let port = port
            .checked_sub(1)
            .ok_or(UsbHubAttachError::InvalidPort)? as usize;
        if port >= hub.num_ports() {
            return Err(UsbHubAttachError::InvalidPort);
        }
        hub.downstream_device_mut(port)
            .ok_or(UsbHubAttachError::NoDevice)
    }

    /// Resets device state due to a USB bus reset (e.g. PORTSC reset).
    ///
    /// Implementations should return to an unconfigured, default-address state.
    fn reset(&mut self) {}

    /// Aborts any in-flight control transfer state.
    ///
    /// USB allows a new SETUP packet on endpoint 0 to abort the previous control transfer.
    /// Synchronous device models typically don't need to do anything here, but asynchronous
    /// models (e.g. USB passthrough to WebUSB/libusb) may need to drop queued host actions and
    /// ignore stale completions.
    fn cancel_control_transfer(&mut self) {}

    /// Handles a USB control transfer SETUP packet.
    ///
    /// For OUT requests, `data_stage` contains the payload provided by the host.
    /// For IN requests, `data_stage` is typically `None`.
    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse;

    /// Handles a non-control IN transfer (interrupt/bulk).
    ///
    /// Implementations should return [`UsbInResult::Nak`] when no data is available yet so the
    /// UHCI scheduler can retry the TD in a later frame.
    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult {
        let _ = max_len;
        self.handle_interrupt_in(ep)
    }

    /// Handles a non-control OUT transfer (interrupt/bulk).
    ///
    /// The default implementation delegates to [`UsbDeviceModel::handle_interrupt_out`] for
    /// backwards compatibility with interrupt-only device models.
    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        self.handle_interrupt_out(ep, data)
    }

    /// Legacy helper for interrupt IN endpoints.
    ///
    /// Returning `None` indicates the endpoint would NAK (no data available).
    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        None
    }

    /// Handles an interrupt IN transfer for a non-control endpoint.
    ///
    /// The default implementation bridges the legacy [`UsbDeviceModel::poll_interrupt_in`]
    /// interface, treating `None` as NAK. Device models that implement endpoint halt should
    /// override this to return [`crate::io::usb::core::UsbInResult::Stall`] when halted.
    fn handle_interrupt_in(&mut self, ep_addr: u8) -> crate::io::usb::core::UsbInResult {
        match self.poll_interrupt_in(ep_addr) {
            Some(data) => crate::io::usb::core::UsbInResult::Data(data),
            None => crate::io::usb::core::UsbInResult::Nak,
        }
    }

    /// Handles interrupt OUT transfers for non-control endpoints.
    ///
    /// Returning [`crate::io::usb::core::UsbOutResult::Stall`] indicates the endpoint is not
    /// implemented.
    fn handle_interrupt_out(
        &mut self,
        _ep_addr: u8,
        _data: &[u8],
    ) -> crate::io::usb::core::UsbOutResult {
        crate::io::usb::core::UsbOutResult::Stall
    }

    /// If this device model is a USB hub, expose it as a [`crate::io::usb::hub::UsbHub`] for
    /// topology-aware routing.
    ///
    /// Hub device models should override this to return `Some(self)`.
    fn as_hub(&self) -> Option<&dyn crate::io::usb::hub::UsbHub> {
        None
    }

    /// Mutable variant of [`UsbDeviceModel::as_hub`].
    fn as_hub_mut(&mut self) -> Option<&mut dyn crate::io::usb::hub::UsbHub> {
        None
    }

    /// Advance device-internal timers by 1ms.
    ///
    /// This is used for devices like USB hubs where operations such as port reset
    /// completion are time-based.
    fn tick_1ms(&mut self) {
        if let Some(hub) = self.as_hub_mut() {
            hub.tick_1ms();
        }
    }

    /// If this device is a USB hub, route an address lookup to its downstream devices.
    ///
    /// The UHCI controller uses this for topology-aware device routing: it first matches the
    /// address of the device itself (via [`core::AttachedUsbDevice`]), and then asks hub models
    /// to search their downstream ports.
    fn child_device_mut_for_address(
        &mut self,
        address: u8,
    ) -> Option<&mut crate::io::usb::core::AttachedUsbDevice> {
        self.as_hub_mut()
            .and_then(|hub| hub.downstream_device_mut_for_address(address))
    }
}
