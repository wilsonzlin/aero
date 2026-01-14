//! IDE (PIIX3) storage controller compatibility layer.
//!
//! The canonical IDE/ATA/ATAPI + Bus Master IDE implementation lives in `aero-devices-storage`.
//! Historically, `crates/emulator` carried an in-tree implementation and exposed it through the
//! `emulator::io::storage::ide` module.
//!
//! This module preserves the emulator-facing API surface used by legacy tests while delegating
//! behavior to the canonical device model (`aero_devices_storage::pci_ide::Piix3IdePciDevice`).

use std::cell::RefCell;
use std::io;

use aero_devices::pci::PciDevice as _;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotResult, SnapshotVersion};
use memory::MemoryBus;

use crate::io::pci::PciDevice as EmuPciDevice;
use crate::io::storage::adapters::VirtualDiskFromEmuDiskBackend;
use crate::io::storage::disk::DiskBackend;
use crate::io::storage::error::DiskResult;
use crate::io::storage::pci_compat;

pub use aero_devices_storage::busmaster::{BusMasterChannel, PrdEntry};

/// Legacy primary/secondary port assignments (command block + control block).
///
/// Note: For PIIX3, the PCI BAR1/BAR3 bases are typically programmed to `ctrl_base - 2` (e.g.
/// 0x3F4) so the 4-byte BAR window covers the 2 control ports at `ctrl_base..=ctrl_base+1`
/// (alt-status/dev-ctl + drive address).
#[derive(Debug, Clone, Copy)]
pub struct IdePortMap {
    pub cmd_base: u16,
    pub ctrl_base: u16,
    pub irq: u8,
}

pub const PRIMARY_PORTS: IdePortMap = IdePortMap {
    cmd_base: 0x1F0,
    ctrl_base: 0x3F6,
    irq: 14,
};

pub const SECONDARY_PORTS: IdePortMap = IdePortMap {
    cmd_base: 0x170,
    ctrl_base: 0x376,
    irq: 15,
};

/// Read-only ISO9660 (or raw CD) backing store.
///
/// The IDE/ATAPI layer treats the image as a sequence of 2048-byte sectors.
///
/// This is the legacy `crates/emulator` trait shape retained for compatibility; it is adapted into
/// the canonical `aero-devices-storage` ATAPI `IsoBackend` trait.
#[cfg(not(target_arch = "wasm32"))]
pub trait IsoBackend: Send {
    fn sector_count(&self) -> u32;
    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> DiskResult<()>;
}

#[cfg(target_arch = "wasm32")]
pub trait IsoBackend {
    fn sector_count(&self) -> u32;
    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> DiskResult<()>;
}

struct IsoBackendAdapter {
    inner: Box<dyn IsoBackend>,
}

impl aero_devices_storage::atapi::IsoBackend for IsoBackendAdapter {
    fn sector_count(&self) -> u32 {
        self.inner.sector_count()
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_sectors(lba, buf).map_err(io::Error::other)
    }
}

fn adapt_iso_backend(
    backend: Box<dyn IsoBackend>,
) -> Box<dyn aero_devices_storage::atapi::IsoBackend> {
    Box::new(IsoBackendAdapter { inner: backend })
}

/// Compatibility wrapper for an ATAPI CD-ROM drive.
///
/// This is a newtype wrapper around the canonical `aero-devices-storage` ATAPI device model so we
/// can keep the legacy `IsoBackend` trait signature.
pub struct AtapiCdrom(aero_devices_storage::atapi::AtapiCdrom);

impl AtapiCdrom {
    pub fn new(backend: Option<Box<dyn IsoBackend>>) -> Self {
        let backend = backend.map(adapt_iso_backend);
        Self(aero_devices_storage::atapi::AtapiCdrom::new(backend))
    }

    fn into_inner(self) -> aero_devices_storage::atapi::AtapiCdrom {
        self.0
    }

    pub fn supports_dma(&self) -> bool {
        self.0.supports_dma()
    }

    pub fn insert_media(&mut self, backend: Box<dyn IsoBackend>) {
        self.0.insert_media(adapt_iso_backend(backend));
    }

    pub fn eject_media(&mut self) {
        self.0.eject_media();
    }
}

/// Compatibility wrapper for an ATA hard drive.
///
/// This adapts the emulator `DiskBackend` trait into the canonical `aero_storage::VirtualDisk`
/// abstraction expected by `aero-devices-storage`'s `AtaDrive`.
pub struct AtaDevice(aero_devices_storage::ata::AtaDrive);

impl AtaDevice {
    pub fn new(backend: Box<dyn DiskBackend>, _model: impl Into<String>) -> Self {
        let disk = VirtualDiskFromEmuDiskBackend::new(backend);
        let drive = aero_devices_storage::ata::AtaDrive::new(Box::new(disk))
            .expect("failed to construct canonical ATA drive");
        Self(drive)
    }

