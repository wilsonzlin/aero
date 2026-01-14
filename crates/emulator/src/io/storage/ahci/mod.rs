//! Emulator compatibility wrapper for the canonical AHCI device model.
//!
//! The emulator historically carried a bespoke AHCI implementation. This module keeps the
//! emulator-facing API stable (`AhciController`, `AhciPciDevice`, and the `registers` constants)
//! while delegating behaviour to `aero-devices-storage`.
//!
//! # Snapshot semantics
//!
//! This controller implements [`aero_io_snapshot::io::state::IoSnapshot`] using the canonical AHCI
//! snapshot schema (`AhciControllerState` in `aero-io-snapshot`) via the underlying
//! `aero-devices-storage` controller.
//!
//! The snapshot only contains guest-visible MMIO register state. The host disk backend is **not**
//! snapshotted; restoring a snapshot intentionally drops any currently attached drive. The
//! platform must call [`AhciController::attach_disk`] after restore (before resuming the guest) if
//! it expects AHCI DMA to function.

pub mod registers;

use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};
use crate::io::storage::adapters::VirtualDiskFromEmuDiskBackend;
use crate::io::storage::disk::{DiskBackend, DiskResult};

use aero_devices::irq::IrqLine;
use aero_devices::pci::profile;
use aero_devices_storage::ata::AtaDrive;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotResult, SnapshotVersion};

use memory::MemoryBus;

use std::cell::Cell;
use std::rc::Rc;

const AHCI_ABAR_CFG_OFFSET: u16 = profile::AHCI_ABAR_CFG_OFFSET as u16;

const PCI_COMMAND_MEM_ENABLE: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER_ENABLE: u16 = 1 << 2;
const PCI_COMMAND_INTX_DISABLE: u16 = 1 << 10;

#[derive(Clone, Default)]
struct RecordingIrqLine {
    level: Rc<Cell<bool>>,
}

impl RecordingIrqLine {
    fn level(&self) -> bool {
        self.level.get()
    }
}

impl IrqLine for RecordingIrqLine {
    fn set_level(&self, high: bool) {
        self.level.set(high);
    }
}

struct DynDiskBackend(Box<dyn DiskBackend>);

impl DiskBackend for DynDiskBackend {
    fn sector_size(&self) -> u32 {
        self.0.sector_size()
    }

    fn total_sectors(&self) -> u64 {
        self.0.total_sectors()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.0.read_sectors(lba, buf)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.0.write_sectors(lba, buf)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.0.flush()
    }
}

fn is_w1c_register(offset: u64) -> bool {
    if offset == registers::HBA_IS {
        return true;
    }

    if offset < registers::HBA_PORTS_BASE {
        return false;
    }

    let port_reg_off = (offset - registers::HBA_PORTS_BASE) % registers::HBA_PORT_STRIDE;
    matches!(port_reg_off, registers::PX_IS | registers::PX_SERR)
}

/// Emulator-facing AHCI controller.
///
/// This is a thin wrapper around [`aero_devices_storage::ahci::AhciController`] with additional
/// compatibility behaviour:
/// - Adapts the emulator [`DiskBackend`] trait into an [`aero_storage::VirtualDisk`].
/// - Records IRQ level for `irq_level()`.
/// - Executes pending command list entries synchronously on MMIO access, so writing PxCI causes
///   DMA to run without requiring an explicit external `process()` call (matching legacy behavior
///   relied on by benches).
pub struct AhciController {
    inner: aero_devices_storage::ahci::AhciController,
    irq: RecordingIrqLine,
    /// When false, MMIO accesses do not call `inner.process(mem)`.
    ///
    /// This is used by the PCI wrapper to gate DMA on PCI COMMAND.BME.
    process_on_mmio: bool,
}

impl AhciController {
    pub const ABAR_SIZE: u64 = profile::AHCI_ABAR_SIZE;

    pub fn new(disk: Box<dyn DiskBackend>) -> Self {
        let irq = RecordingIrqLine::default();
        let mut inner = aero_devices_storage::ahci::AhciController::new(Box::new(irq.clone()), 1);

        let vdisk = VirtualDiskFromEmuDiskBackend::new(DynDiskBackend(disk));
        let drive = AtaDrive::new(Box::new(vdisk))
            .expect("failed to construct ATA drive from emulator disk backend");
        inner.attach_drive(0, drive);

        Self {
            inner,
            irq,
            process_on_mmio: true,
        }
    }

