#![forbid(unsafe_code)]

use aero_audio::hda_pci::HdaPciDevice;
use aero_devices::a20_gate::A20Gate;
use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, SharedAcpiPmIo};
use aero_devices::clock::ManualClock;
use aero_devices::i8042::{register_i8042, I8042Ports, SharedI8042Controller};
use aero_devices::irq::PlatformIrqLine;
use aero_devices::pci::{
    bios_post, register_pci_config_ports, PciBarDefinition, PciBdf, PciConfigPorts, PciDevice,
    PciEcamConfig, PciEcamMmio, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
    PciResourceAllocator, PciResourceAllocatorConfig, SharedPciConfigPorts,
};
use aero_devices::pic8259::register_pic8259_on_platform_interrupts;
use aero_devices::pit8254::{register_pit8254, Pit8254, SharedPit8254};
use aero_devices::reset_ctrl::{ResetCtrl, ResetKind, RESET_CTRL_PORT};
use aero_devices::rtc_cmos::{register_rtc_cmos, RtcCmos, SharedRtcCmos};
use aero_devices::usb::uhci::UhciPciDevice;
use aero_devices::{hpet, i8042};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::pci_ide::{Piix3IdePciDevice, PRIMARY_PORTS, SECONDARY_PORTS};
use aero_devices_storage::AhciPciDevice;
use aero_devices_nvme::{NvmeController, NvmePciDevice};
use aero_interrupts::apic::{IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, LAPIC_MMIO_BASE, LAPIC_MMIO_SIZE};
use aero_net_e1000::E1000Device;
use aero_virtio::devices::blk::VirtioBlk;
use aero_virtio::memory::{
    GuestMemory as VirtioGuestMemory, GuestMemoryError as VirtioGuestMemoryError,
};
use aero_virtio::pci::{InterruptSink as VirtioInterruptSink, VirtioPciDevice};
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::ChipsetState;
use aero_platform::dirty_memory::DEFAULT_DIRTY_PAGE_SIZE;
use aero_platform::interrupts::{InterruptInput, PlatformInterrupts};
use aero_platform::io::{IoPortBus, PortIoDevice};
use aero_platform::memory::MemoryBus;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::{DenseMemory, GuestMemory, MmioHandler};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

mod cpu_core;
mod pci_mmio;
pub use cpu_core::{PcCpuBus, PcInterruptController};
pub use pci_mmio::{PciBarMmioHandler, PciBarMmioRouter};

mod firmware_pci;
pub use firmware_pci::{PciConfigPortsBiosAdapter, SharedPciConfigPortsBiosAdapter};

mod pci_io_router;
pub use pci_io_router::{PciIoBarHandler, PciIoBarRouter};

/// Base physical address of the PCIe ECAM ("MMCONFIG") window.
///
/// This follows the QEMU Q35 convention (256MiB window at 0xB000_0000 covering buses 0..=255).
pub const PCIE_ECAM_BASE: u64 = aero_pc_constants::PCIE_ECAM_BASE;

pub const PCIE_ECAM_CONFIG: PciEcamConfig = PciEcamConfig {
    segment: aero_pc_constants::PCIE_ECAM_SEGMENT,
    start_bus: aero_pc_constants::PCIE_ECAM_START_BUS,
    end_bus: aero_pc_constants::PCIE_ECAM_END_BUS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetEvent {
    Cpu,
    System,
}

#[derive(Debug, Clone, Copy)]
pub struct PcPlatformConfig {
    pub enable_hda: bool,
    pub enable_nvme: bool,
    pub enable_ahci: bool,
    pub enable_ide: bool,
    pub enable_e1000: bool,
    pub mac_addr: Option<[u8; 6]>,
    pub enable_uhci: bool,
    pub enable_virtio_blk: bool,
}

impl Default for PcPlatformConfig {
    fn default() -> Self {
        Self {
            enable_hda: false,
            enable_nvme: false,
            // The canonical PC platform always includes an ICH9 AHCI controller so guests can
            // boot from SATA disks without additional configuration.
            enable_ahci: true,
            enable_ide: false,
            enable_e1000: false,
            mac_addr: None,
            // USB is a core piece of the canonical PC platform; enable UHCI by default so guests
            // can discover a basic USB 1.1 controller without opting in to extra devices.
            enable_uhci: true,
            enable_virtio_blk: false,
        }
    }
}

struct HdaPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl HdaPciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::HDA_ICH6.build_config_space();
        config.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: HdaPciDevice::MMIO_BAR_SIZE,
                prefetchable: false,
            },
        );
        Self { config }
    }
}

impl PciDevice for HdaPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

struct AhciPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl AhciPciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::SATA_AHCI_ICH9.build_config_space();
        config.set_bar_definition(
            5,
            PciBarDefinition::Mmio32 {
                size: 0x2000,
                prefetchable: false,
            },
        );
        Self { config }
    }
}

impl PciDevice for AhciPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

struct E1000PciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl E1000PciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::NIC_E1000_82540EM.build_config_space();
        config.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: aero_net_e1000::E1000_MMIO_SIZE,
                prefetchable: false,
            },
        );
        config.set_bar_definition(
            1,
            PciBarDefinition::Io {
                size: aero_net_e1000::E1000_IO_SIZE,
            },
        );
        Self { config }
    }
}

impl PciDevice for E1000PciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

struct VirtioBlkPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl VirtioBlkPciConfigDevice {
    fn new() -> Self {
        let config = aero_devices::pci::profile::VIRTIO_BLK.build_config_space();
        Self { config }
    }
}

impl PciDevice for VirtioBlkPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

struct NvmePciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl NvmePciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::NVME_CONTROLLER.build_config_space();
        config.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: NvmeController::bar0_len(),
                prefetchable: false,
            },
        );
        Self { config }
    }
}

impl PciDevice for NvmePciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

struct IdePciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

struct Piix3IsaPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl Piix3IsaPciConfigDevice {
    fn new() -> Self {
        let config = aero_devices::pci::profile::ISA_PIIX3.build_config_space();
        Self { config }
    }
}

impl PciDevice for Piix3IsaPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

impl IdePciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::IDE_PIIX3.build_config_space();
        // Legacy IDE compatibility ports.
        config.set_bar_base(0, PRIMARY_PORTS.cmd_base as u64);
        config.set_bar_base(1, 0x3F4); // alt-status/dev-ctl at +2 => 0x3F6
        config.set_bar_base(2, SECONDARY_PORTS.cmd_base as u64);
        config.set_bar_base(3, 0x374); // alt-status/dev-ctl at +2 => 0x376
        config.set_bar_base(4, u64::from(Piix3IdePciDevice::DEFAULT_BUS_MASTER_BASE));
        Self { config }
    }
}

impl PciDevice for IdePciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

struct UhciPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl UhciPciConfigDevice {
    fn new() -> Self {
        let config = aero_devices::pci::profile::USB_UHCI_PIIX3.build_config_space();
        Self { config }
    }
}

