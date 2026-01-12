use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::AtapiCdrom;

/// Configuration for the canonical Windows 7 storage topology.
///
/// This is intentionally small: the topology itself (controllers, BDFs, IRQ wiring) is fixed, and
/// only the attached media is configurable.
pub struct Windows7StorageTopologyConfig {
    /// Primary hard disk attached to the AHCI controller (port 0).
    pub hdd: AtaDrive,
    /// ATAPI CD-ROM attached to the PIIX3 IDE controller (secondary master).
    pub cdrom: AtapiCdrom,
}
