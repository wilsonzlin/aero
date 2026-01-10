use crate::pci::config::PciConfigSpace;
use crate::pci::{PciBdf, PciBus, PciDevice};

/// Intel Q35 + ICH9 IDs are widely recognized by Windows 7 inbox drivers and are the default
/// machine type used by QEMU for modern guests.
///
/// We only model the minimal "platform glue" devices needed for PCI enumeration to look
/// plausible:
/// - PCI host bridge at 00:00.0 (class 0x0600)
/// - ISA/LPC bridge at 00:1f.0 (class 0x0601)
pub const INTEL_VENDOR_ID: u16 = 0x8086;

pub const Q35_HOST_BRIDGE_DEVICE_ID: u16 = 0x29c0;
pub const ICH9_LPC_DEVICE_ID: u16 = 0x2918;

pub struct PciHostBridge {
    config: PciConfigSpace,
}

impl PciHostBridge {
    pub fn new() -> Self {
        let mut config = PciConfigSpace::new(INTEL_VENDOR_ID, Q35_HOST_BRIDGE_DEVICE_ID);
        config.set_class_code(0x06, 0x00, 0x00, 0x00);
        Self { config }
    }
}

impl PciDevice for PciHostBridge {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

pub struct PciIsaBridge {
    config: PciConfigSpace,
}

impl PciIsaBridge {
    pub fn new() -> Self {
        let mut config = PciConfigSpace::new(INTEL_VENDOR_ID, ICH9_LPC_DEVICE_ID);
        config.set_class_code(0x06, 0x01, 0x00, 0x00);
        Self { config }
    }
}

impl PciDevice for PciIsaBridge {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

/// Convenience constructor for the built-in PCI platform topology.
#[derive(Debug, Default)]
pub struct PciPlatform;

impl PciPlatform {
    pub fn build_bus() -> PciBus {
        let mut bus = PciBus::new();
        bus.add_device(PciBdf::new(0, 0, 0), Box::new(PciHostBridge::new()));
        bus.add_device(PciBdf::new(0, 0x1f, 0), Box::new(PciIsaBridge::new()));
        bus
    }
}