    fn into_inner(self) -> aero_devices_storage::ata::AtaDrive {
        self.0
    }
}

/// Legacy emulator IDE controller facade.
///
/// This wraps the canonical PCI PIIX3 IDE controller device model and exposes the methods/tests
/// expected by the legacy emulator harness.
pub struct IdeController {
    inner: RefCell<aero_devices_storage::pci_ide::Piix3IdePciDevice>,
}

impl IdeController {
    pub fn new(bus_master_base: u16) -> Self {
        let mut inner = aero_devices_storage::pci_ide::Piix3IdePciDevice::new();

        // Preserve legacy emulator semantics: expose the controller with I/O decode + bus mastering
        // enabled by default so unit tests can directly poke legacy ports without modeling a full
        // PCI platform/BIOS.
        inner.config_mut().set_command(0x0005);

        inner
            .config_mut()
            .set_bar_base(4, u64::from(bus_master_base));
        inner.controller.set_bus_master_base(bus_master_base);

        Self {
            inner: RefCell::new(inner),
        }
    }

    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        self.inner.get_mut().io_read(port, size)
    }

    pub fn io_write(&mut self, port: u16, size: u8, value: u32) {
        self.inner.get_mut().io_write(port, size, value)
    }

    pub fn tick(&mut self, mem: &mut dyn MemoryBus) {
        self.inner.get_mut().tick(mem)
    }

    pub fn bus_master_base(&self) -> u16 {
        self.inner.borrow().bus_master_base()
    }

    pub fn attach_primary_master_ata(&mut self, dev: AtaDevice) {
        self.inner
            .get_mut()
            .controller
            .attach_primary_master_ata(dev.into_inner());
    }

    pub fn attach_secondary_master_ata(&mut self, dev: AtaDevice) {
        self.inner
            .get_mut()
            .controller
            .attach_secondary_master_ata(dev.into_inner());
    }

    pub fn attach_primary_master_atapi(&mut self, dev: AtapiCdrom) {
        self.inner
            .get_mut()
            .controller
            .attach_primary_master_atapi(dev.into_inner());
    }

    pub fn attach_secondary_master_atapi(&mut self, dev: AtapiCdrom) {
        self.inner
            .get_mut()
            .controller
            .attach_secondary_master_atapi(dev.into_inner());
    }

    /// Reattach an ATAPI backend after snapshot restore without altering guest-visible media state.
    pub fn attach_primary_master_atapi_backend_for_restore(
        &mut self,
        backend: Box<dyn IsoBackend>,
    ) {
        self.inner
            .get_mut()
            .controller
            .attach_primary_master_atapi_backend_for_restore(adapt_iso_backend(backend));
    }

    /// Reattach an ATAPI backend after snapshot restore without altering guest-visible media state.
    pub fn attach_secondary_master_atapi_backend_for_restore(
        &mut self,
        backend: Box<dyn IsoBackend>,
    ) {
        self.inner
            .get_mut()
            .controller
            .attach_secondary_master_atapi_backend_for_restore(adapt_iso_backend(backend));
    }

    pub fn primary_irq_pending(&self) -> bool {
        self.inner.borrow().controller.primary_irq_pending()
    }

    pub fn secondary_irq_pending(&self) -> bool {
        self.inner.borrow().controller.secondary_irq_pending()
    }

    pub fn clear_primary_irq(&mut self) {
        // STATUS reads acknowledge/clear the IDE IRQ latch.
        //
        // Use the controller core directly so this can be used even when PCI I/O decode is
        // disabled.
        let _ = self
            .inner
            .get_mut()
            .controller
            .io_read(PRIMARY_PORTS.cmd_base + 7, 1);
    }

    pub fn clear_secondary_irq(&mut self) {
        let _ = self
            .inner
            .get_mut()
            .controller
            .io_read(SECONDARY_PORTS.cmd_base + 7, 1);
    }

    /// Read from the PCI configuration space (little-endian).
    pub fn pci_config_read(&self, offset: u16, size: u8) -> u32 {
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return 0;
        };
        pci_compat::config_read(inner.config_mut(), offset, size as usize)
    }

    /// Write to the PCI configuration space (little-endian).
    pub fn pci_config_write(&mut self, offset: u16, size: u8, value: u32) {
        pci_compat::config_write(
            self.inner.get_mut().config_mut(),
            offset,
            size as usize,
            value,
        );
    }
}

impl IoSnapshot for IdeController {
    const DEVICE_ID: [u8; 4] =
        <aero_devices_storage::pci_ide::Piix3IdePciDevice as IoSnapshot>::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion =
        <aero_devices_storage::pci_ide::Piix3IdePciDevice as IoSnapshot>::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.inner.borrow().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        self.inner.get_mut().load_state(bytes)
    }
}

impl EmuPciDevice for IdeController {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        self.pci_config_read(offset, size as u8)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        self.pci_config_write(offset, size as u8, value)
    }
}
