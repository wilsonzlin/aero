//! USB subsystem building blocks: a minimal UHCI host controller model and basic USB HID devices.
//!
//! This crate intentionally focuses on correctness and testability over completeness. It is
//! designed to be wired into the emulator's PCI + I/O port framework later.

mod memory;
pub mod passthrough;
pub mod usb;

pub mod hid;
pub mod uhci;
pub mod web;

pub use memory::GuestMemory;
