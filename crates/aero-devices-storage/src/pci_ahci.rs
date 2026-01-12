use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use aero_devices::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use aero_devices::pci::{profile, PciConfigSpace, PciConfigSpaceState, PciDevice};
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use memory::MemoryBus;
use memory::MmioHandler;

use crate::ahci::AhciController;
use crate::ata::AtaDrive;

/// AHCI ABAR (HBA registers) size in bytes.
pub const AHCI_ABAR_SIZE: u64 = 0x2000;

/// PCI BAR index used for AHCI ABAR on Intel ICH9.
pub const AHCI_ABAR_BAR_INDEX: u8 = 5;

const PCI_COMMAND_MEM_ENABLE: u16 = 1 << 1;

#[derive(Clone, Default)]
struct AtomicIrqLine {
    level: Arc<AtomicBool>,
}

impl AtomicIrqLine {
    fn level(&self) -> bool {
        self.level.load(Ordering::SeqCst)
    }
}

impl IrqLine for AtomicIrqLine {
    fn set_level(&self, high: bool) {
        self.level.store(high, Ordering::SeqCst);
    }
}

/// Intel ICH9 AHCI controller exposed as a PCI device.
///
/// This is a thin wrapper around [`AhciController`] that:
/// - exposes an ICH9-like PCI config space (`aero_devices::pci::profile::SATA_AHCI_ICH9`)
/// - provides BAR5 (ABAR) MMIO accessors for HBA/port registers
/// - tracks legacy INTx level for platform routing
/// - bridges DMA through [`memory::MemoryBus`]
pub struct AhciPciDevice {
    config: PciConfigSpace,
    controller: AhciController,
    irq: AtomicIrqLine,
}

impl AhciPciDevice {
    /// Creates a new AHCI PCI device with the given number of ports.
    ///
    /// Port count must be within the AHCI architectural limit (1..=32).
    pub fn new(num_ports: usize) -> Self {
        let irq = AtomicIrqLine::default();
        let controller = AhciController::new(Box::new(irq.clone()), num_ports);
        let config = profile::SATA_AHCI_ICH9.build_config_space();
        Self {
            config,
            controller,
            irq,
        }
    }

    pub fn attach_drive(&mut self, port: usize, drive: AtaDrive) {
        self.controller.attach_drive(port, drive);
    }

    pub fn detach_drive(&mut self, port: usize) {
        self.controller.detach_drive(port);
    }

    /// Reset the device back to its power-on state while preserving attached drives.
    ///
    /// This is intended for machine/platform reset flows where host-provided disk backends should
    /// remain attached across reboots.
    pub fn reset(&mut self) {
        // Mirror PCI reset semantics: clear command register state (BAR programming is preserved).
        <Self as PciDevice>::reset(self);
        self.controller.reset();
    }

    /// Returns the current level of the device's legacy INTx line.
    ///
    /// This is already gated by the PCI Command register's Interrupt Disable bit (bit 10).
    pub fn intx_level(&self) -> bool {
        // PCI command bit 10 disables legacy INTx assertion.
        let intx_disabled = (self.config.command() & (1 << 10)) != 0;
        if intx_disabled {
            return false;
        }
        self.irq.level()
    }

    /// Reads from the AHCI ABAR MMIO region.
    pub fn mmio_read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.clamp(1, 8);
        if (self.config.command() & PCI_COMMAND_MEM_ENABLE) == 0 {
            return all_ones(size);
        }
        let mut out = 0u64;

        for i in 0..size {
            let byte_off = match offset.checked_add(i as u64) {
                Some(v) => v,
                None => break,
            };
            if byte_off >= AHCI_ABAR_SIZE {
                // Unmapped bytes read as 0xFF on the platform memory bus; mirror that here.
                out |= 0xFFu64 << (i * 8);
                continue;
            }

            let word_off = byte_off & !3;
            let shift = ((byte_off & 3) * 8) as u32;
            let word = self.controller.read_u32(word_off);
            let byte = ((word >> shift) & 0xFF) as u64;
            out |= byte << (i * 8);
        }