impl PciDevice for UhciPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

#[derive(Clone)]
struct PcIdePort {
    pci_cfg: SharedPciConfigPorts,
    ide: Rc<RefCell<Piix3IdePciDevice>>,
    bdf: PciBdf,
    port: u16,
}

impl PcIdePort {
    fn sync_config(&self) {
        let (command, bar4_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let bus = pci_cfg.bus_mut();
            let cfg = bus.device_config(self.bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar4_base = cfg
                .and_then(|cfg| cfg.bar_range(4))
                .map(|range| range.base)
                .unwrap_or(0);
            (command, bar4_base)
        };

        let mut ide = self.ide.borrow_mut();
        ide.config_mut().set_command(command);
        if bar4_base != 0 {
            ide.config_mut().set_bar_base(4, bar4_base);
        }
    }
}

impl PortIoDevice for PcIdePort {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        self.sync_config();
        self.ide.borrow_mut().io_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        self.sync_config();
        self.ide.borrow_mut().io_write(port, size, value);
    }
}

/// Bus Master IDE (BAR4) handler registered via the platform's `PciIoWindow`.
///
/// The `port` argument is interpreted as the device-relative offset within BAR4.
#[derive(Clone)]
struct PcIdeBusMasterBar {
    pci_cfg: SharedPciConfigPorts,
    ide: Rc<RefCell<Piix3IdePciDevice>>,
    bdf: PciBdf,
}

impl PcIdeBusMasterBar {
    fn sync_config(&self) {
        let (command, bar4_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let bus = pci_cfg.bus_mut();
            let cfg = bus.device_config(self.bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar4_base = cfg
                .and_then(|cfg| cfg.bar_range(4))
                .map(|range| range.base)
                .unwrap_or(0);
            (command, bar4_base)
        };

        let mut ide = self.ide.borrow_mut();
        ide.config_mut().set_command(command);
        if bar4_base != 0 {
            ide.config_mut().set_bar_base(4, bar4_base);
        }
    }
}

impl PortIoDevice for PcIdeBusMasterBar {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        // `port` is the BAR-relative offset.
        self.sync_config();

        let base = { self.ide.borrow().bus_master_base() };
        let abs_port = base.wrapping_add(port);
        self.ide.borrow_mut().io_read(abs_port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        self.sync_config();

        let base = { self.ide.borrow().bus_master_base() };
        let abs_port = base.wrapping_add(port);
        self.ide.borrow_mut().io_write(abs_port, size, value);
    }
}

/// UHCI (BAR4) handler registered via the platform's `PciIoWindow`.
///
/// The `port` argument is interpreted as the device-relative offset within BAR4.
#[derive(Clone)]
struct PcUhciIoBar {
    uhci: Rc<RefCell<UhciPciDevice>>,
}

impl PcUhciIoBar {
    fn read_all_ones(size: u8) -> u32 {
        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl PortIoDevice for PcUhciIoBar {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return Self::read_all_ones(size),
        };
        self.uhci.borrow_mut().controller_mut().io_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };
        self.uhci
            .borrow_mut()
            .controller_mut()
            .io_write(port, size, value);
    }
}

#[derive(Clone)]
struct E1000PciIoBar {
    e1000: Rc<RefCell<E1000Device>>,
}

impl E1000PciIoBar {
    fn read_all_ones(size: u8) -> u32 {
        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl PortIoDevice for E1000PciIoBar {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return Self::read_all_ones(size),
        };
        self.e1000.borrow_mut().io_read(u32::from(port), size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };
        self.e1000
            .borrow_mut()
            .io_write_reg(u32::from(port), size, value);
    }
}

type PciIoBarKey = (PciBdf, u8);
type SharedPciIoBarMap = Rc<RefCell<HashMap<PciIoBarKey, Box<dyn PortIoDevice>>>>;

#[derive(Clone)]
struct PciIoWindowPort {
    pci_cfg: SharedPciConfigPorts,
    handlers: SharedPciIoBarMap,
}

impl PciIoWindowPort {
    fn map_port(&mut self, port: u16, size: u8) -> Option<(PciIoBarKey, u16)> {
        debug_assert!(matches!(size, 1 | 2 | 4));
        let port_u64 = u64::from(port);
        let access_end = port_u64.checked_add(u64::from(size))?;
        // Iterate the bus' mapped BARs (deterministic order) without per-access allocations.
        let handlers = self.handlers.borrow();
        let mut pci_cfg = self.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        for mapped in bus.iter_mapped_io_bars() {
            let key = (mapped.bdf, mapped.bar);
            if !handlers.contains_key(&key) {
                continue;
            }
            let range = mapped.range;
            // Treat port I/O accesses as byte-addressed ranges (like MMIO): require the entire
            // access to fit within the decoded BAR region.
            if port_u64 >= range.base && access_end <= range.end_exclusive() {
                let dev_offset = port_u64
                    .checked_sub(range.base)
                    .and_then(|v| u16::try_from(v).ok())?;
                return Some((key, dev_offset));
            }
        }

        None
    }

    fn read_all_ones(size: u8) -> u32 {
        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl PortIoDevice for PciIoWindowPort {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        if !matches!(size, 1 | 2 | 4) {
            return Self::read_all_ones(size);
        }
        let Some((key, dev_offset)) = self.map_port(port, size) else {
            return Self::read_all_ones(size);
        };

        let mut handlers = self.handlers.borrow_mut();
        let Some(handler) = handlers.get_mut(&key) else {
            return Self::read_all_ones(size);
        };

        // Dispatch using the device-relative offset.
        handler.read(dev_offset, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        let Some((key, dev_offset)) = self.map_port(port, size) else {
            return;
        };

        let mut handlers = self.handlers.borrow_mut();
        let Some(handler) = handlers.get_mut(&key) else {
            return;
        };

        handler.write(dev_offset, size, value);
    }

    fn reset(&mut self) {
        let mut handlers = self.handlers.borrow_mut();
        for dev in handlers.values_mut() {
            dev.reset();
        }
    }
}

struct HdaDmaMemory<'a> {
    mem: RefCell<&'a mut MemoryBus>,
}

impl aero_audio::mem::MemoryAccess for HdaDmaMemory<'_> {
    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        self.mem.borrow_mut().read_physical(addr, buf);
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        self.mem.borrow_mut().write_physical(addr, buf);
    }
}

#[derive(Default)]
struct NoopVirtioInterruptSink;

impl VirtioInterruptSink for NoopVirtioInterruptSink {
    fn raise_legacy_irq(&mut self) {}

    fn lower_legacy_irq(&mut self) {}

