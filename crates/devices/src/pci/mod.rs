//! PCI core types and platform topology used by Aero.

pub mod capabilities;
pub mod config;
pub mod irq_router;
pub mod msi;
pub mod profile;

mod acpi;
mod bios;
mod bus;
mod ecam;
mod platform;
mod ports;
mod resources;
mod snapshot;

pub use acpi::{build_prt_bus0, dsdt_asl, PciPrtEntry, ACPI_PCI_ROOT_NAME};
pub use bios::bios_post;
pub use bus::{PciBus, PciBusSnapshot, PciConfigMechanism1, PciMappedBar};
pub use config::{
    PciBarDefinition, PciBarKind, PciBarRange, PciCommandChange, PciConfigSpace,
    PciConfigSpaceState, PciConfigWriteEffects, PciDevice, PciSubsystemIds, PciVendorDeviceId,
};
pub use ecam::{PciEcamConfig, PciEcamMmio, PCIE_ECAM_BUS_STRIDE};
pub use irq_router::{
    GsiLevelSink, IoApicPicMirrorSink, PciIntxRouter, PciIntxRouterConfig, PicIrqLevelSink,
};
pub use msi::MsiCapability;
pub use platform::{PciHostBridge, PciIsaBridge, PciPlatform};
pub use ports::{
    register_pci_config_ports, PciConfigPort, PciConfigPorts, SharedPciConfigPorts,
    PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
pub use resources::{PciResourceAllocator, PciResourceAllocatorConfig, PciResourceError};
pub use snapshot::PciCoreSnapshot;

/// PCI bus/device/function identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PciBdf {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciBdf {
    /// Creates a new BDF.
    ///
    /// The caller is responsible for ensuring the values are within the PCI ranges:
    /// bus < 256, device < 32, function < 8.
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self {
            bus,
            device,
            function,
        }
    }
}

impl core::cmp::Ord for PciBdf {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        (self.bus, self.device, self.function).cmp(&(other.bus, other.device, other.function))
    }
}

impl core::cmp::PartialOrd for PciBdf {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// PCI INTx interrupt pin.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PciInterruptPin {
    IntA,
    IntB,
    IntC,
    IntD,
}

impl PciInterruptPin {
    pub const fn index(self) -> usize {
        match self {
            Self::IntA => 0,
            Self::IntB => 1,
            Self::IntC => 2,
            Self::IntD => 3,
        }
    }

    /// Converts to the PCI config-space encoding (1 = INTA#, 2 = INTB#, ...).
    pub const fn to_config_u8(self) -> u8 {
        self.index() as u8 + 1
    }

    pub const fn from_config_u8(val: u8) -> Option<Self> {
        match val {
            1 => Some(Self::IntA),
            2 => Some(Self::IntB),
            3 => Some(Self::IntC),
            4 => Some(Self::IntD),
            _ => None,
        }
    }
}
