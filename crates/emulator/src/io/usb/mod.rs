//! Emulator USB integration.
//!
//! The canonical USB/UHCI implementation lives in `crates/aero-usb`
//! (see `docs/adr/0015-canonical-usb-stack.md`). This module keeps the emulator's
//! `crate::io::usb` path stable by re-exporting the shared implementation and
//! providing thin integration glue (PCI + PortIO wiring, descriptor fixups, etc).
pub mod descriptor_fixups;
pub mod passthrough;
pub mod uhci;

pub use aero_usb::device as core;
pub use aero_usb::hid;
pub use aero_usb::hub;

pub use aero_usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
    UsbHubAttachError, UsbInResult, UsbOutResult, UsbSpeed,
};
