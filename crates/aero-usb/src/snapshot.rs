//! Internal helpers for snapshotting USB device trees.
//!
//! This module provides the "glue" needed to snapshot/restore USB topologies that are stored as
//! `Box<dyn UsbDevice>` trait objects. Concrete device models implement `IoSnapshot`, and these
//! helpers downcast to the supported device types to call their snapshot methods.

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotResult};

use crate::hid::passthrough::{SharedUsbHidPassthroughDevice, UsbHidPassthrough};
use crate::hid::{UsbHidCompositeInput, UsbHidGamepad, UsbHidKeyboard, UsbHidMouse};
use crate::hub::UsbHubDevice;
use crate::passthrough_device::{SharedUsbWebUsbPassthroughDevice, UsbWebUsbPassthroughDevice};
use crate::usb::UsbDevice;

pub(crate) fn save_device_state(dev: &dyn UsbDevice) -> Vec<u8> {
    if let Some(dev) = dev.as_any().downcast_ref::<UsbHubDevice>() {
        return dev.save_state();
    }
    if let Some(dev) = dev.as_any().downcast_ref::<UsbHidKeyboard>() {
        return dev.save_state();
    }
    if let Some(dev) = dev.as_any().downcast_ref::<UsbHidMouse>() {
        return dev.save_state();
    }
    if let Some(dev) = dev.as_any().downcast_ref::<UsbHidGamepad>() {
        return dev.save_state();
    }
    if let Some(dev) = dev.as_any().downcast_ref::<UsbHidCompositeInput>() {
        return dev.save_state();
    }
    if let Some(dev) = dev.as_any().downcast_ref::<UsbHidPassthrough>() {
        return dev.save_state();
    }
    if let Some(dev) = dev.as_any().downcast_ref::<SharedUsbHidPassthroughDevice>() {
        return dev.save_state();
    }
    if let Some(dev) = dev.as_any().downcast_ref::<UsbWebUsbPassthroughDevice>() {
        return dev.save_state();
    }
    if let Some(dev) = dev
        .as_any()
        .downcast_ref::<SharedUsbWebUsbPassthroughDevice>()
    {
        return dev.save_state();
    }

    // Avoid panicking if a caller wires an unsupported `UsbDevice` implementation into the
    // topology. Callers should treat an empty snapshot as "state omitted".
    Vec::new()
}

pub(crate) fn load_device_state(dev: &mut dyn UsbDevice, bytes: &[u8]) -> SnapshotResult<()> {
    if let Some(dev) = dev.as_any_mut().downcast_mut::<UsbHubDevice>() {
        return dev.load_state(bytes);
    }
    if let Some(dev) = dev.as_any_mut().downcast_mut::<UsbHidKeyboard>() {
        return dev.load_state(bytes);
    }
    if let Some(dev) = dev.as_any_mut().downcast_mut::<UsbHidMouse>() {
        return dev.load_state(bytes);
    }
    if let Some(dev) = dev.as_any_mut().downcast_mut::<UsbHidGamepad>() {
        return dev.load_state(bytes);
    }
    if let Some(dev) = dev.as_any_mut().downcast_mut::<UsbHidCompositeInput>() {
        return dev.load_state(bytes);
    }
    if let Some(dev) = dev.as_any_mut().downcast_mut::<UsbHidPassthrough>() {
        return dev.load_state(bytes);
    }
    if let Some(dev) = dev
        .as_any_mut()
        .downcast_mut::<SharedUsbHidPassthroughDevice>()
    {
        return dev.load_state(bytes);
    }
    if let Some(dev) = dev
        .as_any_mut()
        .downcast_mut::<UsbWebUsbPassthroughDevice>()
    {
        return dev.load_state(bytes);
    }
    if let Some(dev) = dev
        .as_any_mut()
        .downcast_mut::<SharedUsbWebUsbPassthroughDevice>()
    {
        return dev.load_state(bytes);
    }

    Err(SnapshotError::InvalidFieldEncoding(
        "unsupported USB device type",
    ))
}
