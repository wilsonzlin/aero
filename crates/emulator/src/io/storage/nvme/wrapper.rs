use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};
use crate::io::storage::disk::{DiskBackend, DiskError};
use aero_devices_nvme as canonical;
use memory::MemoryBus;

const NVME_REG_CC: u64 = 0x0014;
const NVME_REG_CSTS: u64 = 0x001c;
const NVME_REG_AQA: u64 = 0x0024;

const NVME_DOORBELL_BASE: u64 = 0x1000;

const CC_EN: u32 = 1 << 0;

/// Adapter mapping the emulator `DiskBackend` trait into the canonical NVMe backend trait.
struct EmulatorDiskAsNvmeBackend {
    inner: Box<dyn DiskBackend>,
}

fn map_disk_error(err: DiskError) -> canonical::DiskError {
    match err {
        DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors,
        } => canonical::DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors,
        },
        DiskError::UnalignedBuffer { len, sector_size } => canonical::DiskError::UnalignedBuffer {
            len,
            sector_size,
        },
        DiskError::OutOfBounds
        | DiskError::InvalidBufferLength
        | DiskError::NotSupported(_)
        | DiskError::QuotaExceeded
        | DiskError::InUse
        | DiskError::InvalidState(_)
        | DiskError::BackendUnavailable
        | DiskError::Io(_)
        | DiskError::CorruptImage(_)
        | DiskError::Unsupported(_) => canonical::DiskError::Io,
    }
}

impl canonical::DiskBackend for EmulatorDiskAsNvmeBackend {
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn total_sectors(&self) -> u64 {
        self.inner.total_sectors()
    }

    fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> canonical::DiskResult<()> {
        self.inner
            .read_sectors(lba, buffer)
            .map_err(map_disk_error)
    }

    fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> canonical::DiskResult<()> {
        self.inner
            .write_sectors(lba, buffer)
            .map_err(map_disk_error)
    }

    fn flush(&mut self) -> canonical::DiskResult<()> {
        self.inner.flush().map_err(map_disk_error)
    }
}

pub struct NvmeController {
    inner: canonical::NvmeController,
    cfs: bool,
}

impl NvmeController {
    pub const BAR0_SIZE: u64 = 0x4000;

    pub fn new(disk: Box<dyn DiskBackend>) -> Self {
        let inner = canonical::NvmeController::new(Box::new(EmulatorDiskAsNvmeBackend { inner: disk }));
        Self { inner, cfs: false }
    }

    pub fn irq_level(&self) -> bool {
        self.inner.intx_level
    }