    fn signal_msix(&mut self, _vector: u16) {}
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

struct VirtioPciBar0Mmio {
    dev: Rc<RefCell<VirtioPciDevice>>,
}

impl PciBarMmioHandler for VirtioPciBar0Mmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let mut dev = self.dev.borrow_mut();
        match size {
            1 | 2 | 4 | 8 => {
                let mut buf = [0u8; 8];
                dev.bar0_read(offset, &mut buf[..size]);
                u64::from_le_bytes(buf)
            }
            _ => all_ones(size),
        }
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let mut dev = self.dev.borrow_mut();
        match size {
            1 | 2 | 4 | 8 => {
                let bytes = value.to_le_bytes();
                dev.bar0_write(offset, &bytes[..size]);
            }
            _ => {}
        }
    }
}

struct VirtioDmaMemory<'a> {
    mem: &'a mut MemoryBus,
}

impl VirtioGuestMemory for VirtioDmaMemory<'_> {
    fn len(&self) -> u64 {
        self.mem.ram().size()
    }

    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), VirtioGuestMemoryError> {
        self.mem
            .ram()
            .read_into(addr, dst)
            .map_err(|_| VirtioGuestMemoryError::OutOfBounds {
                addr,
                len: dst.len(),
            })
    }

    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), VirtioGuestMemoryError> {
        self.mem
            .ram_mut()
            .write_from(addr, src)
            .map_err(|_| VirtioGuestMemoryError::OutOfBounds {
                addr,
                len: src.len(),
            })
    }

    fn get_slice(&self, addr: u64, len: usize) -> Result<&[u8], VirtioGuestMemoryError> {
        self.mem
            .ram()
            .get_slice(addr, len)
            .ok_or(VirtioGuestMemoryError::OutOfBounds { addr, len })
    }

    fn get_slice_mut(
        &mut self,
        addr: u64,
        len: usize,
    ) -> Result<&mut [u8], VirtioGuestMemoryError> {
        self.mem
            .ram_mut()
            .get_slice_mut(addr, len)
            .ok_or(VirtioGuestMemoryError::OutOfBounds { addr, len })
    }
}

struct IoApicMmio {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl MmioHandler for IoApicMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let size = size.clamp(1, 8);
        let interrupts = self.interrupts.borrow_mut();
        let mut out = 0u64;
        for i in 0..size {
            let off = offset.wrapping_add(i as u64);
            let word_offset = off & !3;
            let shift = ((off & 3) * 8) as u32;
            let word = interrupts.ioapic_mmio_read(word_offset) as u64;
            let byte = (word >> shift) & 0xFF;
            out |= byte << (i * 8);
        }
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let size = size.clamp(1, 8);
        let mut interrupts = self.interrupts.borrow_mut();

        let mut idx = 0usize;
        while idx < size {
            let off = offset.wrapping_add(idx as u64);
            let word_offset = off & !3;
            let start_in_word = (off & 3) as usize;
            let mut word = interrupts.ioapic_mmio_read(word_offset);

            for byte_idx in start_in_word..4 {
                if idx >= size {
                    break;
                }
                let off = offset.wrapping_add(idx as u64);
                if (off & !3) != word_offset {
                    break;
                }
                let byte = ((value >> (idx * 8)) & 0xFF) as u32;
                let shift = (byte_idx * 8) as u32;
                word &= !(0xFF_u32 << shift);
                word |= byte << shift;
                idx += 1;
            }

            interrupts.ioapic_mmio_write(word_offset, word);
        }
    }
}

struct LapicMmio {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl MmioHandler for LapicMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let size = size.clamp(1, 8);
        let interrupts = self.interrupts.borrow();
        let mut buf = [0u8; 8];
        interrupts.lapic_mmio_read(offset, &mut buf[..size]);
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let size = size.clamp(1, 8);
        let interrupts = self.interrupts.borrow();
        let bytes = value.to_le_bytes();
        interrupts.lapic_mmio_write(offset, &bytes[..size]);
    }
}

struct HpetMmio {
    hpet: Rc<RefCell<hpet::Hpet<ManualClock>>>,
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl MmioHandler for HpetMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let mut hpet = self.hpet.borrow_mut();
        let mut interrupts = self.interrupts.borrow_mut();
        hpet.mmio_read(offset, size, &mut *interrupts)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let mut hpet = self.hpet.borrow_mut();
        let mut interrupts = self.interrupts.borrow_mut();
        hpet.mmio_write(offset, size, value, &mut *interrupts);
    }
}

struct PciIntxSource {
    bdf: PciBdf,
    pin: PciInterruptPin,
    query_level: Box<dyn Fn(&PcPlatform) -> bool>,
}

pub struct PcPlatform {
    pub chipset: ChipsetState,
    pub io: IoPortBus,
    pub memory: MemoryBus,
    pub interrupts: Rc<RefCell<PlatformInterrupts>>,

    pub pci_cfg: SharedPciConfigPorts,
    pub pci_intx: PciIntxRouter,
    pub acpi_pm: SharedAcpiPmIo<ManualClock>,

    pub hda: Option<Rc<RefCell<HdaPciDevice>>>,
    pub nvme: Option<Rc<RefCell<NvmePciDevice>>>,
    pub ahci: Option<Rc<RefCell<AhciPciDevice>>>,
    pub ide: Option<Rc<RefCell<Piix3IdePciDevice>>>,
    e1000: Option<Rc<RefCell<E1000Device>>>,
    pub uhci: Option<Rc<RefCell<UhciPciDevice>>>,
    pub virtio_blk: Option<Rc<RefCell<VirtioPciDevice>>>,

    pci_intx_sources: Vec<PciIntxSource>,
    pci_allocator: PciResourceAllocator,
    pci_io_bars: SharedPciIoBarMap,

    clock: ManualClock,
    pit: SharedPit8254,
    rtc: SharedRtcCmos<ManualClock, PlatformIrqLine>,
    hpet: Rc<RefCell<hpet::Hpet<ManualClock>>>,
    i8042: I8042Ports,

    uhci_ns_remainder: u64,

    reset_events: Rc<RefCell<Vec<ResetEvent>>>,
}

impl PcPlatform {
    pub fn new(ram_size: usize) -> Self {
        Self::new_with_config(ram_size, PcPlatformConfig::default())
    }

    pub fn new_with_dirty_tracking(ram_size: usize) -> Self {
        Self::new_with_config_dirty_tracking(
            ram_size,
            PcPlatformConfig::default(),
            DEFAULT_DIRTY_PAGE_SIZE,
        )
    }

