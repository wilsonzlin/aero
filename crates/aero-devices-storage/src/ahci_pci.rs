//! PCI function wrapper for the AHCI controller model.

use crate::ahci::AhciController;
use crate::bus::IrqLine;

use aero_devices::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use aero_devices::pci::profile::SATA_AHCI_ICH9;
use aero_devices::pci::{PciBarDefinition, PciConfigSpace, PciConfigSpaceState, PciDevice};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter};
use memory::MmioHandler;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Default)]
struct LevelIrqLine(Rc<Cell<bool>>);

impl LevelIrqLine {
    fn new() -> Self {
        Self(Rc::new(Cell::new(false)))
    }

    fn level(&self) -> bool {
        self.0.get()
    }
}

impl IrqLine for LevelIrqLine {
    fn set_level(&self, high: bool) {
        self.0.set(high);
    }
}

/// Canonical PCI function wrapper for the AHCI controller model.
///
/// This type bridges [`AhciController`] into Aero's:
/// - PCI config-space + BAR allocation framework (`aero_devices::pci`)
/// - guest physical MMIO bus (`memory::MmioHandler`)
///
/// Snapshotting
/// -----------
/// `load_state()` restores the device-internal level of the INTx line, but it does *not*
/// automatically propagate that to the platform interrupt controller.
///
/// If the machine uses [`aero_devices::pci::PciIntxRouter`] to route INTx pins into GSIs, callers
/// must re-drive the routed GSI levels after restoring device state. This can be done either by:
/// - re-polling each device's `irq_level()` and calling `PciIntxRouter::set_intx_level`, or
/// - restoring the router state and calling `PciIntxRouter::sync_levels_to_sink()`.
pub struct AhciPciDevice {
    controller: AhciController,
    config: PciConfigSpace,
    irq_line: LevelIrqLine,
}

impl AhciPciDevice {
    /// BAR5 MMIO size for ICH9-style AHCI controllers.
    pub const MMIO_BAR_SIZE: u32 = 0x2000;

    pub fn new(num_ports: usize) -> Self {
        let irq_line = LevelIrqLine::new();

        let mut config = SATA_AHCI_ICH9.build_config_space();
        config.set_bar_definition(
            5,
            PciBarDefinition::Mmio32 {
                size: Self::MMIO_BAR_SIZE,
                prefetchable: false,
            },
        );

        Self {
            controller: AhciController::new(Box::new(irq_line.clone()), num_ports),
            config,
            irq_line,
        }
    }

    pub fn controller(&self) -> &AhciController {
        &self.controller
    }

    pub fn controller_mut(&mut self) -> &mut AhciController {
        &mut self.controller
    }

    /// Current asserted level of the device's legacy INTx IRQ line.
    pub fn irq_level(&self) -> bool {
        self.irq_line.level()
    }
}

impl Default for AhciPciDevice {
    fn default() -> Self {
        Self::new(1)
    }
}

impl PciDevice for AhciPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

impl MmioHandler for AhciPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        match size {
            1 | 2 | 4 => {
                let aligned = offset & !3;
                let shift = ((offset & 3) * 8) as u32;
                let value = self.controller.read_u32(aligned) as u64;
                let mask = match size {
                    1 => 0xffu64,
                    2 => 0xffffu64,
                    4 => 0xffff_ffffu64,
                    _ => unreachable!(),
                };
                (value >> shift) & mask
            }
            8 => {
                let lo = self.read(offset, 4);
                let hi = self.read(offset + 4, 4);
                lo | (hi << 32)
            }
            _ => {
                let mut out = 0u64;
                for i in 0..size.min(8) {
                    out |= (self.read(offset + i as u64, 1) & 0xff) << (i * 8);
                }
                out
            }
        }
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        match size {
            1 | 2 | 4 => {
                let aligned = offset & !3;
                let shift = ((offset & 3) * 8) as u32;
                let mask = match size {
                    1 => 0xffu32,
                    2 => 0xffffu32,
                    4 => 0xffff_ffffu32,
                    _ => unreachable!(),
                };
                let value32 = ((value as u32) & mask) << shift;

                // Merge sub-dword writes with the existing dword value.
                let new_val = if size == 4 {
                    value32
                } else {
                    let cur = self.controller.read_u32(aligned);
                    let mask_shifted = mask << shift;
                    (cur & !mask_shifted) | value32
                };

                self.controller.write_u32(aligned, new_val);
            }
            8 => {
                self.write(offset, 4, value as u32 as u64);
                self.write(offset + 4, 4, ((value >> 32) as u32) as u64);
            }
            _ => {
                let bytes = value.to_le_bytes();
                for (i, &b) in bytes.iter().take(size.min(8)).enumerate() {
                    self.write(offset + i as u64, 1, u64::from(b));
                }
            }
        }
    }
}

impl IoSnapshot for AhciPciDevice {
    const DEVICE_ID: [u8; 4] = *b"AHCP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let pci = self.config.snapshot_state();
        let mut pci_enc = Encoder::new().bytes(&pci.bytes);
        for i in 0..6 {
            pci_enc = pci_enc.u64(pci.bar_base[i]).bool(pci.bar_probe[i]);
        }
        w.field_bytes(TAG_PCI, pci_enc.finish());

        w.field_bytes(TAG_CONTROLLER, self.controller.save_state());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI: u16 = 1;
        const TAG_CONTROLLER: u16 = 2;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_PCI) {
            let mut d = Decoder::new(buf);
            let mut config_bytes = [0u8; PCI_CONFIG_SPACE_SIZE];
            config_bytes.copy_from_slice(d.bytes(PCI_CONFIG_SPACE_SIZE)?);

            let mut bar_base = [0u64; 6];
            let mut bar_probe = [false; 6];
            for i in 0..6 {
                bar_base[i] = d.u64()?;
                bar_probe[i] = d.bool()?;
            }
            d.finish()?;

            self.config.restore_state(&PciConfigSpaceState {
                bytes: config_bytes,
                bar_base,
                bar_probe,
            });
        }

        let Some(buf) = r.bytes(TAG_CONTROLLER) else {
            return Err(SnapshotError::InvalidFieldEncoding(
                "missing ahci controller state",
            ));
        };
        self.controller.load_state(buf)?;

        Ok(())
    }
}

