//! USB passthrough contract/state machine.
//!
//! The canonical implementation lives in `crates/aero-usb` (`aero_usb::passthrough`). This module
//! exists to keep the emulator's `crate::io::usb::passthrough` import path stable.

pub use aero_usb::passthrough::*;