    pub fn mmio_read_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64) -> u32 {
        self.mmio_read(mem, offset, 4)
    }

    pub fn mmio_write_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        self.mmio_write(mem, offset, 4, value);
    }

    fn process(&mut self, mem: &mut dyn MemoryBus) {
        self.inner.process(mem);
    }

    fn mmio_read_internal(&self, offset: u64, size: usize) -> u64 {
        let size = size.clamp(1, 8);
        let mut value = self.inner.mmio_read(offset, size);

        if self.cfs {
            // Inject CSTS.CFS (bit 1 of the CSTS register) into the returned byte slice.
            // This preserves the legacy emulator NVMe behaviour where invalid controller enables
            // set CSTS.CFS for hardening tests.
            let abs_bit = NVME_REG_CSTS
                .checked_mul(8)
                .and_then(|v| v.checked_add(1))
                .unwrap_or(u64::MAX);
            let start_bit = offset.saturating_mul(8);
            let end_bit = start_bit.saturating_add(size as u64 * 8);
            if abs_bit >= start_bit && abs_bit < end_bit {
                let rel = abs_bit - start_bit;
                if rel < 64 {
                    value |= 1u64 << rel;
                }
            }
        }

        value
    }

    fn clamp_aqa(value: u32) -> u32 {
        // The canonical NVMe model limits queue entry counts. Keep the legacy emulator behaviour
        // (accept arbitrarily large guest values without panicking) by clamping to a safe value
        // during MMIO writes.
        //
        // Note: AQA encodes both sizes as "size-1". A value of 0x7f corresponds to 128 entries.
        let acqs = (value & 0xffff).min(0x7f);
        let asqs = ((value >> 16) & 0xffff).min(0x7f);
        (asqs << 16) | acqs
    }

    fn doorbell_is_sq(offset: u64) -> bool {
        let aligned = offset & !3;
        if aligned < NVME_DOORBELL_BASE {
            return false;
        }
        let rel = aligned - NVME_DOORBELL_BASE;
        let db_index = rel / 4;
        db_index % 2 == 0
    }

    fn mmio_write_internal(
        &mut self,
        mem: &mut dyn MemoryBus,
        offset: u64,
        size: usize,
        value: u32,
        allow_dma: bool,
    ) {
        // Canonical NVMe requires admin queues to be configured prior to enabling. The legacy
        // emulator NVMe stack historically allowed enabling with missing ASQ/ACQ, but that flows
        // into a device model that will never make forward progress. Leave canonical behaviour
        // intact; tests that require RDY should configure queues.

        if offset == NVME_REG_AQA && size == 4 {
            let clamped = Self::clamp_aqa(value);
            self.inner.mmio_write(offset, size, clamped as u64);
            return;
        }

        if offset == NVME_REG_CC && size == 4 {
            let prev_cc = self.inner.mmio_read(NVME_REG_CC, 4) as u32;
            let prev_en = (prev_cc & CC_EN) != 0;
            let next_en = (value & CC_EN) != 0;
            if !prev_en && next_en {
                // Clear CFS for each enable attempt (mirrors legacy behaviour).
                self.cfs = false;

                // Reject unsupported page sizes (CC.MPS != 0). The canonical device model is fixed
                // at 4KiB pages for PRP handling, so other values must fail.
                let mps = (value >> 7) & 0xf;
                if mps != 0 {
                    self.cfs = true;
                    // Keep CC.EN clear to avoid enabling the canonical controller state machine.
                    self.inner.mmio_write(offset, size, (value & !CC_EN) as u64);
                    return;
                }
            }

            self.inner.mmio_write(offset, size, value as u64);
            return;
        }

        self.inner.mmio_write(offset, size, value as u64);

        // Preserve the legacy "doorbell triggers progress" behaviour by processing pending DMA
        // work immediately after SQ doorbell writes.
        if allow_dma && Self::doorbell_is_sq(offset) {
            self.process(mem);
        }
    }
}

impl MmioDevice for NvmeController {
    fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        self.mmio_read_internal(offset, size) as u32
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        self.mmio_write_internal(mem, offset, size, value, true);
    }
}

pub struct NvmePciDevice {
    config: PciConfigSpace,
    pub bar0: u64,
    bar0_probe: bool,
    deferred_processing: bool,
    pub controller: NvmeController,
}

impl NvmePciDevice {
    pub fn new(controller: NvmeController, bar0: u64) -> Self {
        let mut config = PciConfigSpace::new();
        config.set_u16(0x00, 0x1b36);
        config.set_u16(0x02, 0x0010);

        config.write(0x09, 1, 0x02);
        config.write(0x0a, 1, 0x08);
        config.write(0x0b, 1, 0x01);

        // BAR0/BAR1: 64-bit non-prefetchable MMIO.
        let bar0_addr_mask_lo = !(NvmeController::BAR0_SIZE as u32 - 1) & 0xffff_fff0;
        let bar0 = (bar0 & 0xffff_ffff_0000_0000) | (bar0 & u64::from(bar0_addr_mask_lo));
        config.set_u32(0x10, (bar0 as u32 & bar0_addr_mask_lo) | 0x4);
        config.set_u32(0x14, (bar0 >> 32) as u32);
        config.write(0x3d, 1, 1);

        Self {
            config,
            bar0,
            bar0_probe: false,
            deferred_processing: false,
            controller,
        }
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & (1 << 10)) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.controller.irq_level()
    }

    fn update_deferred_processing(&mut self, _prev_command: u16) {
        // In the legacy emulator NVMe model, SQ doorbell writes perform DMA immediately. The
        // canonical model defers DMA into an explicit `process()` step. This wrapper keeps the old
        // behaviour by running `process()` on doorbell writes when bus mastering is enabled, and by
        // remembering "deferred" doorbell writes that occurred while BME was clear.
        //
        // There is no additional state to update here beyond what `mmio_write` records.
    }

    fn maybe_process_deferred(&mut self, mem: &mut dyn MemoryBus, command: u16) {
        if !self.deferred_processing {
            return;
        }
        // Gate DMA on PCI Bus Master Enable (bit 2).
        if (command & (1 << 2)) == 0 {
            return;
        }

        self.controller.process(mem);
        self.deferred_processing = false;
    }
}

