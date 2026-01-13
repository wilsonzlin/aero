//! Legacy IDE (PIIX3) storage controller.
//!
//! The canonical IDE/ATAPI implementation now lives in `aero-devices-storage`. This module keeps
//! the `emulator::io::storage::ide` import path stable while avoiding compiling the legacy IDE
//! implementation inside `crates/emulator`.

pub use aero_devices_storage::ata::AtaDrive;
pub use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
pub use aero_devices_storage::busmaster::{BusMasterChannel, PrdEntry};
pub use aero_devices_storage::pci_ide::{IdeController as IdeCoreController, IdePortMap, PRIMARY_PORTS, SECONDARY_PORTS};
pub use aero_devices_storage::pci_ide::{Piix3IdePciDevice as IdeController};