    /// Attach (or replace) the host disk backend.
    ///
    /// Snapshot restore drops the currently attached drive; the platform must call this before
    /// resuming the guest if it expects AHCI DMA to function.
    pub fn attach_disk(&mut self, disk: Box<dyn DiskBackend>) {
        let vdisk = VirtualDiskFromEmuDiskBackend::new(DynDiskBackend(disk));
        let drive = AtaDrive::new(Box::new(vdisk))
            .expect("failed to construct ATA drive from emulator disk backend");
        self.inner.attach_drive(0, drive);
    }

    pub fn irq_level(&self) -> bool {
        self.irq.level()
    }

    /// Convenience wrapper around the [`MmioDevice`] trait methods.
    pub fn mmio_read_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64) -> u32 {
        self.mmio_read(mem, offset, 4)
    }

    /// Convenience wrapper around the [`MmioDevice`] trait methods.
    pub fn mmio_write_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        self.mmio_write(mem, offset, 4, value);
    }

    fn maybe_process(&mut self, mem: &mut dyn MemoryBus) {
        if self.process_on_mmio {
            self.inner.process(mem);
        }
    }

    fn with_process_on_mmio<R>(&mut self, enabled: bool, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.process_on_mmio;
        self.process_on_mmio = enabled;
        let out = f(self);
        self.process_on_mmio = prev;
        out
    }

    fn mmio_read_no_process(&mut self, offset: u64, size: usize) -> u32 {
        if size == 0 {
            return 0;
        }
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }

        let mut out = 0u32;
        for i in 0..size {
            let byte_off = match offset.checked_add(i as u64) {
                Some(v) => v,
                None => break,
            };

            let word_off = byte_off & !3;
            let shift = ((byte_off & 3) * 8) as u32;
            let word = self.inner.read_u32(word_off);
            let byte = (word >> shift) & 0xFF;
            out |= byte << (i * 8);
        }

        out
    }

    fn mmio_write_no_process(&mut self, offset: u64, size: usize, value: u32) {
        if size == 0 {
            return;
        }
        if !matches!(size, 1 | 2 | 4) {
            return;
        }

        let mut idx = 0usize;
        while idx < size {
            let byte_off = match offset.checked_add(idx as u64) {
                Some(v) => v,
                None => break,
            };
            let word_off = byte_off & !3;

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

                let shift = ((off & 3) * 8) as u32;
                let byte = (value >> (idx * 8)) & 0xFF;
                write_val |= byte << shift;
                be_mask |= 0xFFu32 << shift;
                idx += 1;
            }

            if be_mask == 0 {
                continue;
            }

            if is_w1c_register(word_off) {
                // For W1C registers, only written bytes should have an effect. Treat unwritten
                // bytes as zeros (no-op).
                self.inner.write_u32(word_off, write_val);
                continue;
            }

            // Regular registers: honour byte enables via read-modify-write.
            let current = self.inner.read_u32(word_off);
            let merged = (current & !be_mask) | (write_val & be_mask);
            self.inner.write_u32(word_off, merged);
        }
    }
}

impl IoSnapshot for AhciController {
    // Match the canonical controller's device id/version so snapshots remain interoperable.
    const DEVICE_ID: [u8; 4] =
        <aero_devices_storage::ahci::AhciController as IoSnapshot>::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion =
        <aero_devices_storage::ahci::AhciController as IoSnapshot>::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.inner.save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        // Note: `aero-devices-storage` AHCI restore drops attached drives; the platform must
        // re-attach disks post-restore.
        self.inner.load_state(bytes)?;
        Ok(())
    }
}

impl MmioDevice for AhciController {
    fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        // Legacy behaviour: allow synchronous progress on any MMIO access.
        self.maybe_process(mem);
        self.mmio_read_no_process(offset, size)
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        self.mmio_write_no_process(offset, size, value);
        // PxCI writes (and other state changes) should be observed without an explicit poll.
        self.maybe_process(mem);
    }
}

/// A PCI wrapper that exposes the AHCI controller as a class-code compatible SATA/AHCI device
/// (0x010601) with an ABAR MMIO BAR (BAR5 on Intel ICH9).
pub struct AhciPciDevice {
    config: PciConfigSpace,
    pub abar: u32,
    abar_probe: bool,
    pub controller: AhciController,
}