impl PciDevice for NvmePciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if offset == 0x10 && size == 4 {
            let addr_mask_lo = !(NvmeController::BAR0_SIZE as u32 - 1) & 0xffff_fff0;
            return if self.bar0_probe {
                addr_mask_lo | 0x4
            } else {
                (self.bar0 as u32 & addr_mask_lo) | 0x4
            };
        }
        if offset == 0x14 && size == 4 {
            return if self.bar0_probe {
                0xffff_ffff
            } else {
                (self.bar0 >> 32) as u32
            };
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        let prev_command = self.config.read(0x04, 2) as u16;
        if offset == 0x10 && size == 4 {
            if value == 0xffff_ffff {
                self.bar0_probe = true;
                self.bar0 = 0;
                self.config.write(offset, size, 0);
                self.update_deferred_processing(prev_command);
                return;
            }

            self.bar0_probe = false;
            let addr_mask_lo = !(NvmeController::BAR0_SIZE as u32 - 1) & 0xffff_fff0;
            let addr_lo = (value & addr_mask_lo) as u64;
            self.bar0 = (self.bar0 & 0xffff_ffff_0000_0000) | addr_lo;
            self.config.write(offset, size, (addr_lo as u32) | 0x4);
            self.update_deferred_processing(prev_command);
            return;
        }
        if offset == 0x14 && size == 4 {
            if value == 0xffff_ffff {
                self.bar0_probe = true;
                self.bar0 &= 0xffff_ffff;
                self.config.write(offset, size, 0);
                self.update_deferred_processing(prev_command);
                return;
            }

            self.bar0_probe = false;
            self.bar0 = (self.bar0 & 0x0000_0000_ffff_ffff) | ((value as u64) << 32);
            self.config.write(offset, size, value);
            self.update_deferred_processing(prev_command);
            return;
        }
        self.config.write(offset, size, value);
        self.update_deferred_processing(prev_command);
    }
}

impl MmioDevice for NvmePciDevice {
    fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        let command = self.config.read(0x04, 2) as u16;
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if (command & (1 << 1)) == 0 {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => 0,
            };
        }

        self.maybe_process_deferred(mem, command);
        self.controller.mmio_read(mem, offset, size)
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        let command = self.config.read(0x04, 2) as u16;
        // Gate MMIO on PCI command Memory Space Enable (bit 1).
        if (command & (1 << 1)) == 0 {
            return;
        }

        self.maybe_process_deferred(mem, command);

        // Gate DMA on PCI command Bus Master Enable (bit 2).
        let bus_master_enabled = (command & (1 << 2)) != 0;
        let allow_dma = bus_master_enabled;

        if !bus_master_enabled && NvmeController::doorbell_is_sq(offset) {
            // Latch the doorbell update but defer processing until bus mastering is enabled.
            self.controller
                .mmio_write_internal(mem, offset, size, value, false);
            self.deferred_processing = true;
            return;
        }

        self.controller
            .mmio_write_internal(mem, offset, size, value, allow_dma);
    }
}
