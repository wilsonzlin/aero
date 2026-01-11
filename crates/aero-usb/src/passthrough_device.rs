//! WebUSB passthrough USB device (UHCI-visible).
//!
//! The implementation lives in [`crate::passthrough`] alongside the host-action wire types and the
//! request-level [`crate::passthrough::UsbPassthroughDevice`] state machine. This module exists as
//! a dedicated export location for the `UsbDevice` adapter.

pub use crate::passthrough::UsbWebUsbPassthroughDevice;
