//! USB subsystem building blocks: a minimal UHCI host controller model, early xHCI scaffolding,
//! and basic USB HID devices.
//!
//! This crate is the canonical USB implementation for Aero's browser/WASM runtime
//! (see `docs/adr/0015-canonical-usb-stack.md`).
//!
//! The main consumer is the browser-based emulator, which exposes an emulated UHCI (USB 1.1)
//! controller and USB HID devices (keyboard/mouse/gamepad/consumer-control + passthrough). There is also an
//! in-progress xHCI (USB 3.0) controller model; currently only core data types are implemented.
//!
//! ## Snapshot/restore
//!
//! Several models implement `aero_io_snapshot::io::state::IoSnapshot` using the canonical
//! `aero-io-snapshot` TLV encoding so they can participate in VM save/restore:
//!
//! | Type | `IoSnapshot::DEVICE_ID` |
//! |------|------------------------|
//! | `device::AttachedUsbDevice` | `b"ADEV"` |
//! | `uhci::UhciController` | `b"UHCI"` |
//! | `ehci::EhciController` | `b"EHCI"` |
//! | `xhci::XhciController` | `b"XHCI"` |
//! | `hub::UsbHubDevice` | `b"UHUB"` |
//! | `hid::UsbHidKeyboard` / `hid::UsbHidKeyboardHandle` | `b"UKBD"` |
//! | `hid::UsbHidMouse` / `hid::UsbHidMouseHandle` | `b"UMSE"` |
//! | `hid::UsbHidGamepad` / `hid::UsbHidGamepadHandle` | `b"UGPD"` |
//! | `hid::UsbHidConsumerControl` / `hid::UsbHidConsumerControlHandle` | `b"UCON"` |
//! | `hid::composite::UsbCompositeHidInput` / `hid::UsbCompositeHidInputHandle` | `b"UCMP"` |
//! | `hid::UsbHidPassthrough` / `hid::UsbHidPassthroughHandle` | `b"HIDP"` |
//! | `passthrough::UsbPassthroughDevice` | `b"USBP"` |
//! | `passthrough_device::UsbWebUsbPassthroughDevice` | `b"WUSB"` |
//!
//! `UhciController` snapshots include the full USB topology: each hub port stores an
//! [`device::AttachedUsbDevice`] (`ADEV`) snapshot, and `ADEV` snapshots embed a nested snapshot of
//! the concrete inner device model (e.g. `UHUB`, `UKBD`, `HIDP`, `WUSB`). During restore, hubs will
//! reconstruct missing device instances from these nested model snapshots so hosts do not need to
//! pre-attach devices purely to satisfy snapshot loading.
//!
//! Hosts may still choose to pre-attach devices (e.g. passthrough devices that require an external
//! handle) and the restore logic will prefer the existing device instance over reconstruction.

pub mod descriptor_fixups;
pub mod device;
pub mod ehci;
pub mod hid;
pub mod hub;
pub mod memory;
pub mod passthrough;
pub mod passthrough_device;
pub mod uhci;
pub mod usb2_port;
pub mod web;
pub mod xhci;

extern crate alloc;

pub use device::{UsbInResult, UsbOutResult};
pub use memory::MemoryBus;
pub use passthrough_device::UsbWebUsbPassthroughDevice;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;

/// USB bus speed (as seen by the root hub).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbSpeed {
    /// USB 1.1 low-speed (1.5Mbps).
    Low,
    /// USB 1.1 full-speed (12Mbps).
    Full,
    /// USB 2.0 high-speed (480Mbps).
    ///
    /// This is primarily used for passthrough/WebUSB devices and EHCI/xHCI modelling; UHCI itself
    /// is USB 1.1 and cannot operate at high speed. Higher-level hub/topology logic may still
    /// report `High` so snapshotting and cross-controller plumbing can round-trip device
    /// identity/speed.
    High,
}

/// USB control transfer SETUP packet.
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

impl RequestDirection {
    pub fn is_host_to_device(self) -> bool {
        matches!(self, RequestDirection::HostToDevice)
    }

    pub fn is_device_to_host(self) -> bool {
        matches!(self, RequestDirection::DeviceToHost)
    }
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
    /// Indicates the request completed with a timeout/CRC style error.
    ///
    /// This is primarily used by asynchronous passthrough devices to surface host-side failures as
    /// UHCI TD timeout/CRC errors (rather than a USB STALL handshake).
    Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbHubAttachError {
    NotAHub,
    InvalidPort,
    PortOccupied,
    NoDevice,
}

impl fmt::Display for UsbHubAttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsbHubAttachError::NotAHub => write!(f, "device is not a USB hub"),
            UsbHubAttachError::InvalidPort => write!(f, "invalid hub port"),
            UsbHubAttachError::PortOccupied => write!(f, "hub port is already occupied"),
            UsbHubAttachError::NoDevice => write!(f, "no device attached to hub port"),
        }
    }
}

impl core::error::Error for UsbHubAttachError {}