    pub fn new_with_hda(ram_size: usize) -> Self {
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_hda: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_hda_dirty_tracking(ram_size: usize) -> Self {
        Self::new_with_config_dirty_tracking(
            ram_size,
            PcPlatformConfig {
                enable_hda: true,
                ..Default::default()
            },
            DEFAULT_DIRTY_PAGE_SIZE,
        )
    }

    pub fn new_with_nvme(ram_size: usize) -> Self {
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_nvme: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_ahci(ram_size: usize) -> Self {
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_ahci: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_ahci_dirty_tracking(ram_size: usize) -> Self {
        Self::new_with_config_dirty_tracking(
            ram_size,
            PcPlatformConfig {
                enable_ahci: true,
                ..Default::default()
            },
            DEFAULT_DIRTY_PAGE_SIZE,
        )
    }

    pub fn new_with_ide(ram_size: usize) -> Self {
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_ide: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_ide_dirty_tracking(ram_size: usize) -> Self {
        Self::new_with_config_dirty_tracking(
            ram_size,
            PcPlatformConfig {
                enable_ide: true,
                ..Default::default()
            },
            DEFAULT_DIRTY_PAGE_SIZE,
        )
    }

    /// Convenience constructor for the canonical Windows 7 storage topology:
    /// - AHCI HDD on `00:02.0` (port 0)
    /// - IDE/ATAPI on `00:01.1` (secondary master is typically used for the install ISO)
    ///
    /// See also: `docs/05-storage-topology-win7.md` (canonical PCI BDFs + media attachment mapping).
    pub fn new_with_win7_storage(ram_size: usize) -> Self {
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_ahci: true,
                enable_ide: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_win7_storage_dirty_tracking(ram_size: usize) -> Self {
        Self::new_with_config_dirty_tracking(
            ram_size,
            PcPlatformConfig {
                enable_ahci: true,
                enable_ide: true,
                ..Default::default()
            },
            DEFAULT_DIRTY_PAGE_SIZE,
        )
    }

    pub fn new_with_e1000(ram_size: usize) -> Self {
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_e1000: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_e1000_dirty_tracking(ram_size: usize) -> Self {
        Self::new_with_config_dirty_tracking(
            ram_size,
            PcPlatformConfig {
                enable_e1000: true,
                ..Default::default()
            },
            DEFAULT_DIRTY_PAGE_SIZE,
        )
    }

    pub fn new_with_virtio_blk(ram_size: usize) -> Self {
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_virtio_blk: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_config(ram_size: usize, config: PcPlatformConfig) -> Self {
        let ram = DenseMemory::new(ram_size as u64).expect("failed to allocate guest RAM");
        Self::new_with_config_and_ram(Box::new(ram), config)
    }

    pub fn new_with_config_dirty_tracking(
        ram_size: usize,
        config: PcPlatformConfig,
        page_size: u32,
    ) -> Self {
        let ram = DenseMemory::new(ram_size as u64).expect("failed to allocate guest RAM");
        Self::new_with_config_and_ram_dirty_tracking(Box::new(ram), config, page_size)
    }

    pub fn new_with_config_and_ram(ram: Box<dyn GuestMemory>, config: PcPlatformConfig) -> Self {
        Self::new_with_config_and_ram_inner(ram, config, None)
    }

    pub fn new_with_config_and_ram_dirty_tracking(
        ram: Box<dyn GuestMemory>,
        config: PcPlatformConfig,
        page_size: u32,
    ) -> Self {
        Self::new_with_config_and_ram_inner(ram, config, Some(page_size))
    }

    fn new_with_config_and_ram_inner(
        ram: Box<dyn GuestMemory>,
        config: PcPlatformConfig,
        dirty_page_size: Option<u32>,
    ) -> Self {
        let chipset = ChipsetState::new(false);
        let filter = AddressFilter::new(chipset.a20());

        let mut io = IoPortBus::new();
        let ram_size = ram.size();
        let mut memory = match dirty_page_size {
            Some(page_size) => MemoryBus::with_ram_dirty_tracking(filter, ram, page_size),
            None => MemoryBus::with_ram(filter, ram),
        };

        let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));

        let clock = ManualClock::new();

        let reset_events = Rc::new(RefCell::new(Vec::new()));

        PlatformInterrupts::register_imcr_ports(&mut io, interrupts.clone());
        register_pic8259_on_platform_interrupts(&mut io, interrupts.clone());

        let pit = Rc::new(RefCell::new(Pit8254::new()));
        pit.borrow_mut()
            .connect_irq0_to_platform_interrupts(interrupts.clone());
        register_pit8254(&mut io, pit.clone());

        let rtc_irq8 = PlatformIrqLine::isa(interrupts.clone(), 8);
        let rtc = Rc::new(RefCell::new(RtcCmos::new(clock.clone(), rtc_irq8)));
        rtc.borrow_mut().set_memory_size_bytes(ram_size);
        register_rtc_cmos(&mut io, rtc.clone());

        let i8042_ports = I8042Ports::new();
        i8042_ports.connect_irqs_to_platform_interrupts(interrupts.clone());
        let i8042_ctrl = i8042_ports.controller();

        {
            let reset_events = reset_events.clone();
            let sink =
                i8042::PlatformSystemControlSink::with_reset_sink(chipset.a20(), move |_kind| {
                    reset_events.borrow_mut().push(ResetEvent::System)
                });
            i8042_ctrl
                .borrow_mut()
                .set_system_control_sink(Box::new(sink));
        }
        register_i8042(&mut io, i8042_ctrl.clone());

        io.register(
            0x92,
            Box::new(A20Gate::with_reset_sink(chipset.a20(), {
                let reset_events = reset_events.clone();
                move |_kind| reset_events.borrow_mut().push(ResetEvent::System)
            })),
        );

        let sci_irq = PlatformIrqLine::isa(interrupts.clone(), 9);
        let acpi_pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
            AcpiPmConfig::default(),
            AcpiPmCallbacks {
                sci_irq: Box::new(sci_irq),
                request_power_off: None,
            },
            clock.clone(),
        )));
        aero_devices::acpi_pm::register_acpi_pm(&mut io, acpi_pm.clone());

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        register_pci_config_ports(&mut io, pci_cfg.clone());

        memory
            .map_mmio(
                PCIE_ECAM_BASE,
                PCIE_ECAM_CONFIG.window_size_bytes(),
                Box::new(PciEcamMmio::new(pci_cfg.clone(), PCIE_ECAM_CONFIG)),
            )
            .unwrap();

        // Register Reset Control Register after PCI config ports so it can own port 0xCF9.
        io.register(
            RESET_CTRL_PORT,
            Box::new(ResetCtrl::new({
                let reset_events = reset_events.clone();
                move |kind| {
                    let event = match kind {
                        ResetKind::Cpu => ResetEvent::Cpu,
                        ResetKind::System => ResetEvent::System,
                    };
                    reset_events.borrow_mut().push(event);
                }
            })),
        );

        let pci_intx = PciIntxRouter::new(PciIntxRouterConfig::default());
        let pci_allocator_config = PciResourceAllocatorConfig::default();
        let mut pci_allocator = PciResourceAllocator::new(pci_allocator_config.clone());

        let mut pci_intx_sources: Vec<PciIntxSource> = Vec::new();
        let pci_io_bars: SharedPciIoBarMap = Rc::new(RefCell::new(HashMap::new()));

        let hda = if config.enable_hda {
            let profile = aero_devices::pci::profile::HDA_ICH6;
            let bdf = profile.bdf;

            let hda = Rc::new(RefCell::new(HdaPciDevice::new()));

            {
                let hda_for_intx = hda.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |_pc| hda_for_intx.borrow().irq_level()),
                });
            }

            let mut dev = HdaPciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(hda)
        } else {
            None
        };

