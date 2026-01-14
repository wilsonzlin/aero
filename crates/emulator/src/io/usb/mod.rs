//! Emulator USB integration.
//!
//! The canonical USB implementation lives in `crates/aero-usb`
//! (see `docs/adr/0015-canonical-usb-stack.md`). This module keeps the emulator's
//! `crate::io::usb` path stable by re-exporting the shared implementation and
//! providing thin integration glue (PCI + PortIO wiring, descriptor fixups, etc).
pub mod descriptor_fixups;
#[cfg(feature = "legacy-usb-ehci")]
pub mod ehci;
pub mod passthrough;
pub mod uhci;
#[cfg(feature = "legacy-usb-xhci")]
pub mod xhci;

pub use aero_usb::device as core;
pub use aero_usb::hid;
pub use aero_usb::hub;
#[cfg(feature = "legacy-usb-xhci")]
pub use aero_usb::xhci::XhciController;

pub use aero_usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
    UsbHubAttachError, UsbInResult, UsbOutResult, UsbSpeed,
};
