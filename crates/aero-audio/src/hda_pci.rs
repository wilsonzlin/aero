use crate::hda::{HdaController, HDA_MMIO_SIZE};

use aero_devices::pci::profile::HDA_ICH6;
use aero_devices::pci::{PciBarDefinition, PciConfigSpace, PciDevice};
use memory::MmioHandler;

/// Canonical PCI function wrapper for Aero's Intel HD Audio controller model.
///
/// This type bridges [`HdaController`] into Aero's:
/// - PCI config-space + BAR allocation framework (`aero_devices::pci`)
/// - guest physical MMIO bus (`memory::MmioHandler`)
pub struct HdaPciDevice {
    controller: HdaController,
    config: PciConfigSpace,
}

impl HdaPciDevice {
    /// BAR0 MMIO size for ICH6-style HDA controllers.
    pub const MMIO_BAR_SIZE: u32 = HDA_MMIO_SIZE as u32;

    pub fn new() -> Self {
        let mut config = HDA_ICH6.build_config_space();
        config.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: Self::MMIO_BAR_SIZE,
                prefetchable: false,
            },
        );

        Self {
            controller: HdaController::new(),
            config,
        }
    }

    pub fn controller(&self) -> &HdaController {
        &self.controller
    }

    pub fn controller_mut(&mut self) -> &mut HdaController {
        &mut self.controller
    }

    /// Current asserted level of the device's INTx IRQ line.
    pub fn irq_level(&self) -> bool {
        self.controller.irq_level()
    }
}

impl Default for HdaPciDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl PciDevice for HdaPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

impl MmioHandler for HdaPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.controller.mmio_read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.controller.mmio_write(offset, size, value);
    }
}
