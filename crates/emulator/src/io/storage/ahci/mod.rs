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
                let byte = ((value >> (idx * 8)) & 0xFF) as u32;
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
        config.write(0x0e, 1, u32::from(pci_profile.header_type));

        config.write(0x2c, 2, u32::from(pci_profile.subsystem_vendor_id));
        config.write(0x2e, 2, u32::from(pci_profile.subsystem_id));

        let abar_mask = !(profile::AHCI_ABAR_SIZE_U32 - 1) & 0xffff_fff0;
        let abar = abar_base & abar_mask;
        config.set_u32(AHCI_ABAR_CFG_OFFSET as usize, abar);

        let int_pin = pci_profile
            .interrupt_pin
            .map(|p| p.to_config_u8())
            .unwrap_or(0);
        config.write(0x3d, 1, u32::from(int_pin));

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
        if offset == AHCI_ABAR_CFG_OFFSET && size == 4 {
            if self.abar_probe {
                return !(profile::AHCI_ABAR_SIZE_U32 - 1) & 0xffff_fff0;
            }
            return self.abar;
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        let bar_off = AHCI_ABAR_CFG_OFFSET;
        let Some(end) = offset.checked_add(size as u16) else {
            return;
        };
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
    use memory::MemoryBus;
    use std::sync::{Arc, Mutex};

    const ATA_CMD_READ_DMA_EXT: u8 = 0x25;

    #[derive(Clone, Debug)]
    struct VecMemory {
        data: Vec<u8>,
    }

    impl VecMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
            let start = usize::try_from(paddr).expect("paddr too large for VecMemory");
            let end = start.checked_add(len).expect("address wrap");
            assert!(end <= self.data.len(), "out-of-bounds physical access");
            start..end
        }
    }

    impl MemoryBus for VecMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let range = self.range(paddr, buf.len());
            buf.copy_from_slice(&self.data[range]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let range = self.range(paddr, buf.len());
            self.data[range].copy_from_slice(buf);
        }
    }

    #[derive(Clone)]
    struct SharedDisk(Arc<Mutex<MemDisk>>);

    impl DiskBackend for SharedDisk {
        fn sector_size(&self) -> u32 {
            self.0.lock().unwrap().sector_size()
        }

        fn total_sectors(&self) -> u64 {
            self.0.lock().unwrap().total_sectors()
        }

        fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
            self.0.lock().unwrap().read_sectors(lba, buf)
        }

        fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
            self.0.lock().unwrap().write_sectors(lba, buf)
        }

        fn flush(&mut self) -> DiskResult<()> {
            self.0.lock().unwrap().flush()
        }
    }

    fn build_cmd_header(cfl_dwords: u32, write: bool, prdt_len: u16, ctba: u64) -> [u8; 32] {
        let mut buf = [0u8; 32];
        let mut dw0 = cfl_dwords & 0x1f;
        if write {
            dw0 |= 1 << 6;
        }
        dw0 |= (prdt_len as u32) << 16;
        buf[0..4].copy_from_slice(&dw0.to_le_bytes());
        buf[8..12].copy_from_slice(&(ctba as u32).to_le_bytes());
        buf[12..16].copy_from_slice(&((ctba >> 32) as u32).to_le_bytes());
        buf
    }

    fn write_reg_h2d_fis(mem: &mut VecMemory, addr: u64, cmd: u8, lba: u64, count: u16) {
        let mut fis = [0u8; 64];
        fis[0] = 0x27; // Register H2D FIS
        fis[1] = 0x80; // C=1
        fis[2] = cmd;

        fis[4] = (lba & 0xff) as u8;
        fis[5] = ((lba >> 8) & 0xff) as u8;
        fis[6] = ((lba >> 16) & 0xff) as u8;
        fis[7] = 0x40; // device: LBA mode
        fis[8] = ((lba >> 24) & 0xff) as u8;
        fis[9] = ((lba >> 32) & 0xff) as u8;
        fis[10] = ((lba >> 40) & 0xff) as u8;

        fis[12] = (count & 0xff) as u8;
        fis[13] = (count >> 8) as u8;

        mem.write_physical(addr, &fis);
    }

    fn write_prd(mem: &mut VecMemory, addr: u64, dba: u64, len: u32) {
        let mut prd = [0u8; 16];
        prd[0..4].copy_from_slice(&(dba as u32).to_le_bytes());
        prd[4..8].copy_from_slice(&((dba >> 32) as u32).to_le_bytes());
        let dbc = len.saturating_sub(1) & 0x003f_ffff;
        prd[12..16].copy_from_slice(&dbc.to_le_bytes());
        mem.write_physical(addr, &prd);
    }

    #[test]
    fn snapshot_roundtrip_preserves_mmio_state_and_requires_disk_reattach_for_dma() {
        let disk = Arc::new(Mutex::new(MemDisk::new(16)));
        {
            let mut d = disk.lock().unwrap();
            for (i, b) in d.data_mut().iter_mut().enumerate() {
                *b = (i & 0xff) as u8;
            }
        }
        let shared_disk = SharedDisk(disk.clone());

        let mut mem = VecMemory::new(0x20_000);
        let mut controller = AhciController::new(Box::new(shared_disk.clone()));

        let clb = 0x1000u64;
        let fb = 0x2000u64;
        let ctba = 0x3000u64;
        let dst = 0x4000u64;

        controller.mmio_write_u32(
            &mut mem,
            registers::HBA_GHC,
            registers::GHC_AE | registers::GHC_IE,
        );
        controller.mmio_write_u32(
            &mut mem,
            registers::HBA_PORTS_BASE + registers::PX_CLB,
            clb as u32,
        );
        controller.mmio_write_u32(
            &mut mem,
            registers::HBA_PORTS_BASE + registers::PX_FB,
            fb as u32,
        );
        controller.mmio_write_u32(
            &mut mem,
            registers::HBA_PORTS_BASE + registers::PX_IE,
            registers::PXIE_DHRE,
        );
        controller.mmio_write_u32(
            &mut mem,
            registers::HBA_PORTS_BASE + registers::PX_CMD,
            registers::PXCMD_FRE | registers::PXCMD_ST | registers::PXCMD_SUD,
        );

        let snap = controller.save_state();

        let mut restored = AhciController::new(Box::new(shared_disk.clone()));
        restored.load_state(&snap).unwrap();

        assert_eq!(
            restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CLB) as u64
                | ((restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CLBU)
                    as u64)
                    << 32),
            clb
        );
        assert_eq!(
            restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_FB) as u64
                | ((restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_FBU)
                    as u64)
                    << 32),
            fb
        );

        // Prepare a READ DMA EXT command.
        let header = build_cmd_header(5, false, 1, ctba);
        mem.write_physical(clb, &header);
        write_reg_h2d_fis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 2, 1);
        write_prd(&mut mem, ctba + 0x80, dst, 512);
        mem.write_physical(dst, &[0xaa; 512]);

        // Without re-attaching a disk, the canonical controller leaves the command pending and does
        // not DMA.
        restored.mmio_write_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CI, 1);
        assert_eq!(
            restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CI) & 1,
            1
        );

        let mut got = [0u8; 512];
        mem.read_physical(dst, &mut got);
        assert_eq!(got, [0xaa; 512]);

        // Re-attach the disk and trigger processing on the next MMIO access.
        restored.attach_disk(Box::new(shared_disk));
        let _ = restored.mmio_read_u32(&mut mem, registers::HBA_IS);

        assert_eq!(
            restored.mmio_read_u32(&mut mem, registers::HBA_PORTS_BASE + registers::PX_CI) & 1,
            0
        );

        mem.read_physical(dst, &mut got);
        let disk_guard = disk.lock().unwrap();
        let expected = &disk_guard.data()[2 * 512..3 * 512];
        assert_eq!(&got[..], expected);
    }
}
