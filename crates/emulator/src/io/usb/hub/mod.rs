use crate::io::usb::core::AttachedUsbDevice;
use crate::io::usb::UsbDeviceModel;
use thiserror::Error;

mod root;
mod device;

pub use root::RootHub;
pub use device::UsbHubDevice;

/// Object-safe traversal interface for USB hubs.
///
/// External hub device models implement this trait and expose it via
/// [`UsbDeviceModel::as_hub`] / [`UsbDeviceModel::as_hub_mut`]. The UHCI schedule walker then
/// resolves device addresses by recursively walking through hub topology.
pub trait UsbHub {
    /// Advances hub internal time by 1ms.
    ///
    /// Hub implementations should update any pending port reset timers and recurse into nested hubs
    /// so time-based events propagate down the topology.
    fn tick_1ms(&mut self);

    /// Returns a mutable reference to a reachable downstream device with the given USB address.
    ///
    /// Implementations should only consider devices behind ports that are connected and enabled
    /// (and powered, if modelled).
    fn downstream_device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice>;

    /// Returns the device currently attached to `port`, if any.
    ///
    /// This accessor is used by topology configuration helpers (e.g. attaching devices behind
    /// nested hubs) and does not need to apply reachability rules.
    fn downstream_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice>;

    /// Attaches a new device model to the given downstream port.
    fn attach_downstream(&mut self, port: usize, model: Box<dyn UsbDeviceModel>);

    /// Detaches the device (if any) from the given downstream port.
    fn detach_downstream(&mut self, port: usize);

    /// Number of downstream ports on this hub.
    fn num_ports(&self) -> usize;
}

/// Errors produced when configuring or traversing the USB hub topology.
///
/// For path-based APIs (e.g. [`RootHub::attach_at_path`]), `depth` refers to the index within the
/// provided path slice where the failure occurred.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UsbTopologyError {
    #[error("USB topology path is empty")]
    EmptyPath,
    #[error("invalid port {port} at depth {depth} (hub has {num_ports} ports)")]
    PortOutOfRange {
        depth: usize,
        port: usize,
        num_ports: usize,
    },
    #[error("port {port} at depth {depth} is already occupied")]
    PortOccupied { depth: usize, port: usize },
    #[error("no device attached at port {port} (depth {depth})")]
    NoDeviceAtPort { depth: usize, port: usize },
    #[error("cannot traverse port {port} at depth {depth}: device is not a USB hub")]
    NotAHub { depth: usize, port: usize },
}

impl UsbTopologyError {
    fn with_depth(self, depth: usize) -> Self {
        match self {
            UsbTopologyError::EmptyPath => UsbTopologyError::EmptyPath,
            UsbTopologyError::PortOutOfRange {
                port,
                num_ports,
                ..
            } => UsbTopologyError::PortOutOfRange {
                depth,
                port,
                num_ports,
            },
            UsbTopologyError::PortOccupied { port, .. } => UsbTopologyError::PortOccupied { depth, port },
            UsbTopologyError::NoDeviceAtPort { port, .. } => UsbTopologyError::NoDeviceAtPort { depth, port },
            UsbTopologyError::NotAHub { port, .. } => UsbTopologyError::NotAHub { depth, port },
        }
    }
}

#[cfg(test)]
mod reset_tests;