        let nvme = if config.enable_nvme {
            let profile = aero_devices::pci::profile::NVME_CONTROLLER;
            let bdf = profile.bdf;

            // Use an `aero-storage` disk image as the backend for the NVMe controller so the same
            // underlying disk abstraction can be reused across controllers (AHCI/NVMe/virtio-blk).
            let disk = RawDisk::create(
                MemBackend::new(),
                1024u64 * SECTOR_SIZE as u64,
            )
            .expect("failed to allocate in-memory NVMe disk");
            let nvme = Rc::new(RefCell::new(
                NvmePciDevice::try_new_from_aero_storage(disk)
                    .expect("in-memory NVMe disk should be 512-byte aligned"),
            ));

            {
                let nvme_for_intx = nvme.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |_pc| nvme_for_intx.borrow().irq_level()),
                });
            }

            let mut dev = NvmePciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(nvme)
        } else {
            None
        };

        let ahci = if config.enable_ahci {
            let profile = aero_devices::pci::profile::SATA_AHCI_ICH9;
            let bdf = profile.bdf;

            let ahci = Rc::new(RefCell::new(AhciPciDevice::new(1)));

            {
                let ahci_for_intx = ahci.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |pc| {
                        let command = {
                            let mut pci_cfg = pc.pci_cfg.borrow_mut();
                            pci_cfg
                                .bus_mut()
                                .device_config(bdf)
                                .map(|cfg| cfg.command())
                                .unwrap_or(0)
                        };

                        // Keep device-side gating consistent when the same device model is also used
                        // outside the platform (e.g. in unit tests).
                        let mut ahci = ahci_for_intx.borrow_mut();
                        ahci.config_mut().set_command(command);

                        ahci.intx_level()
                    }),
                });
            }

            let mut dev = AhciPciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(ahci)
        } else {
            None
        };

        // PIIX3 is a multi-function PCI device. Ensure function 0 exists and has the
        // multi-function bit set so OSes enumerate the IDE/UHCI functions at 00:01.1/00:01.2.
        if config.enable_ide || config.enable_uhci {
            let bdf = aero_devices::pci::profile::ISA_PIIX3.bdf;
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(Piix3IsaPciConfigDevice::new()));
        }

        let uhci = if config.enable_uhci {
            let profile = aero_devices::pci::profile::USB_UHCI_PIIX3;
            let bdf = profile.bdf;

            let uhci = Rc::new(RefCell::new(UhciPciDevice::default()));

            {
                let uhci_for_intx = uhci.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |_pc| uhci_for_intx.borrow().irq_level()),
                });
            }

            let mut dev = UhciPciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(uhci)
        } else {
            None
        };

        let ide = if config.enable_ide {
            let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));

            let profile = aero_devices::pci::profile::IDE_PIIX3;
            let bdf = profile.bdf;

            let mut dev = IdePciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(ide)
        } else {
            None
        };

        let e1000 = if config.enable_e1000 {
            const DEFAULT_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

            let profile = aero_devices::pci::profile::NIC_E1000_82540EM;
            let bdf = profile.bdf;

            let mac = config.mac_addr.unwrap_or(DEFAULT_MAC);
            let e1000 = Rc::new(RefCell::new(E1000Device::new(mac)));

            {
                let e1000_for_intx = e1000.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |_pc| e1000_for_intx.borrow().irq_level()),
                });
            }

            let mut dev = E1000PciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(e1000)
        } else {
            None
        };

        let virtio_blk = if config.enable_virtio_blk {
            let profile = aero_devices::pci::profile::VIRTIO_BLK;
            let bdf = profile.bdf;

            // Use an `aero-storage` disk image so callers can reuse the same disk abstraction across
            // different controllers without bespoke glue.
            let disk = RawDisk::create(MemBackend::new(), (16 * 1024 * 1024) as u64)
                .expect("failed to allocate in-memory virtio-blk disk");
            let backend: Box<dyn VirtualDisk + Send> = Box::new(disk);
            let virtio_blk = Rc::new(RefCell::new(VirtioPciDevice::new(
                Box::new(VirtioBlk::new(backend)),
                Box::new(NoopVirtioInterruptSink),
            )));

            {
                let virtio_for_intx = virtio_blk.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |_pc| virtio_for_intx.borrow().irq_level()),
                });
            }

            let mut dev = VirtioBlkPciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(virtio_blk)
        } else {
            None
        };

        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            bios_post(pci_cfg.bus_mut(), &mut pci_allocator).unwrap();
        }

        // Register IDE legacy I/O ports after BIOS POST so the guest-visible PCI command/BAR
        // state is initialized. Bus Master IDE (BAR4) is routed through the PCI I/O window so BAR
        // relocation is reflected immediately.
        if let Some(ide_dev) = ide.as_ref() {
            let bdf = aero_devices::pci::profile::IDE_PIIX3.bdf;

            // Legacy command/control ports are fixed.
            for port in PRIMARY_PORTS.cmd_base..PRIMARY_PORTS.cmd_base + 8 {
                io.register(
                    port,
                    Box::new(PcIdePort {
                        pci_cfg: pci_cfg.clone(),
                        ide: ide_dev.clone(),
                        bdf,
                        port,
                    }),
                );
            }
            for port in PRIMARY_PORTS.ctrl_base..PRIMARY_PORTS.ctrl_base + 2 {
                io.register(
                    port,
                    Box::new(PcIdePort {
                        pci_cfg: pci_cfg.clone(),
                        ide: ide_dev.clone(),
                        bdf,
                        port,
                    }),
                );
            }
            for port in SECONDARY_PORTS.cmd_base..SECONDARY_PORTS.cmd_base + 8 {
                io.register(
                    port,
                    Box::new(PcIdePort {
                        pci_cfg: pci_cfg.clone(),
                        ide: ide_dev.clone(),
                        bdf,
                        port,
                    }),
                );
            }
            for port in SECONDARY_PORTS.ctrl_base..SECONDARY_PORTS.ctrl_base + 2 {
                io.register(
                    port,
                    Box::new(PcIdePort {
                        pci_cfg: pci_cfg.clone(),
                        ide: ide_dev.clone(),
                        bdf,
                        port,
                    }),
                );
            }

            // Bus Master IDE (BAR4) is a relocatable PCI I/O BAR. Register a single handler keyed
            // by (BDF, BAR4) so the `PciIoWindow` can route accesses after guest reprogramming.
            let prev = pci_io_bars.borrow_mut().insert(
                (bdf, 4),
                Box::new(PcIdeBusMasterBar {
                    pci_cfg: pci_cfg.clone(),
                    ide: ide_dev.clone(),
                    bdf,
                }),
            );
            assert!(
                prev.is_none(),
                "duplicate IDE Bus Master BAR4 handler registration for {bdf:?}"
            );
        }

        // Register UHCI's relocatable BAR4 I/O region through the PCI I/O window dispatcher.
        if let Some(uhci_dev) = uhci.as_ref() {
            let bdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
            let bar = UhciPciDevice::IO_BAR_INDEX;
            let prev = pci_io_bars.borrow_mut().insert(
                (bdf, bar),
                Box::new(PcUhciIoBar {
                    uhci: uhci_dev.clone(),
                }),
            );
            assert!(
                prev.is_none(),
                "duplicate UHCI BAR{bar} handler registration for {bdf:?}"
            );
        }
        // E1000 BAR1 is a relocatable PCI I/O BAR. Register a handler keyed by (BDF, BAR1) so the
        // `PciIoWindow` can route accesses after guest reprogramming.
        if let Some(e1000_dev) = e1000.as_ref() {
            let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
            let prev = pci_io_bars.borrow_mut().insert(
                (bdf, 1),
                Box::new(E1000PciIoBar {
                    e1000: e1000_dev.clone(),
                }),
            );
            assert!(
                prev.is_none(),
                "duplicate E1000 BAR1 handler registration for {bdf:?}"
            );
        }

        // Map the PCI MMIO window used by `PciResourceAllocator` so BAR reprogramming is reflected
        // immediately without needing MMIO unmap/remap support in `MemoryBus`.
        let mut pci_mmio_router =
            PciBarMmioRouter::new(pci_allocator_config.mmio_base, pci_cfg.clone());
        if let Some(hda) = hda.clone() {
            pci_mmio_router.register_shared_handler(
                aero_devices::pci::profile::HDA_ICH6.bdf,
                0,
                hda,
            );
        }
        if let Some(ahci) = ahci.clone() {
            // ICH9 AHCI uses BAR5 (ABAR).
            let bdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
            pci_mmio_router.register_handler(
                bdf,
                5,
                PcAhciMmioBar {
                    pci_cfg: pci_cfg.clone(),
                    ahci,
                    bdf,
                },
            );
        }
        if let Some(e1000) = e1000.clone() {
            pci_mmio_router.register_shared_handler(
                aero_devices::pci::profile::NIC_E1000_82540EM.bdf,
                0,
                e1000,
            );
        }
        if let Some(virtio_blk) = virtio_blk.clone() {
            pci_mmio_router.register_handler(
                aero_devices::pci::profile::VIRTIO_BLK.bdf,
                0,
                VirtioPciBar0Mmio { dev: virtio_blk },
            );
        }
        if let Some(nvme) = nvme.clone() {
            pci_mmio_router.register_shared_handler(
                aero_devices::pci::profile::NVME_CONTROLLER.bdf,
                0,
                nvme,
            );
        }
        memory
            .map_mmio(
                pci_allocator_config.mmio_base,
                pci_allocator_config.mmio_size,
                Box::new(pci_mmio_router),
            )
            .unwrap();

        // Register a dispatcher for the PCI I/O window used by `PciResourceAllocator`. It consults
        // the live PCI bus state on each access, so BAR relocation is immediately reflected without
        // requiring explicit I/O unmap/remap support.
        let io_base = u16::try_from(pci_allocator_config.io_base)
            .expect("PCI IO window base must fit in u16");
        let io_size = u16::try_from(pci_allocator_config.io_size)
            .expect("PCI IO window size must fit in u16");
        io.register_range(
            io_base,
            io_size,
            Box::new(PciIoWindowPort {
                pci_cfg: pci_cfg.clone(),
                handlers: pci_io_bars.clone(),
            }),
        );

        let hpet = Rc::new(RefCell::new(hpet::Hpet::new_default(clock.clone())));

        memory
            .map_mmio(
                LAPIC_MMIO_BASE,
                LAPIC_MMIO_SIZE,
                Box::new(LapicMmio {
                    interrupts: interrupts.clone(),
                }),
            )
            .unwrap();
        memory
            .map_mmio(
                IOAPIC_MMIO_BASE,
                IOAPIC_MMIO_SIZE,
                Box::new(IoApicMmio {
                    interrupts: interrupts.clone(),
                }),
            )
            .unwrap();
        memory
            .map_mmio(
                hpet::HPET_MMIO_BASE,
                hpet::HPET_MMIO_SIZE,
                Box::new(HpetMmio {
                    hpet: hpet.clone(),
                    interrupts: interrupts.clone(),
                }),
            )
            .unwrap();

        Self {
            chipset,
            io,
            memory,
            interrupts,
            pci_cfg,
            pci_intx,
            acpi_pm,
            hda,
            nvme,
            ahci,
            ide,
            e1000,
            uhci,
            virtio_blk,
            pci_intx_sources,
            pci_allocator,
            pci_io_bars,
            clock,
            pit,
            rtc,
            hpet,
            i8042: i8042_ports,
            uhci_ns_remainder: 0,
            reset_events,
        }
    }

    pub fn register_pci_intx_source<F>(&mut self, bdf: PciBdf, pin: PciInterruptPin, query_level: F)
    where
        F: Fn(&PcPlatform) -> bool + 'static,
    {
        self.pci_intx_sources.push(PciIntxSource {
            bdf,
            pin,
            query_level: Box::new(query_level),
        });
    }

    /// Registers a handler for a PCI I/O BAR in the platform's PCI I/O window.
    ///
    /// The handler is keyed by `(bdf, bar_index)` and is dispatched to whenever that BAR currently
    /// decodes the accessed I/O port (respecting PCI command I/O decoding and BAR relocation).
    ///
    /// The `port` argument passed to the handler is the device-relative offset within the BAR
    /// (i.e. `port - bar.base`).
    pub fn register_pci_io_bar(&mut self, bdf: PciBdf, bar: u8, dev: Box<dyn PortIoDevice>) {
        let prev = self.pci_io_bars.borrow_mut().insert((bdf, bar), dev);
        assert!(
            prev.is_none(),
            "duplicate PCI I/O BAR handler registration for {bdf:?} BAR{bar}"
        );
    }

    pub fn i8042_controller(&self) -> SharedI8042Controller {
        self.i8042.controller()
    }

    /// Snapshot/restore + testing hook: returns a clone of the shared PIT (8254) device.
    ///
    /// This accessor exists so external snapshot adapters can read/write the PIT's `IoSnapshot`
    /// state without exposing internal fields publicly. Most users should interact with the PIT
    /// through the platform I/O port bus (`PcPlatform::io`).
    pub fn pit(&self) -> SharedPit8254 {
        self.pit.clone()
    }

    /// Snapshot/restore + testing hook: returns a clone of the shared RTC/CMOS device.
    ///
    /// This accessor exists so external snapshot adapters can read/write the RTC's `IoSnapshot`
    /// state without exposing internal fields publicly. Most users should interact with the RTC
    /// through the platform I/O port bus (`PcPlatform::io`).
    pub fn rtc(&self) -> SharedRtcCmos<ManualClock, PlatformIrqLine> {
        self.rtc.clone()
    }

    /// Snapshot/restore + testing hook: returns a clone of the shared HPET device.
    ///
    /// This accessor exists so external snapshot adapters can read/write the HPET's `IoSnapshot`
    /// state without exposing internal fields publicly. Most users should interact with the HPET
    /// through the platform MMIO bus (`PcPlatform::memory`).
    pub fn hpet(&self) -> Rc<RefCell<hpet::Hpet<ManualClock>>> {
        self.hpet.clone()
    }

    /// Snapshot/restore + testing hook: returns a clone of the platform's deterministic timebase.
    ///
    /// Time-based devices created by [`PcPlatform`] (RTC, HPET, ACPI PM timer, LAPIC timer) are
    /// wired to a shared [`ManualClock`]. Snapshot/restore code can use this handle to coordinate
    /// device restores against the same timebase.
    pub fn clock(&self) -> ManualClock {
        self.clock.clone()
    }

    pub fn has_e1000(&self) -> bool {
        self.e1000.is_some()
    }

    pub fn e1000_mac_addr(&self) -> Option<[u8; 6]> {
        self.e1000.as_ref().map(|e1000| e1000.borrow().mac_addr())
    }

    pub fn e1000_pop_tx_frame(&mut self) -> Option<Vec<u8>> {
        self.e1000
            .as_ref()
            .and_then(|e1000| e1000.borrow_mut().pop_tx_frame())
    }

    pub fn e1000_enqueue_rx_frame(&mut self, frame: Vec<u8>) {
        let Some(e1000) = self.e1000.as_ref() else {
            return;
        };
        e1000.borrow_mut().enqueue_rx_frame(frame);
    }

    pub fn reset_pci(&mut self) {
        let mut pci_cfg = self.pci_cfg.borrow_mut();
        bios_post(pci_cfg.bus_mut(), &mut self.pci_allocator).unwrap();
    }

    pub fn attach_ahci_drive_port0(&mut self, drive: AtaDrive) {
        self.attach_ahci_drive(0, drive);
    }

    pub fn attach_ahci_disk(
        &mut self,
        port: usize,
        disk: Box<dyn VirtualDisk>,
    ) -> std::io::Result<()> {
        self.attach_ahci_drive(port, AtaDrive::new(disk)?);
        Ok(())
    }

    pub fn attach_ahci_disk_port0(&mut self, disk: Box<dyn VirtualDisk>) -> std::io::Result<()> {
        self.attach_ahci_disk(0, disk)
    }

    pub fn attach_ide_primary_master_drive(&mut self, drive: AtaDrive) {
        self.ide
            .as_ref()
            .expect("IDE controller is not enabled on this PcPlatform")
            .borrow_mut()
            .controller
            .attach_primary_master_ata(drive);
    }

    pub fn attach_ide_primary_master_disk(
        &mut self,
        disk: Box<dyn VirtualDisk>,
    ) -> std::io::Result<()> {
        self.attach_ide_primary_master_drive(AtaDrive::new(disk)?);
        Ok(())
    }

    pub fn attach_ide_secondary_master_atapi(&mut self, dev: AtapiCdrom) {
        self.ide
            .as_ref()
            .expect("IDE controller is not enabled on this PcPlatform")
            .borrow_mut()
            .controller
            .attach_secondary_master_atapi(dev);
    }

    pub fn attach_ide_secondary_master_iso(
        &mut self,
        disk: Box<dyn VirtualDisk + Send>,
    ) -> std::io::Result<()> {
        self.attach_ide_secondary_master_atapi(AtapiCdrom::new_from_virtual_disk(disk)?);
        Ok(())
    }

    pub fn process_hda(&mut self, output_frames: usize) {
        let Some(hda) = self.hda.as_ref() else {
            return;
        };

        // Only allow the device to DMA when Bus Mastering is enabled (PCI command bit 2).
        let bdf = aero_devices::pci::profile::HDA_ICH6.bdf;
        let bus_master_enabled = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .is_some_and(|cfg| (cfg.command() & (1 << 2)) != 0)
        };
        if !bus_master_enabled {
            return;
        };

        let mut hda = hda.borrow_mut();
        let mut mem = HdaDmaMemory {
            mem: RefCell::new(&mut self.memory),
        };
        hda.controller_mut().process(&mut mem, output_frames);
    }

    pub fn process_nvme(&mut self) {
        let Some(nvme) = self.nvme.as_ref() else {
            return;
        };

        // Only allow the device to DMA when Bus Mastering is enabled (PCI command bit 2).
        let bdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
        let bus_master_enabled = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .is_some_and(|cfg| (cfg.command() & (1 << 2)) != 0)
        };
        if !bus_master_enabled {
            return;
        };

        let mut nvme = nvme.borrow_mut();
        nvme.controller_mut().process(&mut self.memory);
    }

    pub fn process_ahci(&mut self) {
        let Some(ahci) = self.ahci.as_ref() else {
            return;
        };

        let bdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
        let command = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .map(|cfg| cfg.command())
                .unwrap_or(0)
        };

        // Keep the device's internal view of the PCI command register in sync so it can apply
        // Bus Master and INTx disable gating when used standalone.
        {
            let mut ahci = ahci.borrow_mut();
            ahci.config_mut().set_command(command);
        }

        // Only allow the device to DMA when Bus Mastering is enabled (PCI command bit 2).
        let bus_master_enabled = (command & (1 << 2)) != 0;
        if !bus_master_enabled {
            return;
        }

        let mut ahci = ahci.borrow_mut();
        ahci.process(&mut self.memory);
    }

    pub fn process_ide(&mut self) {
        let Some(ide) = self.ide.as_ref() else {
            return;
        };

        let bdf = aero_devices::pci::profile::IDE_PIIX3.bdf;
        let (command, bar4_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar4 = cfg
                .and_then(|cfg| cfg.bar_range(4))
                .map(|range| range.base)
                .unwrap_or(0);
            (command, bar4)
        };

        {
            let mut ide = ide.borrow_mut();
            ide.config_mut().set_command(command);
            if bar4_base != 0 {
                ide.config_mut().set_bar_base(4, bar4_base);
            }
        }

        let mut ide = ide.borrow_mut();
        ide.tick(&mut self.memory);
    }

    pub fn process_virtio_blk(&mut self) {
        let Some(virtio_blk) = self.virtio_blk.as_ref() else {
            return;
        };

        let bdf = aero_devices::pci::profile::VIRTIO_BLK.bdf;
        let bus_master_enabled = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .is_some_and(|cfg| (cfg.command() & (1 << 2)) != 0)
        };
        if !bus_master_enabled {
            return;
        }

        let mut virtio_blk = virtio_blk.borrow_mut();
        let mut mem = VirtioDmaMemory {
            mem: &mut self.memory,
        };
        virtio_blk.process_notified_queues(&mut mem);
    }

    pub fn attach_ahci_drive(&mut self, port: usize, drive: AtaDrive) {
        let Some(ahci) = self.ahci.as_ref() else {
            return;
        };
        ahci.borrow_mut().attach_drive(port, drive);
    }

    pub fn detach_ahci_drive(&mut self, port: usize) {
        let Some(ahci) = self.ahci.as_ref() else {
            return;
        };
        ahci.borrow_mut().detach_drive(port);
    }

    pub fn process_e1000(&mut self) {
        let Some(e1000) = self.e1000.as_ref() else {
            return;
        };

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let (command, bar0_base, bar1_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar0_base = cfg
                .and_then(|cfg| cfg.bar_range(0))
                .map(|range| range.base)
                .unwrap_or(0);
            let bar1_base = cfg
                .and_then(|cfg| cfg.bar_range(1))
                .map(|range| range.base)
                .unwrap_or(0);
            (command, bar0_base, bar1_base)
        };

        // Keep the device model's internal PCI config state in sync with the platform PCI bus.
        //
        // The E1000 model gates DMA on COMMAND.BME (bit 2) by consulting its own PCI config state,
        // while the PC platform maintains a separate canonical config space for enumeration.
        // Mirror the live config (command + BAR bases) into the NIC model before polling so
        // bus-master gating works without needing a general "config write hook".
        {
            let mut e1000 = e1000.borrow_mut();
            e1000.pci_config_write(0x04, 2, u32::from(command));
            if let Ok(bar0_base) = u32::try_from(bar0_base) {
                if bar0_base != 0 {
                    e1000.pci_config_write(0x10, 4, bar0_base);
                }
            }
            if let Ok(bar1_base) = u32::try_from(bar1_base) {
                if bar1_base != 0 {
                    e1000.pci_config_write(0x14, 4, bar1_base);
                }
            }
        }

        // Only allow the device to DMA when Bus Mastering is enabled (PCI command bit 2).
        let bus_master_enabled = (command & (1 << 2)) != 0;
        {
            // Keep the E1000 model's internal PCI command register in sync with the platform's PCI
            // config space. The device model gates DMA on its own `pci` state.
            let mut dev = e1000.borrow_mut();
            dev.pci.write(0x04, 2, command as u32);
            if bus_master_enabled {
                dev.poll(&mut self.memory);
            }
        }
    }

    pub fn poll_pci_intx_lines(&mut self) {
        for src in &self.pci_intx_sources {
            let mut level = (src.query_level)(self);

            // Respect PCI command register Interrupt Disable bit (bit 10). When set, the device must
            // not assert INTx.
            //
            // This is important for guests that switch to MSI/MSI-X and disable legacy INTx.
            let intx_disabled = {
                let mut pci_cfg = self.pci_cfg.borrow_mut();
                match pci_cfg.bus_mut().device_config(src.bdf) {
                    Some(cfg) => (cfg.command() & (1 << 10)) != 0,
                    None => {
                        // The source is registered but its config-space function is not present;
                        // treat it as disabled so we don't deliver interrupts for a missing device.
                        true
                    }
                }
            };
            if intx_disabled {
                level = false;
            }

            self.pci_intx.set_intx_level(
                src.bdf,
                src.pin,
                level,
                &mut *self.interrupts.borrow_mut(),
            );
        }

        if let Some(ide) = self.ide.as_ref() {
            // IDE legacy mode uses ISA IRQ14/IRQ15 rather than PCI INTx.
            let (irq14, irq15) = {
                let ide = ide.borrow();
                (
                    ide.controller.primary_irq_pending(),
                    ide.controller.secondary_irq_pending(),
                )
            };
            let mut interrupts = self.interrupts.borrow_mut();
            if irq14 {
                interrupts.raise_irq(InterruptInput::IsaIrq(14));
            } else {
                interrupts.lower_irq(InterruptInput::IsaIrq(14));
            }
            if irq15 {
                interrupts.raise_irq(InterruptInput::IsaIrq(15));
            } else {
                interrupts.lower_irq(InterruptInput::IsaIrq(15));
            }
        }
    }

    /// Re-drives any currently asserted PCI INTx lines into the platform interrupt controller.
    ///
    /// This is mainly intended for snapshot restore flows: restoring a `PciIntxRouter` updates its
    /// internal source/refcount bookkeeping, but it cannot directly reassert the corresponding
    /// platform GSIs because it has no access to the `PlatformInterrupts` sink during
    /// `IoSnapshot::load_state()`.
    ///
    /// Call this after restoring both the `PciIntxRouter` and `PlatformInterrupts` to ensure the
    /// interrupt controller sees the restored INTx levels.
    pub fn sync_pci_intx_levels_to_interrupts(&mut self) {
        self.pci_intx
            .sync_levels_to_sink(&mut *self.interrupts.borrow_mut());
    }

    pub fn tick(&mut self, delta_ns: u64) {
        self.clock.advance_ns(delta_ns);
        self.acpi_pm.borrow_mut().advance_ns(delta_ns);
        self.pit.borrow_mut().advance_ns(delta_ns);
        self.rtc.borrow_mut().tick();

        // Keep the LAPIC timer deterministic: advance time only via `tick`.
        self.interrupts.borrow().tick(delta_ns);

        {
            let mut hpet = self.hpet.borrow_mut();
            let mut interrupts = self.interrupts.borrow_mut();
            hpet.poll(&mut *interrupts);
        }

        if let Some(uhci) = self.uhci.as_ref() {
            const NS_PER_MS: u64 = 1_000_000;
            let bdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
            let (command, bar4_base) = {
                let mut pci_cfg = self.pci_cfg.borrow_mut();
                let cfg = pci_cfg.bus_mut().device_config(bdf);
                let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                let bar4_base = cfg
                    .and_then(|cfg| cfg.bar_range(UhciPciDevice::IO_BAR_INDEX))
                    .map(|range| range.base)
                    .unwrap_or(0);
                (command, bar4_base)
            };

            // Keep the UHCI model's view of PCI config state in sync so it can apply bus mastering
            // gating when used via `tick_1ms`.
            let mut uhci = uhci.borrow_mut();
            uhci.config_mut().set_command(command);
            if bar4_base != 0 {
                uhci.config_mut()
                    .set_bar_base(UhciPciDevice::IO_BAR_INDEX, bar4_base);
            }

            self.uhci_ns_remainder = self.uhci_ns_remainder.saturating_add(delta_ns);
            let mut ticks = self.uhci_ns_remainder / NS_PER_MS;
            self.uhci_ns_remainder %= NS_PER_MS;

            while ticks != 0 {
                uhci.tick_1ms(&mut self.memory);
                ticks -= 1;
            }
        }
    }

    pub fn take_reset_events(&mut self) -> Vec<ResetEvent> {
        std::mem::take(&mut *self.reset_events.borrow_mut())
    }
}