/// USB device model exposed behind the UHCI "root hub".
///
/// This is a high-level interface: it operates on decoded control requests and queued interrupt
/// reports, not raw USB transactions. Transaction-level details (endpoint 0 state machine, NAK
/// behaviour, etc) are handled by [`device::AttachedUsbDevice`].
pub trait UsbDeviceModel: Any {
    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

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

        let port = port.checked_sub(1).ok_or(UsbHubAttachError::InvalidPort)? as usize;
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

        let port = port.checked_sub(1).ok_or(UsbHubAttachError::InvalidPort)? as usize;
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
    /// This is used by host-side topology management (e.g. [`hub::RootHub::attach_at_path`]) to
    /// traverse nested hubs by port-number without relying on concrete downcasting.
    #[doc(hidden)]
    fn hub_port_device_mut(
        &mut self,
        port: u8,
    ) -> Result<&mut device::AttachedUsbDevice, UsbHubAttachError> {
        let Some(hub) = self.as_hub_mut() else {
            return Err(UsbHubAttachError::NotAHub);
        };

        let port = port.checked_sub(1).ok_or(UsbHubAttachError::InvalidPort)? as usize;
        if port >= hub.num_ports() {
            return Err(UsbHubAttachError::InvalidPort);
        }
        hub.downstream_device_mut(port)
            .ok_or(UsbHubAttachError::NoDevice)
    }

    /// Resets device state due to a USB bus reset (e.g. PORTSC reset).
    fn reset(&mut self) {}

    /// Abort any in-flight control transfer state.
    ///
    /// USB allows a new SETUP packet on endpoint 0 to abort the previous control transfer.
    /// Synchronous device models typically don't need to do anything here, but asynchronous
    /// models (e.g. WebUSB/libusb passthrough) may need to drop queued host actions and ignore
    /// stale completions.
    fn cancel_control_transfer(&mut self) {}

    /// Handles a USB control transfer request. For OUT requests, `data_stage` contains the
    /// payload provided by the host.
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
        match self.handle_interrupt_in(ep) {
            UsbInResult::Data(mut data) => {
                if data.len() > max_len {
                    data.truncate(max_len);
                }
                UsbInResult::Data(data)
            }
            other => other,
        }
    }

    /// Handles a non-control OUT transfer (interrupt/bulk).
    ///
    /// The default implementation delegates to [`UsbDeviceModel::handle_interrupt_out`] for
    /// backwards compatibility with interrupt-only device models.
    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        self.handle_interrupt_out(ep, data)
    }

    /// Polls an interrupt IN endpoint and returns the next queued report (if any).
    ///
    /// Returning `None` indicates the endpoint would NAK.
    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        None
    }

    /// Handles an interrupt IN transfer for a non-control endpoint.
    ///
    /// The default implementation bridges the legacy [`UsbDeviceModel::poll_interrupt_in`]
    /// interface, treating `None` as NAK. Device models that implement endpoint halt should
    /// override this to return [`UsbInResult::Stall`] when halted.
    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        match self.poll_interrupt_in(ep_addr) {
            Some(data) => UsbInResult::Data(data),
            None => UsbInResult::Nak,
        }
    }

    /// Handles interrupt OUT transfers for non-control endpoints.
    fn handle_interrupt_out(&mut self, _ep_addr: u8, _data: &[u8]) -> device::UsbOutResult {
        device::UsbOutResult::Stall
    }

    /// If this device model is a USB hub, expose it as a [`crate::hub::UsbHub`] for
    /// topology-aware routing.
    ///
    /// Hub device models should override this to return `Some(self)`.
    fn as_hub(&self) -> Option<&dyn crate::hub::UsbHub> {
        None
    }

    /// Mutable variant of [`UsbDeviceModel::as_hub`].
    fn as_hub_mut(&mut self) -> Option<&mut dyn crate::hub::UsbHub> {
        None
    }

    /// Advance device-internal timers by 1ms.
    fn tick_1ms(&mut self) {
        if let Some(hub) = self.as_hub_mut() {
            hub.tick_1ms();
        }
    }

    /// Notifies the device model that its upstream port has entered or exited suspend.
    ///
    /// This is used for HID remote-wakeup modelling: a device should only report remote wake events
    /// (via [`UsbDeviceModel::poll_remote_wakeup`]) that occur while suspended.
    fn set_suspended(&mut self, _suspended: bool) {}

    /// Poll for a remote wakeup event while the upstream port is suspended.
    ///
    /// Returning `true` indicates the device would signal a remote wakeup (resume) event. Host-side
    /// hub/port models should treat this as a *level-to-edge* conversion: once a wake has been
    /// observed, the device should return `false` until another host-visible wake event occurs.
    fn poll_remote_wakeup(&mut self) -> bool {
        false
    }

    /// If this device is a USB hub, route an address lookup to its downstream devices.
    fn child_device_mut_for_address(
        &mut self,
        address: u8,
    ) -> Option<&mut device::AttachedUsbDevice> {
        self.as_hub_mut()
            .and_then(|hub| hub.downstream_device_mut_for_address(address))
    }
}
