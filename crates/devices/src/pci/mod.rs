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

    /// Packs this BDF into a compact `u16` key using the standard PCI config-address bit layout.
    ///
    /// Layout (LSB..MSB):
    /// - bits 0..=2: function (0-7)
    /// - bits 3..=7: device (0-31)
    /// - bits 8..=15: bus (0-255)
    ///
    /// This matches the BDF portion of PCI config mechanism #1 (`0xCF8`) after shifting right by 8:
    /// `packed = (cfg_addr >> 8) & 0xFFFF`.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `device >= 32` or `function >= 8`.
    pub const fn pack_u16(self) -> u16 {
        debug_assert!(self.device < 32);
        debug_assert!(self.function < 8);
        ((self.bus as u16) << 8) | ((self.device as u16) << 3) | (self.function as u16)
    }

    /// Unpacks a `u16` produced by [`PciBdf::pack_u16`] back into a [`PciBdf`].
    pub const fn unpack_u16(v: u16) -> Self {
        let bus = (v >> 8) as u8;
        let device = ((v >> 3) & 0x1f) as u8;
        let function = (v & 0x7) as u8;

        // These should always hold due to the masks above, but keep them as debug assertions to
        // document the intended ranges.
        debug_assert!(device < 32);
        debug_assert!(function < 8);

        Self {
            bus,
            device,
            function,
        }
    }
}

impl From<PciBdf> for u16 {
    fn from(value: PciBdf) -> Self {
        value.pack_u16()
    }
}

impl From<u16> for PciBdf {
    fn from(value: u16) -> Self {
        Self::unpack_u16(value)
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
