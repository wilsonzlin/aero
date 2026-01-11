//! USB 1.1 external hub device model (USB class 0x09).
//!
//! The UHCI root hub only exposes two ports. Real-world topologies frequently place multiple
//! devices behind an external hub. This module provides an external hub device model and a small
//! traversal trait used by [`crate::usb::UsbBus`] for topology-aware address routing.

use crate::usb::UsbDevice;

mod device;

pub use device::UsbHubDevice;

/// Object-safe traversal interface for USB hubs.
///
/// External hub devices implement this trait and expose it via
/// [`crate::usb::UsbDevice::as_hub`] / [`crate::usb::UsbDevice::as_hub_mut`]. The USB bus then
/// resolves device addresses by recursively walking the hub topology.
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
    fn downstream_device_mut_for_address(&mut self, address: u8) -> Option<&mut dyn UsbDevice>;

    /// Returns the device currently attached to `port`, if any.
    ///
    /// This accessor is used by topology configuration helpers (e.g. attaching devices behind
    /// nested hubs) and does not need to apply reachability rules.
    fn downstream_device(&self, port: usize) -> Option<&dyn UsbDevice>;

    /// Returns the device currently attached to `port`, if any.
    ///
    /// This accessor is used by topology configuration helpers (e.g. attaching devices behind
    /// nested hubs) and does not need to apply reachability rules.
    fn downstream_device_mut(&mut self, port: usize) -> Option<&mut dyn UsbDevice>;

    /// Attaches a new device to the given downstream port.
    fn attach_downstream(&mut self, port: usize, device: Box<dyn UsbDevice>);

    /// Detaches the device (if any) from the given downstream port.
    fn detach_downstream(&mut self, port: usize);

    /// Number of downstream ports on this hub.
    fn num_ports(&self) -> usize;
}