impl AhciPciDevice {
    pub fn new(controller: AhciController, abar_base: u32) -> Self {
        let mut config = PciConfigSpace::new();
        let pci_profile = profile::SATA_AHCI_ICH9;

        config.set_u16(0x00, pci_profile.vendor_id);
        config.set_u16(0x02, pci_profile.device_id);

        config.write(0x08, 1, u32::from(pci_profile.revision_id));
        config.write(0x09, 1, u32::from(pci_profile.class.prog_if));
        config.write(0x0a, 1, u32::from(pci_profile.class.sub_class));
        config.write(0x0b, 1, u32::from(pci_profile.class.base_class));
        config.set_u8(0x0e, pci_profile.header_type);

        config.write(0x2c, 2, u32::from(pci_profile.subsystem_vendor_id));
        config.write(0x2e, 2, u32::from(pci_profile.subsystem_id));

        let abar_mask = !(profile::AHCI_ABAR_SIZE_U32 - 1) & 0xffff_fff0;
        let abar = abar_base & abar_mask;
        config.set_u32(AHCI_ABAR_CFG_OFFSET as usize, abar);

        let int_pin = pci_profile
            .interrupt_pin
            .map(|p| p.to_config_u8())
            .unwrap_or(0);
        config.set_u8(0x3d, int_pin);

        Self {
            config,
            abar,
            abar_probe: false,
            controller,
        }
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn mem_space_enabled(&self) -> bool {
        (self.command() & PCI_COMMAND_MEM_ENABLE) != 0
    }

    fn bus_master_enabled(&self) -> bool {
        (self.command() & PCI_COMMAND_BUS_MASTER_ENABLE) != 0
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & PCI_COMMAND_INTX_DISABLE) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.controller.irq_level()
    }
}

impl PciDevice for AhciPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }
        let Some(end) = offset.checked_add(size as u16) else {
            return 0;
        };
        if end as usize > 256 {
            return 0;
        }

        let bar_off = AHCI_ABAR_CFG_OFFSET;
        let bar_end = bar_off + 4;
        let overlaps_bar = offset < bar_end && end > bar_off;

        if overlaps_bar {
            let mask = !(profile::AHCI_ABAR_SIZE_U32 - 1) & 0xffff_fff0;
            let bar_val = if self.abar_probe { mask } else { self.abar };

            let mut out = 0u32;
            for i in 0..size {
                let byte_off = offset + i as u16;
                let byte = if (bar_off..bar_end).contains(&byte_off) {
                    let shift = u32::from(byte_off - bar_off) * 8;
                    (bar_val >> shift) & 0xFF
                } else {
                    self.config.read(byte_off, 1) & 0xFF
                };
                out |= byte << (8 * i);
            }
            return out;
        }

        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        let bar_off = AHCI_ABAR_CFG_OFFSET;
        let Some(end) = offset.checked_add(size as u16) else {
            return;
        };
        if end as usize > 256 {
            return;
        }
        let overlaps_bar = offset < bar_off + 4 && end > bar_off;

        if overlaps_bar {
            // PCI BAR probing uses an all-ones write to discover the size mask.
            if offset == bar_off && size == 4 && value == 0xffff_ffff {
                self.abar_probe = true;
                self.abar = 0;
                self.config.write(bar_off, 4, 0);
                return;
            }

            // Apply the write byte-wise, then clamp to the BAR alignment and flags.
            self.abar_probe = false;
            self.config.write(offset, size, value);

            let raw = self.config.read(bar_off, 4);
            let mask = !(profile::AHCI_ABAR_SIZE_U32 - 1) & 0xffff_fff0;
            self.abar = raw & mask;
            self.config.write(bar_off, 4, self.abar);
            return;
        }

        self.config.write(offset, size, value);
    }
}

impl MmioDevice for AhciPciDevice {
    fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => 0,
            };
        }

        let bme = self.bus_master_enabled();
        self.controller
            .with_process_on_mmio(bme, |ctl| ctl.mmio_read(mem, offset, size))
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return;
        }

        let bme = self.bus_master_enabled();
        self.controller
            .with_process_on_mmio(bme, |ctl| ctl.mmio_write(mem, offset, size, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::storage::disk::MemDisk;

    #[test]
    fn abar_probe_subword_reads_return_mask_bytes() {
        let disk = Box::new(MemDisk::new(16));
        let controller = AhciController::new(disk);
        let mut dev = AhciPciDevice::new(controller, 0);

        dev.config_write(AHCI_ABAR_CFG_OFFSET, 4, 0xffff_ffff);
        let mask = dev.config_read(AHCI_ABAR_CFG_OFFSET, 4);
        assert_eq!(mask, !(profile::AHCI_ABAR_SIZE_U32 - 1) & 0xffff_fff0);

        // Subword reads should return bytes from the probe mask (not the raw config bytes, which
        // are cleared during probing).
        assert_eq!(dev.config_read(AHCI_ABAR_CFG_OFFSET, 1), mask & 0xFF);
        assert_eq!(
            dev.config_read(AHCI_ABAR_CFG_OFFSET + 1, 1),
            (mask >> 8) & 0xFF
        );
        assert_eq!(
            dev.config_read(AHCI_ABAR_CFG_OFFSET + 2, 2),
            (mask >> 16) & 0xFFFF
        );
    }
}
