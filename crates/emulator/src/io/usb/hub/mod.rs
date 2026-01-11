use crate::io::usb::core::AttachedUsbDevice;
use crate::io::usb::UsbDeviceModel;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbTopologyError {
    EmptyPath,
    PortOutOfRange {
        depth: usize,
        port: usize,
        num_ports: usize,
    },
    NoDeviceAtPort {
        depth: usize,
        port: usize,
    },
    NotAHub {
        depth: usize,
        port: usize,
    },
}