        out
    }

    /// Writes to the AHCI ABAR MMIO region.
    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.clamp(1, 8);
        if (self.config.command() & PCI_COMMAND_MEM_ENABLE) == 0 {
            return;
        }

        let mut idx = 0usize;
        while idx < size {
            let byte_off = match offset.checked_add(idx as u64) {
                Some(v) => v,
                None => break,
            };
            if byte_off >= AHCI_ABAR_SIZE {
                idx += 1;
                continue;
            }

            let word_off = byte_off & !3;

            // Collect the bytes that fall into this 32-bit word.
            let mut be_mask = 0u32;
            let mut write_val = 0u32;
            while idx < size {
                let off = match offset.checked_add(idx as u64) {
                    Some(v) => v,
                    None => break,
                };
                if (off & !3) != word_off {
                    break;
                }
                if off >= AHCI_ABAR_SIZE {
                    idx += 1;
                    continue;
                }

                let byte_idx_in_word = (off & 3) as usize;
                let shift = (byte_idx_in_word * 8) as u32;
                let byte = ((value >> (idx * 8)) & 0xFF) as u32;
                write_val |= byte << shift;
                be_mask |= 0xFFu32 << shift;
                idx += 1;
            }

            if be_mask == 0 {
                continue;
            }

            if is_w1c_register(word_off) {
                // For W1C registers, only the written bytes should have an effect. Unwritten bytes
                // behave as zeros (no-op).
                self.controller.write_u32(word_off, write_val);
                continue;
            }

            // For regular registers, implement byte enables via read-modify-write.
            let current = self.controller.read_u32(word_off);
            let merged = (current & !be_mask) | (write_val & be_mask);
            self.controller.write_u32(word_off, merged);
        }
    }

    /// Processes pending AHCI commands (DMA) using the provided guest physical memory bus.
    ///
    /// This should be called by a platform when:
    /// - the guest sets bits in PxCI, and/or
    /// - on a periodic tick to model asynchronous device progress.
    ///
    /// DMA is only performed when PCI Bus Mastering is enabled (PCI Command bit 2).
    pub fn process(&mut self, mem: &mut dyn MemoryBus) {
        let bus_master_enabled = (self.config.command() & (1 << 2)) != 0;
        if !bus_master_enabled {
            return;
        }

        self.controller.process(mem);
    }
}

fn is_w1c_register(offset: u64) -> bool {
    // HBA.IS
    if offset == 0x08 {
        return true;
    }

    // Per-port registers: PxIS (0x10) and PxSERR (0x30).
    if offset < 0x100 {
        return false;
    }
    let port_reg_off = (offset - 0x100) % 0x80;
    matches!(port_reg_off, 0x10 | 0x30)
}

impl PciDevice for AhciPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        // Preserve BAR programming but disable decoding.
        self.config.set_command(0);

        // Reset the AHCI controller register state while preserving attached drives.
        //
        // This models a power-on (GHC.HR) reset which firmware/guests commonly use to get back to a
        // known baseline.
        self.controller.write_u32(0x04, 1);
    }
}

impl MmioHandler for AhciPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.mmio_read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.mmio_write(offset, size, value);
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

fn all_ones(size: usize) -> u64 {
    match size {
        0 => 0,
        1 => 0xff,
        2 => 0xffff,
        3 => 0x00ff_ffff,
        4 => 0xffff_ffff,
        5 => 0x0000_ffff_ffff,
        6 => 0x00ff_ffff_ffff,
        7 => 0x00ff_ffff_ffff_ffff,
        _ => u64::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mmio_read_size0_returns_zero() {
        let mut dev = AhciPciDevice::new(1);

        // Even if the PCI function is not enabled for MMIO decoding, a size-0 access is a no-op.
        assert_eq!(dev.mmio_read(0, 0), 0);
    }

    #[test]
    fn mmio_write_size0_is_noop_and_does_not_clear_w1c_or_irq() {
        let mut dev = AhciPciDevice::new(1);
        dev.config_mut().set_command(PCI_COMMAND_MEM_ENABLE);

        // Synthesize an asserted interrupt by setting PxIS + PxIE with GHC.IE enabled.
        let mut state = dev.controller.snapshot_state();
        state.hba.ghc |= 1 << 1; // GHC.IE
        state.ports[0].is = 1; // PxIS.DHRS
        state.ports[0].ie = 1; // PxIE.DHRE (enable)
        dev.controller.restore_state(&state);

        assert!(dev.intx_level(), "interrupt should be asserted before the write");

        let px_is_off = 0x100 + 0x10; // PxIS for port 0.
        let before = dev.mmio_read(px_is_off, 4);
        assert_eq!(before, 1);

        // Regression: previously `size=0` was clamped to 1, which would perform a 1-byte W1C write
        // and clear the interrupt status/level.
        dev.mmio_write(px_is_off, 0, 0xff);

        let after = dev.mmio_read(px_is_off, 4);
        assert_eq!(after, before, "size-0 MMIO write must not change PxIS");
        assert!(dev.intx_level(), "size-0 MMIO write must not change IRQ level");
    }
}
