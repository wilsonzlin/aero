//! Shared guest-visible USB topology constants for the browser/WASM runtime.
//!
//! The web runtime (UHCI/EHCI/xHCI) reserves some root hub ports for fixed topology:
//! - Root port 0: external hub / topology manager (synthetic HID devices, WebHID passthrough, etc.)
//! - Root port 1: WebUSB passthrough (`UsbWebUsbPassthroughDevice`)
//!
//! Keeping these assignments consistent across controller bridges avoids topology conflicts.

/// Guest-visible root hub port reserved for WebUSB passthrough.
///
/// Root ports are 0-based in the host-managed topology contract.
pub(crate) const WEBUSB_ROOT_PORT: u8 = 1;
