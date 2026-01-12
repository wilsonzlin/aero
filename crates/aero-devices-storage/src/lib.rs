//! Storage controller devices.
//!
//! This crate intentionally models the *device-side* behaviour of:
//! - Legacy IDE (ATA PIO) via I/O ports (`0x1F0/0x3F6`, `0x170/0x376`)
//! - AHCI (SATA) via HBA memory registers and command list DMA
//!
//! The goal is to provide enough fidelity for early boot (e.g. FreeDOS via IDE)
//! and Windows 7 boot (AHCI via `msahci.sys`) when wired into a full emulator.

pub mod ahci;
pub mod ide;

pub mod ata;
pub mod bus;
pub mod pci_ahci;

pub use aero_devices::irq::IrqLine;
pub use pci_ahci::AhciPciDevice;

pub use memory::MemoryBus;

/// PCI PIIX3-compatible IDE controller with ATA + ATAPI + Bus Master DMA.
pub mod pci_ide;

/// Bus Master IDE (BMIDE) DMA engine used by [`pci_ide`].
pub mod busmaster;

/// ATAPI CD-ROM (packet device) used by [`pci_ide`].
pub mod atapi;
