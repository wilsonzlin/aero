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
pub const WEBUSB_ROOT_PORT: u8 = 1;

// Keep the reserved root port consistent across the Rust machine implementation (`aero-machine`)
// and the WASM bridges (`aero-wasm`). This is a hard contract for browser snapshots and USB
// topology managers.
const _: () = assert!(WEBUSB_ROOT_PORT == aero_machine::Machine::UHCI_WEBUSB_ROOT_PORT);

/// Maximum number of WebUSB host actions to drain from WASM per JS poll.
///
/// Draining actions produces a JS array (`UsbHostAction[]`). Keep this bounded so that a bug or a
/// malicious guest can't force the runtime to allocate an unbounded array in one tick.
pub(crate) const MAX_WEBUSB_HOST_ACTIONS_PER_DRAIN: usize = 64;
