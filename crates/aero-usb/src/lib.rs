//! USB subsystem building blocks: a minimal UHCI host controller model and basic USB HID devices.
//!
//! This crate is the canonical USB/UHCI implementation for Aero's browser/WASM runtime
//! (see `docs/adr/0015-canonical-usb-stack.md`).
//!
//! This crate intentionally focuses on correctness and testability over completeness. It is
//! designed to be wired into the emulator's PCI + I/O port framework later.
//!
//! ## Snapshot/restore
//!
//! Several models implement `aero_io_snapshot::io::state::IoSnapshot` using the canonical
//! `aero-io-snapshot` TLV encoding so they can participate in VM save/restore:
//!
//! | Type | `IoSnapshot::DEVICE_ID` |
//! |------|------------------------|
//! | `uhci::UhciController` | `b"UHCI"` |
//! | `hub::UsbHubDevice` | `b"UHUB"` |
//! | `hid::passthrough::UsbHidPassthrough` | `b"HIDP"` |
//! | `passthrough::UsbPassthroughDevice` | `b"USBP"` |
//! | `passthrough_device::UsbWebUsbPassthroughDevice` | `b"WUSB"` |

mod memory;
pub mod passthrough;
pub mod passthrough_device;
pub mod usb;

pub mod hid;
pub mod hub;
pub mod uhci;
pub mod web;

pub use memory::GuestMemory;
pub use passthrough_device::UsbWebUsbPassthroughDevice;
