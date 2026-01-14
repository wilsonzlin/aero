#![forbid(unsafe_code)]

#[cfg(feature = "hda")]
use aero_audio::hda_pci::HdaPciDevice;
use aero_devices::a20_gate::{A20Gate, A20_GATE_PORT};
use aero_devices::acpi_pm::{
    AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, AcpiSleepState, SharedAcpiPmIo,
};
use aero_devices::clock::ManualClock;
use aero_devices::dma::{register_dma8237, Dma8237};
use aero_devices::i8042::{register_i8042, I8042Ports, SharedI8042Controller};
use aero_devices::irq::{IrqLine, PlatformIrqLine};
use aero_devices::pci::profile::{AHCI_ABAR_BAR_INDEX, AHCI_ABAR_SIZE_U32};
use aero_devices::pci::{
    bios_post, msix::PCI_CAP_ID_MSIX, register_pci_config_ports, MsiCapability, MsixCapability,
    PciBarDefinition, PciBdf, PciConfigPorts, PciDevice, PciEcamConfig, PciEcamMmio,
    PciInterruptPin, PciIntxRouter, PciIntxRouterConfig, PciResourceAllocator,
    PciResourceAllocatorConfig, SharedPciConfigPorts,
};
use aero_devices::pic8259::register_pic8259_on_platform_interrupts;
use aero_devices::pit8254::{register_pit8254, Pit8254, SharedPit8254};
use aero_devices::reset_ctrl::{ResetCtrl, ResetKind, RESET_CTRL_PORT};
use aero_devices::rtc_cmos::{register_rtc_cmos, RtcCmos, SharedRtcCmos};
use aero_devices::usb::ehci::EhciPciDevice;
use aero_devices::usb::uhci::UhciPciDevice;
use aero_devices::usb::xhci::XhciPciDevice;
use aero_devices::{hpet, i8042};
use aero_devices_nvme::{NvmeController, NvmePciDevice};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::atapi::AtapiCdrom;
use aero_devices_storage::pci_ide::{Piix3IdePciDevice, PRIMARY_PORTS, SECONDARY_PORTS};
use aero_devices_storage::AhciPciDevice;
use aero_interrupts::apic::{IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, LAPIC_MMIO_BASE, LAPIC_MMIO_SIZE};
use aero_net_e1000::E1000Device;
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::ChipsetState;
use aero_platform::dirty_memory::DEFAULT_DIRTY_PAGE_SIZE;
use aero_platform::interrupts::mmio::{IoApicMmio, LapicMmio};
use aero_platform::interrupts::msi::{MsiMessage, MsiTrigger};
use aero_platform::interrupts::PlatformInterrupts;
use aero_platform::io::{IoPortBus, PortIoDevice};
use aero_platform::memory::MemoryBus;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use aero_virtio::devices::blk::VirtioBlk;
use aero_virtio::memory::{
    GuestMemory as VirtioGuestMemory, GuestMemoryError as VirtioGuestMemoryError,
};
use aero_virtio::pci::{InterruptSink as VirtioInterruptSink, VirtioPciDevice};
use memory::{DenseMemory, GuestMemory, GuestMemoryResult, MmioHandler, PhysicalMemoryBus};
use std::cell::RefCell;
use std::rc::Rc;

// `VirtualDisk` is conditionally `Send` via `aero_storage::VirtualDiskSend`:
// - native: `dyn VirtualDisk` is `Send`
// - wasm32: `dyn VirtualDisk` may be `!Send` (OPFS/JS-backed handles, etc.)
type NvmeDisk = Box<dyn VirtualDisk>;
type VirtioBlkDisk = Box<dyn VirtualDisk>;

/// Cloneable [`GuestMemory`] wrapper used to share a single RAM backing store across multiple
/// memory buses.
///
/// `PcPlatform` primarily exposes an [`aero_platform::memory::MemoryBus`] that owns a
/// `memory::PhysicalMemoryBus` (RAM + ROM + MMIO). Some device models (notably xHCI) perform DMA as
/// an immediate side effect of MMIO writes. Because the MMIO handler trait does not supply a handle
/// to the platform memory bus, those devices accept an optional independent DMA bus via
/// `set_dma_memory_bus`.
///
/// To keep DMA accesses coherent with the main platform memory bus, we wrap the RAM backend in an
/// `Rc<RefCell<_>>` and instantiate a minimal `PhysicalMemoryBus` for device-local DMA that shares
/// the same underlying storage.
///
/// Note: this wrapper intentionally does not expose `get_slice` fast paths (it always returns
/// `None`) because returning references into a `RefCell`-borrowed inner buffer is not sound.
#[derive(Clone)]
struct SharedGuestMemory {
    inner: Rc<RefCell<Box<dyn GuestMemory>>>,
}

impl SharedGuestMemory {
    fn new(inner: Box<dyn GuestMemory>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(inner)),
        }
    }
}

impl GuestMemory for SharedGuestMemory {
    fn size(&self) -> u64 {
        self.inner.borrow().size()
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        self.inner.borrow().read_into(paddr, dst)
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        self.inner.borrow_mut().write_from(paddr, src)
    }

    fn get_slice(&self, _paddr: u64, _len: usize) -> Option<&[u8]> {
        None
    }

    fn get_slice_mut(&mut self, _paddr: u64, _len: usize) -> Option<&mut [u8]> {
        None
    }
}

mod cpu_core;
pub use aero_devices::pci::{PciBarMmioHandler, PciBarMmioRouter, PciConfigSyncedMmioBar};
pub use aero_devices::pci::{PciIoBarHandler, PciIoBarRouter};
pub use cpu_core::{PcCpuBus, PcInterruptController};

mod firmware_pci;
pub use firmware_pci::{PciConfigPortsBiosAdapter, SharedPciConfigPortsBiosAdapter};

mod windows7_storage;
pub use windows7_storage::Windows7StorageTopologyConfig;

mod snapshot_harness;
pub use snapshot_harness::PcPlatformSnapshotHarness;

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
    /// The guest requested S5 (soft-off) via ACPI PM1a_CNT.
    PowerOff,
}

#[derive(Debug, Clone, Copy)]
pub struct PcPlatformConfig {
    /// Number of virtual CPUs exposed by the platform.
    ///
    /// This value is currently used to size per-vCPU platform infrastructure like the interrupt
    /// controller complex (LAPIC state). The `PcPlatform` integration remains single-threaded; a
    /// `cpu_count > 1` primarily exists so firmware can publish SMP-capable ACPI/SMBIOS tables.
    pub cpu_count: u8,
    /// Enable the Intel HDA controller (ICH6 model).
    ///
    /// Note: the actual HDA device integration is feature-gated behind the crate feature `hda`.
    /// When `hda` is disabled, this flag is ignored (no HDA device will be instantiated).
    pub enable_hda: bool,
    pub enable_nvme: bool,
    pub enable_ahci: bool,
    pub enable_ide: bool,
    pub enable_e1000: bool,
    pub mac_addr: Option<[u8; 6]>,
    pub enable_uhci: bool,
    /// Enable a USB 2.0 EHCI controller (MMIO + INTx).
    ///
    /// Default is `false` to avoid changing the canonical platform topology until EHCI is used by
    /// higher-level integrations.
    pub enable_ehci: bool,
    /// Enable an xHCI (USB 3.0) controller exposed as a PCI MMIO device.
    ///
    /// Disabled by default to avoid changing guest expectations for the canonical platform.
    pub enable_xhci: bool,
    pub enable_virtio_blk: bool,
    /// Expose MSI-X in virtio PCI config space and route virtio interrupts as MSI.
    ///
    /// When disabled, virtio devices continue to operate in INTx-only mode using the platform's
    /// INTx polling loop.
    pub enable_virtio_msix: bool,
}

impl Default for PcPlatformConfig {
    fn default() -> Self {
        Self {
            cpu_count: 1,
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
            enable_ehci: false,
            enable_xhci: false,
            enable_virtio_blk: false,
            enable_virtio_msix: true,
        }
    }
}

#[cfg(feature = "hda")]
struct HdaPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

#[cfg(feature = "hda")]
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

#[cfg(feature = "hda")]
impl PciDevice for HdaPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

struct AhciPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl AhciPciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::SATA_AHCI_ICH9.build_config_space();
        config.set_bar_definition(
            AHCI_ABAR_BAR_INDEX,
            PciBarDefinition::Mmio32 {
                size: AHCI_ABAR_SIZE_U32,
                prefetchable: false,
            },
        );
        // Expose MSI in the guest-visible PCI function config space.
        //
        // The canonical PCI profile (`SATA_AHCI_ICH9`) declares MSI, but keep this defensive so the
        // platform doesn't accidentally regress if profiles are customized.
        if config.capability::<MsiCapability>().is_none() {
            config.add_capability(Box::new(MsiCapability::new()));
        }
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

    fn reset(&mut self) {
        *self = Self::new();
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

    fn reset(&mut self) {
        *self = Self::new();
    }
}

struct VirtioBlkPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
    enable_msix: bool,
}

impl VirtioBlkPciConfigDevice {
    fn new(enable_msix: bool) -> Self {
        // The upstream virtio PCI profiles include MSI-X by default, but the PC platform keeps
        // virtio MSI-X behind a runtime config knob (`PcPlatformConfig::enable_virtio_msix`) so
        // existing INTx-only integrations can remain stable.

        let base_profile = aero_devices::pci::profile::VIRTIO_BLK;
        let profile = if enable_msix {
            base_profile
        } else {
            aero_devices::pci::profile::PciDeviceProfile {
                capabilities: &aero_devices::pci::profile::VIRTIO_VENDOR_CAPS,
                ..base_profile
            }
        };

        let config = profile.build_config_space();
        Self {
            config,
            enable_msix,
        }
    }
}

impl PciDevice for VirtioBlkPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        *self = Self::new(self.enable_msix);
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

    fn reset(&mut self) {
        *self = Self::new();
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

    fn reset(&mut self) {
        *self = Self::new();
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

    fn reset(&mut self) {
        *self = Self::new();
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

    fn reset(&mut self) {
        // Restore the PCI config space back to its profiled power-on state.
        //
        // This ensures guest writes to writable config registers (e.g. MSI, interrupt line, BAR
        // probe state) do not persist across a platform reset.
        *self = Self::new();
    }
}

struct EhciPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl EhciPciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::USB_EHCI_ICH9.build_config_space();
        config.set_bar_definition(
            EhciPciDevice::MMIO_BAR_INDEX,
            PciBarDefinition::Mmio32 {
                size: EhciPciDevice::MMIO_BAR_SIZE,
                prefetchable: false,
            },
        );
        Self { config }
    }
}

impl PciDevice for EhciPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        *self = Self::new();
    }
}

struct XhciPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl XhciPciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::USB_XHCI_QEMU.build_config_space();
        // Expose a single-vector MSI capability so guests can opt into message-signaled interrupts
        // via the canonical PCI config mechanism (#1).
        if config.capability::<MsiCapability>().is_none() {
            config.add_capability(Box::new(MsiCapability::new()));
        }
        // Backward compatibility: older profiles may omit MSI-X. Ensure we expose the canonical
        // BAR0-backed MSI-X table/PBA so modern guests can prefer MSI-X over MSI/INTx.
        if config.capability::<MsixCapability>().is_none() {
            config.add_capability(Box::new(MsixCapability::new(
                aero_devices::pci::profile::XHCI_MSIX_TABLE_SIZE,
                aero_devices::pci::profile::XHCI_MSIX_TABLE_BAR,
                aero_devices::pci::profile::XHCI_MSIX_TABLE_OFFSET,
                aero_devices::pci::profile::XHCI_MSIX_PBA_BAR,
                aero_devices::pci::profile::XHCI_MSIX_PBA_OFFSET,
            )));
        }
        config.set_bar_definition(
            XhciPciDevice::MMIO_BAR_INDEX,
            PciBarDefinition::Mmio32 {
                size: XhciPciDevice::MMIO_BAR_SIZE,
                prefetchable: false,
            },
        );
        Self { config }
    }
}

impl PciDevice for XhciPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        *self = Self::new();
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
            let bar4_base = cfg.and_then(|cfg| cfg.bar_range(4)).map(|range| range.base);
            (command, bar4_base)
        };

        let mut ide = self.ide.borrow_mut();
        ide.config_mut().set_command(command);
        if let Some(bar4_base) = bar4_base {
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

/// Bus Master IDE (BAR4) handler registered with the platform's [`PciIoBarRouter`].
#[derive(Clone)]
struct PcIdeBusMasterBar {
    pci_cfg: SharedPciConfigPorts,
    ide: Rc<RefCell<Piix3IdePciDevice>>,
    bdf: PciBdf,
}

impl PcIdeBusMasterBar {
    fn read_all_ones(size: usize) -> u32 {
        match size {
            0 => 0,
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }

    fn sync_config(&self) {
        let (command, bar4_base) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let bus = pci_cfg.bus_mut();
            let cfg = bus.device_config(self.bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar4_base = cfg.and_then(|cfg| cfg.bar_range(4)).map(|range| range.base);
            (command, bar4_base)
        };

        let mut ide = self.ide.borrow_mut();
        ide.config_mut().set_command(command);
        if let Some(bar4_base) = bar4_base {
            ide.config_mut().set_bar_base(4, bar4_base);
        }
    }
}

impl PciIoBarHandler for PcIdeBusMasterBar {
    fn io_read(&mut self, offset: u64, size: usize) -> u32 {
        let size_u8 = match size {
            1 | 2 | 4 => size as u8,
            _ => return Self::read_all_ones(size),
        };
        let Ok(offset_u16) = u16::try_from(offset) else {
            return Self::read_all_ones(size);
        };

        self.sync_config();
        let base = { self.ide.borrow().bus_master_base() };
        let abs_port = base.wrapping_add(offset_u16);
        self.ide.borrow_mut().io_read(abs_port, size_u8)
    }

    fn io_write(&mut self, offset: u64, size: usize, value: u32) {
        let size_u8 = match size {
            1 | 2 | 4 => size as u8,
            _ => return,
        };
        let Ok(offset_u16) = u16::try_from(offset) else {
            return;
        };

        self.sync_config();
        let base = { self.ide.borrow().bus_master_base() };
        let abs_port = base.wrapping_add(offset_u16);
        self.ide.borrow_mut().io_write(abs_port, size_u8, value);
    }
}

/// UHCI (BAR4) I/O handler registered with the platform's [`PciIoBarRouter`].
#[derive(Clone)]
struct PcUhciIoBar {
    uhci: Rc<RefCell<UhciPciDevice>>,
}

impl PcUhciIoBar {
    fn read_all_ones(size: usize) -> u32 {
        match size {
            0 => 0,
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl PciIoBarHandler for PcUhciIoBar {
    fn io_read(&mut self, offset: u64, size: usize) -> u32 {
        let size = match size {
            1 | 2 | 4 => size,
            _ => return Self::read_all_ones(size),
        };
        let Ok(offset_u16) = u16::try_from(offset) else {
            return Self::read_all_ones(size);
        };
        self.uhci
            .borrow_mut()
            .controller_mut()
            .io_read(offset_u16, size)
    }

    fn io_write(&mut self, offset: u64, size: usize, value: u32) {
        let size = match size {
            1 | 2 | 4 => size,
            _ => return,
        };
        let Ok(offset_u16) = u16::try_from(offset) else {
            return;
        };
        self.uhci
            .borrow_mut()
            .controller_mut()
            .io_write(offset_u16, size, value);
    }
}

#[derive(Clone)]
struct E1000PciIoBar {
    e1000: Rc<RefCell<E1000Device>>,
}

impl E1000PciIoBar {
    fn read_all_ones(size: usize) -> u32 {
        match size {
            0 => 0,
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl PciIoBarHandler for E1000PciIoBar {
    fn io_read(&mut self, offset: u64, size: usize) -> u32 {
        let size = match size {
            1 | 2 | 4 => size,
            _ => return Self::read_all_ones(size),
        };
        let Ok(offset_u32) = u32::try_from(offset) else {
            return Self::read_all_ones(size);
        };

        self.e1000.borrow_mut().io_read(offset_u32, size)
    }

    fn io_write(&mut self, offset: u64, size: usize, value: u32) {
        let size = match size {
            1 | 2 | 4 => size,
            _ => return,
        };
        let Ok(offset_u32) = u32::try_from(offset) else {
            return;
        };

        self.e1000
            .borrow_mut()
            .io_write_reg(offset_u32, size, value);
    }
}

type SharedPciBarMmioRouter = Rc<RefCell<PciBarMmioRouter>>;
type SharedPciIoBarRouter = Rc<RefCell<PciIoBarRouter>>;

#[derive(Clone)]
struct SharedPciBarMmioRouterMmio {
    router: SharedPciBarMmioRouter,
}

impl MmioHandler for SharedPciBarMmioRouterMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let mut router = self.router.borrow_mut();
        MmioHandler::read(&mut *router, offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let mut router = self.router.borrow_mut();
        MmioHandler::write(&mut *router, offset, size, value);
    }
}

#[derive(Clone)]
struct PciIoBarRouterPort {
    router: SharedPciIoBarRouter,
}

impl PciIoBarRouterPort {
    fn read_all_ones(size: u8) -> u32 {
        match size {
            0 => 0,
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl PortIoDevice for PciIoBarRouterPort {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let mask = Self::read_all_ones(size);
        let size_usize = match size {
            1 | 2 | 4 => usize::from(size),
            _ => return mask,
        };

        self.router
            .borrow_mut()
            .dispatch_read(port, size_usize)
            .map(|v| v & mask)
            .unwrap_or(mask)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        let size_usize = match size {
            1 | 2 | 4 => usize::from(size),
            _ => return,
        };

        let _ = self
            .router
            .borrow_mut()
            .dispatch_write(port, size_usize, value);
    }
}

#[cfg(feature = "hda")]
struct HdaDmaMemory<'a> {
    mem: RefCell<&'a mut MemoryBus>,
}

#[cfg(feature = "hda")]
impl aero_audio::mem::MemoryAccess for HdaDmaMemory<'_> {
    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        self.mem.borrow_mut().read_physical(addr, buf);
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        self.mem.borrow_mut().write_physical(addr, buf);
    }
}

#[derive(Clone)]
struct VirtioPlatformInterruptSink {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl VirtioInterruptSink for VirtioPlatformInterruptSink {
    fn raise_legacy_irq(&mut self) {
        // INTx delivery for virtio devices is driven by the platform's polling loop.
    }

    fn lower_legacy_irq(&mut self) {
        // INTx delivery for virtio devices is driven by the platform's polling loop.
    }

    fn signal_msix(&mut self, message: MsiMessage) {
        self.interrupts.borrow_mut().trigger_msi(message);
    }
}

fn all_ones(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}

struct VirtioPciBar0Mmio {
    pci_cfg: SharedPciConfigPorts,
    bdf: PciBdf,
    dev: Rc<RefCell<VirtioPciDevice>>,
}

impl VirtioPciBar0Mmio {
    fn sync_pci_config(&mut self) {
        let (command, msix_enabled, msix_masked) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            match pci_cfg.bus_mut().device_config(self.bdf) {
                Some(cfg) => {
                    let msix = cfg.capability::<MsixCapability>();
                    (
                        cfg.command(),
                        msix.is_some_and(|msix| msix.enabled()),
                        msix.is_some_and(|msix| msix.function_masked()),
                    )
                }
                None => (0, false, false),
            }
        };

        let mut dev = self.dev.borrow_mut();
        dev.set_pci_command(command);

        // Keep MSI-X enable + function mask bits coherent between the canonical PCI config space
        // owned by the PC platform and the virtio transport's internal PCI config model.
        //
        // This is required because virtio-pci programs per-queue MSI-X vectors via BAR0 common
        // config, and those writes are gated on `msix.enabled()`.
        sync_virtio_msix_from_platform(&mut dev, msix_enabled, msix_masked);
    }
}

impl PciBarMmioHandler for VirtioPciBar0Mmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.sync_pci_config();
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
        self.sync_pci_config();
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

fn sync_virtio_msix_from_platform(dev: &mut VirtioPciDevice, enabled: bool, function_masked: bool) {
    let Some(off) = dev.config_mut().find_capability(PCI_CAP_ID_MSIX) else {
        return;
    };

    // Preserve the read-only table size bits and only synchronize the guest-writable enable/mask
    // bits.
    let ctrl = dev.config_mut().read(u16::from(off) + 0x02, 2) as u16;
    let mut new_ctrl = ctrl & !((1 << 15) | (1 << 14));
    if enabled {
        new_ctrl |= 1 << 15;
    }
    if function_masked {
        new_ctrl |= 1 << 14;
    }
    if new_ctrl != ctrl {
        // Route this through `VirtioPciDevice::config_write` instead of writing directly to the
        // underlying `PciConfigSpace` so we preserve virtio-transport side effects:
        // - INTx gating when MSI-X is toggled
        // - pending MSI-X vector redelivery when Function Mask is cleared
        dev.config_write(u16::from(off) + 0x02, &new_ctrl.to_le_bytes());
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
}

struct HpetMmio {
    hpet: Rc<RefCell<hpet::Hpet<ManualClock>>>,
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl MmioHandler for HpetMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return 0;
        }
        let mut hpet = self.hpet.borrow_mut();
        let mut interrupts = self.interrupts.borrow_mut();
        hpet.mmio_read(offset, size, &mut *interrupts)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return;
        }
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

fn sync_msi_capability_into_config(
    cfg: &mut aero_devices::pci::PciConfigSpace,
    enabled: bool,
    addr: u64,
    data: u16,
    mask: u32,
) {
    let Some(off) = cfg.find_capability(aero_devices::pci::msi::PCI_CAP_ID_MSI) else {
        return;
    };
    let base = u16::from(off);

    // Preserve read-only capability bits (64-bit + per-vector masking) by only mutating the MSI
    // Enable bit in Message Control.
    let ctrl = cfg.read(base + 0x02, 2) as u16;
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;

    cfg.write(base + 0x04, 4, addr as u32);
    if is_64bit {
        cfg.write(base + 0x08, 4, (addr >> 32) as u32);
        cfg.write(base + 0x0c, 2, u32::from(data));
        if per_vector_masking {
            cfg.write(base + 0x10, 4, mask);
        }
    } else {
        cfg.write(base + 0x08, 2, u32::from(data));
        if per_vector_masking {
            cfg.write(base + 0x0c, 4, mask);
        }
    }

    let new_ctrl = if enabled {
        ctrl | 0x0001
    } else {
        ctrl & !0x0001
    };
    cfg.write(base + 0x02, 2, u32::from(new_ctrl));
}

fn sync_msix_capability_into_config(
    cfg: &mut aero_devices::pci::PciConfigSpace,
    enabled: bool,
    function_masked: bool,
) {
    let Some(off) = cfg.find_capability(PCI_CAP_ID_MSIX) else {
        return;
    };
    let base = u16::from(off);
    let ctrl = cfg.read(base + 0x02, 2) as u16;
    let mut new_ctrl = ctrl;
    if enabled {
        new_ctrl |= 1 << 15;
    } else {
        new_ctrl &= !(1 << 15);
    }
    if function_masked {
        new_ctrl |= 1 << 14;
    } else {
        new_ctrl &= !(1 << 14);
    }
    cfg.write(base + 0x02, 2, u32::from(new_ctrl));
}

/// Canonical PC/Q35 platform wiring: chipset + MMIO + port I/O + PCI + interrupt controllers + timers.
///
/// ## Current limitation: BSP-only execution (no SMP scheduling yet)
///
/// `PcPlatform` can be configured with `cpu_count > 1` to size per-vCPU platform state (for example,
/// LAPIC instances so firmware can publish SMP-capable ACPI/SMBIOS tables), but the canonical
/// integrations are still single-threaded and do not yet execute/schedule APs end-to-end.
///
/// See `docs/21-smp.md` for the SMP bring-up plan/progress tracker.
pub struct PcPlatform {
    pub chipset: ChipsetState,
    pub io: IoPortBus,
    pub memory: MemoryBus,
    /// Total guest RAM bytes in the dense backing store (excluding any below-4GiB MMIO holes when
    /// RAM is remapped above 4GiB).
    ///
    /// Note: `memory.ram().size()` may be larger than this when the RAM backend is wrapped in
    /// `MappedGuestMemory` to expose a PC-style non-contiguous guest-physical layout.
    ram_size_bytes: u64,
    pub interrupts: Rc<RefCell<PlatformInterrupts>>,

    pub pci_cfg: SharedPciConfigPorts,
    pub pci_intx: PciIntxRouter,
    pub acpi_pm: SharedAcpiPmIo<ManualClock>,

    #[cfg(feature = "hda")]
    pub hda: Option<Rc<RefCell<HdaPciDevice>>>,
    pub nvme: Option<Rc<RefCell<NvmePciDevice>>>,
    pub ahci: Option<Rc<RefCell<AhciPciDevice>>>,
    pub ide: Option<Rc<RefCell<Piix3IdePciDevice>>>,
    ide_irq14_line: PlatformIrqLine,
    ide_irq15_line: PlatformIrqLine,
    e1000: Option<Rc<RefCell<E1000Device>>>,
    pub uhci: Option<Rc<RefCell<UhciPciDevice>>>,
    pub ehci: Option<Rc<RefCell<EhciPciDevice>>>,
    pub xhci: Option<Rc<RefCell<XhciPciDevice>>>,
    pub virtio_blk: Option<Rc<RefCell<VirtioPciDevice>>>,

    pci_intx_sources: Vec<PciIntxSource>,
    pci_allocator: PciResourceAllocator,
    pci_io_router: SharedPciIoBarRouter,
    pci_mmio_router: SharedPciBarMmioRouter,

    clock: ManualClock,
    pit: SharedPit8254,
    rtc: SharedRtcCmos<ManualClock, PlatformIrqLine>,
    hpet: Rc<RefCell<hpet::Hpet<ManualClock>>>,
    i8042: I8042Ports,

    usb_ns_remainder: u64,
    xhci_ns_remainder: u64,

    reset_events: Rc<RefCell<Vec<ResetEvent>>>,
    sleep_events: Rc<RefCell<Vec<AcpiSleepState>>>,
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
        // Note: the HDA device itself is only included when the crate feature `hda` is enabled.
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_hda: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_hda_dirty_tracking(ram_size: usize) -> Self {
        // Note: the HDA device itself is only included when the crate feature `hda` is enabled.
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

    pub fn new_with_nvme_dirty_tracking(ram_size: usize) -> Self {
        Self::new_with_config_dirty_tracking(
            ram_size,
            PcPlatformConfig {
                enable_nvme: true,
                ..Default::default()
            },
            DEFAULT_DIRTY_PAGE_SIZE,
        )
    }

    pub fn new_with_nvme_disk(ram_size: usize, disk: Box<dyn VirtualDisk>) -> Self {
        Self::new_with_config_and_nvme_disk(
            ram_size,
            PcPlatformConfig {
                enable_nvme: true,
                ..Default::default()
            },
            disk,
        )
    }

    pub fn new_with_config_and_nvme_disk(
        ram_size: usize,
        mut config: PcPlatformConfig,
        disk: Box<dyn VirtualDisk>,
    ) -> Self {
        config.enable_nvme = true;
        let ram = DenseMemory::new(ram_size as u64).expect("failed to allocate guest RAM");
        Self::new_with_config_and_ram_inner(
            ram_size as u64,
            Box::new(ram),
            config,
            None,
            Some(disk),
            None,
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

    /// Constructs the canonical Windows 7 boot/install storage topology and attaches media.
    ///
    /// Topology (fixed):
    /// - AHCI HDD on `00:02.0` (port 0)
    /// - IDE/ATAPI CD-ROM on `00:01.1` (secondary channel, master drive)
    ///
    /// For the normative contract, see: `docs/05-storage-topology-win7.md`.
    pub fn new_with_windows7_storage_topology(
        ram_size: usize,
        storage: Windows7StorageTopologyConfig,
    ) -> Self {
        let mut pc = Self::new_with_win7_storage(ram_size);
        pc.attach_ahci_drive_port0(storage.hdd);
        pc.attach_ide_secondary_master_atapi(storage.cdrom);
        pc
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

    pub fn new_with_xhci(ram_size: usize) -> Self {
        Self::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_xhci: true,
                ..Default::default()
            },
        )
    }

    pub fn new_with_xhci_dirty_tracking(ram_size: usize) -> Self {
        Self::new_with_config_dirty_tracking(
            ram_size,
            PcPlatformConfig {
                enable_xhci: true,
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

    pub fn new_with_virtio_blk_disk(ram_size: usize, disk: Box<dyn VirtualDisk>) -> Self {
        Self::new_with_config_and_virtio_blk_disk(
            ram_size,
            PcPlatformConfig {
                enable_virtio_blk: true,
                ..Default::default()
            },
            disk,
        )
    }

    pub fn new_with_config_and_virtio_blk_disk(
        ram_size: usize,
        mut config: PcPlatformConfig,
        disk: Box<dyn VirtualDisk>,
    ) -> Self {
        config.enable_virtio_blk = true;
        let ram = DenseMemory::new(ram_size as u64).expect("failed to allocate guest RAM");
        Self::new_with_config_and_ram_inner(
            ram_size as u64,
            Box::new(ram),
            config,
            None,
            None,
            Some(disk),
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
        let ram_size_bytes = ram.size();
        Self::new_with_config_and_ram_inner(ram_size_bytes, ram, config, None, None, None)
    }

    pub fn new_with_config_and_ram_dirty_tracking(
        ram: Box<dyn GuestMemory>,
        config: PcPlatformConfig,
        page_size: u32,
    ) -> Self {
        let ram_size_bytes = ram.size();
        Self::new_with_config_and_ram_inner(
            ram_size_bytes,
            ram,
            config,
            Some(page_size),
            None,
            None,
        )
    }

    fn new_with_config_and_ram_inner(
        ram_size_bytes: u64,
        ram: Box<dyn GuestMemory>,
        config: PcPlatformConfig,
        dirty_page_size: Option<u32>,
        nvme_disk_override: Option<NvmeDisk>,
        virtio_blk_disk_override: Option<VirtioBlkDisk>,
    ) -> Self {
        let chipset = ChipsetState::new(false);
        let filter = AddressFilter::new(chipset.a20());

        let mut io = IoPortBus::new();
        let (mut memory, xhci_dma_bus): (MemoryBus, Option<Rc<RefCell<dyn memory::MemoryBus>>>) =
            if config.enable_xhci {
                // xHCI performs DMA as an immediate side effect of some MMIO writes (notably
                // USBCMD.RUN edges and doorbell processing). The xHCI PCI wrapper therefore accepts
                // an optional independent DMA bus via `set_dma_memory_bus`.
                //
                // To keep those MMIO-triggered DMA accesses coherent with the platform memory bus,
                // share the RAM backing store and construct a minimal `PhysicalMemoryBus` for the
                // controller's DMA path.
                let shared_ram = SharedGuestMemory::new(ram);
                let dma_bus: Rc<RefCell<dyn memory::MemoryBus>> = Rc::new(RefCell::new(
                    PhysicalMemoryBus::new(Box::new(shared_ram.clone())),
                ));

                let memory = match dirty_page_size {
                    Some(page_size) => {
                        MemoryBus::with_ram_dirty_tracking(filter, Box::new(shared_ram), page_size)
                    }
                    None => MemoryBus::with_ram(filter, Box::new(shared_ram)),
                };
                (memory, Some(dma_bus))
            } else {
                // Fast path: keep the original RAM backend unchanged (including optional
                // `get_slice`/`get_slice_mut` support needed by virtio).
                let memory = match dirty_page_size {
                    Some(page_size) => MemoryBus::with_ram_dirty_tracking(filter, ram, page_size),
                    None => MemoryBus::with_ram(filter, ram),
                };
                (memory, None)
            };

        let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new_with_cpu_count(
            config.cpu_count,
        )));
        let ide_irq14_line = PlatformIrqLine::isa(interrupts.clone(), 14);
        let ide_irq15_line = PlatformIrqLine::isa(interrupts.clone(), 15);

        let clock = ManualClock::new();

        let reset_events = Rc::new(RefCell::new(Vec::new()));
        let sleep_events = Rc::new(RefCell::new(Vec::new()));

        PlatformInterrupts::register_imcr_ports(&mut io, interrupts.clone());
        register_pic8259_on_platform_interrupts(&mut io, interrupts.clone());

        let dma = Rc::new(RefCell::new(Dma8237::new()));
        register_dma8237(&mut io, dma);

        let pit = Rc::new(RefCell::new(Pit8254::new()));
        pit.borrow_mut()
            .connect_irq0_to_platform_interrupts(interrupts.clone());
        register_pit8254(&mut io, pit.clone());

        let rtc_irq8 = PlatformIrqLine::isa(interrupts.clone(), 8);
        let rtc = Rc::new(RefCell::new(RtcCmos::new(clock.clone(), rtc_irq8)));
        // Program CMOS with the *actual* guest RAM size (not including the below-4GiB PCI MMIO
        // hole when RAM is remapped above 4GiB).
        rtc.borrow_mut().set_memory_size_bytes(ram_size_bytes);
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
            A20_GATE_PORT,
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
                request_power_off: Some(Box::new({
                    let reset_events = reset_events.clone();
                    move || reset_events.borrow_mut().push(ResetEvent::PowerOff)
                })),
                request_sleep: Some(Box::new({
                    let sleep_events = sleep_events.clone();
                    move |state| {
                        // S5 is surfaced via `ResetEvent::PowerOff`. Record all other sleep states
                        // so embeddings can decide what to do (e.g. suspend on S3, power off on S4).
                        if state != AcpiSleepState::S5 {
                            sleep_events.borrow_mut().push(state);
                        }
                    }
                })),
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
        let pci_io_router: SharedPciIoBarRouter =
            Rc::new(RefCell::new(PciIoBarRouter::new(pci_cfg.clone())));

        #[cfg(feature = "hda")]
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

            let disk = nvme_disk_override.unwrap_or_else(|| {
                // Use an `aero-storage` disk image as the backend for the NVMe controller so the same
                // underlying disk abstraction can be reused across controllers (AHCI/NVMe/virtio-blk).
                Box::new(
                    RawDisk::create(MemBackend::new(), 1024u64 * SECTOR_SIZE as u64)
                        .expect("failed to allocate in-memory NVMe disk"),
                )
            });
            let nvme = Rc::new(RefCell::new(
                NvmePciDevice::try_new_from_virtual_disk(disk)
                    .expect("NVMe disk should be 512-byte aligned"),
            ));
            // Provide an MSI sink so the NVMe device model can deliver MSI/MSI-X when the guest
            // enables it via PCI config space.
            nvme.borrow_mut()
                .set_msi_target(Some(Box::new(interrupts.clone())));

            {
                let nvme_for_intx = nvme.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |pc| {
                        let (command, msi_state, msix_state) = {
                            let mut pci_cfg = pc.pci_cfg.borrow_mut();
                            let cfg = pci_cfg.bus_mut().device_config(bdf);
                            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                            let msi_state = cfg
                                .and_then(|cfg| cfg.capability::<MsiCapability>())
                                .map(|msi| {
                                    (
                                        msi.enabled(),
                                        msi.message_address(),
                                        msi.message_data(),
                                        msi.mask_bits(),
                                    )
                                });
                            let msix_state = cfg
                                .and_then(|cfg| cfg.capability::<MsixCapability>())
                                .map(|msix| (msix.enabled(), msix.function_masked()));
                            (command, msi_state, msix_state)
                        };

                        // Keep device-side gating consistent when the same device model is also used
                        // outside the platform (e.g. in unit tests).
                        //
                        // Note: MSI pending bits are device-managed and must not be overwritten from
                        // the canonical PCI config space (which cannot observe them).
                        let mut nvme = nvme_for_intx.borrow_mut();
                        let cfg = nvme.config_mut();
                        cfg.set_command(command);
                        if let Some((enabled, addr, data, mask)) = msi_state {
                            sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                        }
                        if let Some((enabled, function_masked)) = msix_state {
                            sync_msix_capability_into_config(cfg, enabled, function_masked);
                        }

                        nvme.irq_level()
                    }),
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
            // Provide an MSI sink so the device model can inject message-signaled interrupts when
            // the guest enables MSI in PCI config space.
            ahci.borrow_mut()
                .set_msi_target(Some(Box::new(interrupts.clone())));
            // Attach a small in-memory disk by default so guests see a SATA device without any
            // additional host-side wiring. Callers can override this by attaching their own disk
            // via `PcPlatform::attach_ahci_disk_port0`.
            {
                let disk = RawDisk::create(MemBackend::new(), 1024u64 * SECTOR_SIZE as u64)
                    .expect("failed to allocate in-memory AHCI disk");
                let drive = AtaDrive::new(Box::new(disk))
                    .expect("in-memory AHCI disk should be 512-byte aligned");
                ahci.borrow_mut().attach_drive(0, drive);
            }

            {
                let ahci_for_intx = ahci.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |pc| {
                        let (command, msi_state) = {
                            let mut pci_cfg = pc.pci_cfg.borrow_mut();
                            let cfg = pci_cfg.bus_mut().device_config(bdf);
                            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                            let msi_state = cfg
                                .and_then(|cfg| cfg.capability::<MsiCapability>())
                                .map(|msi| {
                                    (
                                        msi.enabled(),
                                        msi.message_address(),
                                        msi.message_data(),
                                        msi.mask_bits(),
                                    )
                                });
                            (command, msi_state)
                        };

                        // Keep device-side gating consistent when the same device model is also used
                        // outside the platform (e.g. in unit tests).
                        let mut ahci = ahci_for_intx.borrow_mut();
                        {
                            // Note: MSI pending bits are device-managed and must not be overwritten
                            // from the canonical PCI config space (which cannot observe them).
                            let cfg = ahci.config_mut();
                            cfg.set_command(command);
                            if let Some((enabled, addr, data, mask)) = msi_state {
                                sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                            }
                        }

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
                    query_level: Box::new(move |pc| {
                        let command = {
                            let mut pci_cfg = pc.pci_cfg.borrow_mut();
                            pci_cfg
                                .bus_mut()
                                .device_config(bdf)
                                .map(|cfg| cfg.command())
                                .unwrap_or(0)
                        };

                        // Keep device-side gating consistent when the same device model is also
                        // used outside the platform (e.g. in unit tests).
                        let mut uhci = uhci_for_intx.borrow_mut();
                        uhci.config_mut().set_command(command);

                        uhci.irq_level()
                    }),
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

        let ehci = if config.enable_ehci {
            let profile = aero_devices::pci::profile::USB_EHCI_ICH9;
            let bdf = profile.bdf;

            let ehci = Rc::new(RefCell::new(EhciPciDevice::default()));

            {
                let ehci_for_intx = ehci.clone();
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

                        // Keep device-side gating consistent when the same device model is also
                        // used outside the platform (e.g. in unit tests).
                        let mut ehci = ehci_for_intx.borrow_mut();
                        ehci.config_mut().set_command(command);

                        ehci.irq_level()
                    }),
                });
            }

            let mut dev = EhciPciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(ehci)
        } else {
            None
        };

        let xhci = if config.enable_xhci {
            let profile = aero_devices::pci::profile::USB_XHCI_QEMU;
            let bdf = profile.bdf;

            let xhci = Rc::new(RefCell::new(XhciPciDevice::default()));
            // xHCI performs DMA as an immediate side effect of some MMIO writes (notably USBCMD.RUN
            // edges). Provide a dedicated DMA bus backed by the same underlying guest RAM so these
            // accesses are coherent with the main platform memory bus.
            xhci.borrow_mut().set_dma_memory_bus(xhci_dma_bus.clone());
            // Provide an MSI sink so the xHCI device model can deliver MSI when the guest enables
            // it via PCI config space.
            xhci.borrow_mut()
                .set_msi_target(Some(Box::new(interrupts.clone())));

            {
                let xhci_for_intx = xhci.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |pc| {
                        let (command, msi_state, msix_state) = {
                            let mut pci_cfg = pc.pci_cfg.borrow_mut();
                            let cfg = pci_cfg.bus_mut().device_config(bdf);
                            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                            let msi_state = cfg
                                .and_then(|cfg| cfg.capability::<MsiCapability>())
                                .map(|msi| {
                                    (
                                        msi.enabled(),
                                        msi.message_address(),
                                        msi.message_data(),
                                        msi.mask_bits(),
                                    )
                                });
                            let msix_state = cfg
                                .and_then(|cfg| cfg.capability::<MsixCapability>())
                                .map(|msix| (msix.enabled(), msix.function_masked()));
                            (command, msi_state, msix_state)
                        };

                        // Keep device-side gating consistent when the same device model is also
                        // used outside the platform (e.g. in unit tests). This also ensures INTx
                        // is suppressed when MSI is active.
                        //
                        // Note: MSI pending bits are device-managed and must not be overwritten from
                        // the canonical PCI config space (which cannot observe them).
                        let mut xhci = xhci_for_intx.borrow_mut();
                        let cfg = xhci.config_mut();
                        cfg.set_command(command);
                        if let Some((enabled, addr, data, mask)) = msi_state {
                            sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                        }
                        if let Some((enabled, function_masked)) = msix_state {
                            sync_msix_capability_into_config(cfg, enabled, function_masked);
                        }

                        xhci.irq_level()
                    }),
                });
            }

            let mut dev = XhciPciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(xhci)
        } else {
            None
        };

        let ide = if config.enable_ide {
            let ide = Rc::new(RefCell::new(Piix3IdePciDevice::new()));
            // Attach a small in-memory disk by default so guests see an IDE HDD without any
            // additional host-side wiring. Callers can override this by attaching their own disk
            // via `PcPlatform::attach_ide_primary_master_disk`.
            {
                let disk = RawDisk::create(MemBackend::new(), 1024u64 * SECTOR_SIZE as u64)
                    .expect("failed to allocate in-memory IDE disk");
                let drive = AtaDrive::new(Box::new(disk))
                    .expect("in-memory IDE disk should be 512-byte aligned");
                ide.borrow_mut().controller.attach_primary_master_ata(drive);
            }

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
                    query_level: Box::new(move |pc| {
                        let command = {
                            let mut pci_cfg = pc.pci_cfg.borrow_mut();
                            pci_cfg
                                .bus_mut()
                                .device_config(bdf)
                                .map(|cfg| cfg.command())
                                .unwrap_or(0)
                        };

                        // Keep the device model's internal PCI command register in sync so
                        // `E1000Device::irq_level` can respect COMMAND.INTX_DISABLE (bit 10) even
                        // though the platform owns the canonical PCI config space.
                        let mut dev = e1000_for_intx.borrow_mut();
                        dev.pci_config_write(0x04, 2, u32::from(command));
                        dev.irq_level()
                    }),
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
            let backend: VirtioBlkDisk = virtio_blk_disk_override.unwrap_or_else(|| {
                Box::new(
                    RawDisk::create(MemBackend::new(), (16 * 1024 * 1024) as u64)
                        .expect("failed to allocate in-memory virtio-blk disk"),
                )
            });

            let interrupts_sink: Box<dyn VirtioInterruptSink> =
                Box::new(VirtioPlatformInterruptSink {
                    interrupts: interrupts.clone(),
                });
            let virtio_blk = Rc::new(RefCell::new(VirtioPciDevice::new(
                Box::new(VirtioBlk::new(backend)),
                interrupts_sink,
            )));

            {
                let virtio_for_intx = virtio_blk.clone();
                pci_intx_sources.push(PciIntxSource {
                    bdf,
                    pin: PciInterruptPin::IntA,
                    query_level: Box::new(move |pc| {
                        let (command, msix_enabled, msix_masked) = {
                            let mut pci_cfg = pc.pci_cfg.borrow_mut();
                            match pci_cfg.bus_mut().device_config(bdf) {
                                Some(cfg) => {
                                    let msix = cfg.capability::<MsixCapability>();
                                    (
                                        cfg.command(),
                                        msix.is_some_and(|msix| msix.enabled()),
                                        msix.is_some_and(|msix| msix.function_masked()),
                                    )
                                }
                                None => (0, false, false),
                            }
                        };

                        // Keep the virtio transport's internal PCI command register in sync so
                        // `VirtioPciDevice::irq_level` can respect COMMAND.INTX_DISABLE (bit 10)
                        // even though the PC platform owns the canonical PCI config space.
                        let mut dev = virtio_for_intx.borrow_mut();
                        // Mirror MSI-X enable/mask bits into the runtime virtio transport so INTx
                        // is suppressed once MSI-X is enabled in canonical PCI config space.
                        sync_msix_capability_into_config(
                            dev.config_mut(),
                            msix_enabled,
                            msix_masked,
                        );
                        dev.set_pci_command(command);
                        dev.irq_level()
                    }),
                });
            }

            let mut dev = VirtioBlkPciConfigDevice::new(config.enable_virtio_msix);
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

            // Bus Master IDE (BAR4) is a relocatable PCI I/O BAR. Register a handler so port I/O
            // accesses follow BAR relocation and COMMAND.IO gating.
            pci_io_router.borrow_mut().register_handler(
                bdf,
                4,
                PcIdeBusMasterBar {
                    pci_cfg: pci_cfg.clone(),
                    ide: ide_dev.clone(),
                    bdf,
                },
            );
        }

        // Register UHCI's relocatable BAR4 I/O region through the PCI I/O BAR router.
        if let Some(uhci_dev) = uhci.as_ref() {
            let bdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
            let bar = UhciPciDevice::IO_BAR_INDEX;
            pci_io_router.borrow_mut().register_handler(
                bdf,
                bar,
                PcUhciIoBar {
                    uhci: uhci_dev.clone(),
                },
            );
        }
        // E1000 BAR1 is a relocatable PCI I/O BAR.
        if let Some(e1000_dev) = e1000.as_ref() {
            let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
            pci_io_router.borrow_mut().register_handler(
                bdf,
                1,
                E1000PciIoBar {
                    e1000: e1000_dev.clone(),
                },
            );
        }

        // Map the full PCI MMIO window reported by ACPI (`PCI0._CRS`) so BAR reprogramming is
        // reflected immediately even when the guest OS relocates a BAR outside the allocator's
        // default sub-window.
        //
        // This window intentionally starts above ECAM (`PCIE_ECAM_BASE..`) and ends right below
        // the IOAPIC base to avoid overlaps with fixed chipset MMIO.
        let pci_mmio_base = aero_pc_constants::PCI_MMIO_BASE;
        let pci_mmio_size = aero_pc_constants::PCI_MMIO_SIZE;
        let pci_mmio_router: SharedPciBarMmioRouter = Rc::new(RefCell::new(PciBarMmioRouter::new(
            pci_mmio_base,
            pci_cfg.clone(),
        )));
        {
            let mut router = pci_mmio_router.borrow_mut();

            #[cfg(feature = "hda")]
            if let Some(hda) = hda.clone() {
                router.register_shared_handler(aero_devices::pci::profile::HDA_ICH6.bdf, 0, hda);
            }

            if let Some(ahci) = ahci.clone() {
                // ICH9 AHCI uses BAR5 (ABAR).
                let bdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
                router.register_handler(
                    bdf,
                    AHCI_ABAR_BAR_INDEX,
                    PciConfigSyncedMmioBar::new(pci_cfg.clone(), ahci, bdf, AHCI_ABAR_BAR_INDEX),
                );
            }
            if let Some(e1000) = e1000.clone() {
                router.register_shared_handler(
                    aero_devices::pci::profile::NIC_E1000_82540EM.bdf,
                    0,
                    e1000,
                );
            }
            if let Some(ehci) = ehci.clone() {
                let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
                let bar = EhciPciDevice::MMIO_BAR_INDEX;
                router.register_handler(
                    bdf,
                    bar,
                    PciConfigSyncedMmioBar::new(pci_cfg.clone(), ehci, bdf, bar),
                );
            }
            if let Some(virtio_blk) = virtio_blk.clone() {
                router.register_handler(
                    aero_devices::pci::profile::VIRTIO_BLK.bdf,
                    0,
                    VirtioPciBar0Mmio {
                        pci_cfg: pci_cfg.clone(),
                        bdf: aero_devices::pci::profile::VIRTIO_BLK.bdf,
                        dev: virtio_blk,
                    },
                );
            }
            if let Some(nvme) = nvme.clone() {
                let bdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
                router.register_handler(
                    bdf,
                    0,
                    PciConfigSyncedMmioBar::new(pci_cfg.clone(), nvme, bdf, 0),
                );
            }
        }
        memory
            .map_mmio(
                pci_mmio_base,
                pci_mmio_size,
                Box::new(SharedPciBarMmioRouterMmio {
                    router: pci_mmio_router.clone(),
                }),
            )
            .unwrap();

        // Register dispatchers for the PCI I/O port windows advertised by ACPI (`PCI0._CRS`).
        //
        // The allocator's default I/O window (`PciResourceAllocatorConfig::io_base/io_size`) is a
        // *subset* of the ACPI-reported windows, but guests are allowed to reprogram PCI I/O BARs
        // anywhere inside `_CRS`. Registering the router over the full windows ensures port I/O
        // continues to decode correctly after BAR relocation.
        //
        // Q35-style windows:
        // - `0x0000..0x0CF7` (len 0x0CF8)
        // - `0x0D00..0xFFFF` (len 0xF300)
        //
        // The gap `0x0CF8..0x0CFF` is reserved for PCI config mechanism #1.
        io.register_range(
            0x0000,
            0x0CF8,
            Box::new(PciIoBarRouterPort {
                router: pci_io_router.clone(),
            }),
        );
        io.register_range(
            0x0D00,
            0xF300,
            Box::new(PciIoBarRouterPort {
                router: pci_io_router.clone(),
            }),
        );

        let hpet = Rc::new(RefCell::new(hpet::Hpet::new_default(clock.clone())));

        memory
            .map_mmio(
                LAPIC_MMIO_BASE,
                LAPIC_MMIO_SIZE,
                Box::new(LapicMmio::from_platform_interrupts(interrupts.clone())),
            )
            .unwrap();
        memory
            .map_mmio(
                IOAPIC_MMIO_BASE,
                IOAPIC_MMIO_SIZE,
                Box::new(IoApicMmio::from_platform_interrupts(interrupts.clone())),
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

        let mut pc = Self {
            chipset,
            io,
            memory,
            ram_size_bytes,
            interrupts,
            pci_cfg,
            pci_intx,
            acpi_pm,
            #[cfg(feature = "hda")]
            hda,
            nvme,
            ahci,
            ide,
            ide_irq14_line,
            ide_irq15_line,
            e1000,
            uhci,
            ehci,
            xhci,
            virtio_blk,
            pci_intx_sources,
            pci_allocator,
            pci_io_router,
            pci_mmio_router,
            clock,
            pit,
            rtc,
            hpet,
            i8042: i8042_ports,
            usb_ns_remainder: 0,
            xhci_ns_remainder: 0,
            reset_events,
            sleep_events,
        };

        if let Some(ehci) = pc.ehci.clone() {
            let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
            let bar = EhciPciDevice::MMIO_BAR_INDEX;
            let handler = Rc::new(RefCell::new(PciConfigSyncedMmioBar::new(
                pc.pci_cfg.clone(),
                ehci,
                bdf,
                bar,
            )));
            pc.register_pci_mmio_bar_handler(bdf, bar, handler);
        }

        if let Some(xhci) = pc.xhci.clone() {
            let bdf = aero_devices::pci::profile::USB_XHCI_QEMU.bdf;
            let bar = XhciPciDevice::MMIO_BAR_INDEX;
            // xHCI can raise an interrupt as an immediate side effect of MMIO writes (e.g. RUN
            // edges). Keep the device model's internal PCI config image synchronized (including
            // MSI/MSI-X enable + message fields) so interrupt delivery observes the guest's latest
            // config space programming.
            let handler = Rc::new(RefCell::new(PciConfigSyncedMmioBar::new(
                pc.pci_cfg.clone(),
                xhci,
                bdf,
                bar,
            )));
            pc.register_pci_mmio_bar_handler(bdf, bar, handler);
        }

        pc
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
    /// The handler receives an `offset` relative to the BAR base (i.e. `port - bar.base`).
    pub fn register_pci_io_bar<H>(&mut self, bdf: PciBdf, bar: u8, handler: H)
    where
        H: PciIoBarHandler + 'static,
    {
        self.pci_io_router
            .borrow_mut()
            .register_handler(bdf, bar, handler);
    }

    /// Registers a handler for a PCI MMIO BAR in the platform's PCI MMIO window.
    ///
    /// The handler is keyed by `(bdf, bar_index)` and is dispatched to whenever that BAR currently
    /// decodes the accessed MMIO address (respecting PCI command MEM decoding and BAR relocation).
    pub fn register_pci_mmio_bar_handler<T>(
        &mut self,
        bdf: PciBdf,
        bar: u8,
        handler: Rc<RefCell<T>>,
    ) where
        T: MmioHandler + 'static,
    {
        self.pci_mmio_router
            .borrow_mut()
            .register_shared_handler(bdf, bar, handler);
    }

    pub fn pci_mmio_router(&self) -> SharedPciBarMmioRouter {
        self.pci_mmio_router.clone()
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

    /// Host-facing helper for resuming from an ACPI sleep state.
    ///
    /// Sets `PM1_STS.WAK_STS` and triggers a wake source (power button) so a guest that has armed
    /// the power button as a wake event receives an SCI.
    pub fn acpi_wake(&mut self) {
        let mut pm = self.acpi_pm.borrow_mut();
        pm.set_wake_status();
        pm.trigger_power_button();
    }

    pub fn has_e1000(&self) -> bool {
        self.e1000.is_some()
    }

    pub fn e1000_mac_addr(&self) -> Option<[u8; 6]> {
        self.e1000.as_ref().map(|e1000| e1000.borrow().mac_addr())
    }

    /// Snapshot/restore + integration hook: returns a clone of the shared E1000 device model.
    ///
    /// Most users should interact with the E1000 through the platform PCI/MMIO/PIO buses and the
    /// host-facing queue helpers (`process_e1000`, `e1000_pop_tx_frame`, `e1000_enqueue_rx_frame`).
    /// This accessor exists for higher-level glue code that wants direct access to the device
    /// model, e.g. to share common pump logic across integration layers.
    pub fn e1000(&self) -> Option<Rc<RefCell<E1000Device>>> {
        self.e1000.clone()
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

        // Re-populate `Interrupt Line`/`Interrupt Pin` config-space fields for devices that are
        // wired through this platform's INTx router.
        for src in &self.pci_intx_sources {
            if let Some(cfg) = pci_cfg.bus_mut().device_config_mut(src.bdf) {
                self.pci_intx
                    .configure_device_intx(src.bdf, Some(src.pin), cfg);
            }
        }
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
        disk: Box<dyn VirtualDisk>,
    ) -> std::io::Result<()> {
        self.attach_ide_secondary_master_atapi(AtapiCdrom::new_from_virtual_disk(disk)?);
        Ok(())
    }

    #[cfg(feature = "hda")]
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

    #[cfg(not(feature = "hda"))]
    pub fn process_hda(&mut self, _output_frames: usize) {}

    /// Reset the platform back to a deterministic "power-on" baseline.
    ///
    /// This is intended for higher layers that need a repeatable reset sequence (e.g.
    /// `aero_machine::Machine::reset()` and snapshot restore flows).
    ///
    /// # Semantics
    ///
    /// - Guest RAM is **not** reallocated or cleared. The underlying RAM backing and its contents
    ///   are preserved.
    /// - Chipset state is reset (A20 is disabled).
    /// - Deterministic time sources are reset (`ManualClock` set to `0`) and timer devices are
    ///   reset (PIT, RTC, HPET, LAPIC timer).
    /// - Interrupt routing is reset to the initial legacy-PIC mode (`PlatformInterrupts::reset`).
    /// - PCI core state is reset:
    ///   - PCI config address latch (port `0xCF8`) is cleared.
    ///   - `PciIntxRouter` bookkeeping is cleared.
    ///   - `bios_post(...)` is rerun to reset PCI devices and deterministically reassign BARs.
    /// - ACPI PM and i8042 controller state is reset via their port-level reset hooks.
    /// - Any pending [`ResetEvent`]s are cleared.
    pub fn reset(&mut self) {
        // Reset deterministic time first so any device re-initialization sees `t=0`.
        self.clock.set_ns(0);

        // Reset interrupt controller complex (PIC/IOAPIC/LAPIC + IMCR + LAPIC timer clock).
        self.interrupts.borrow_mut().reset();

        // Reset chipset-level state (A20 starts disabled on a PC reset).
        self.chipset.a20().set_enabled(false);

        // Reset port-mapped devices that provide `PortIoDevice::reset` (i8042, ACPI PM, reset ctrl,
        // A20 gate, PCI I/O window handlers, etc). This intentionally does *not* rebuild the port
        // map.
        self.io.reset();

        // Reset PIT while preserving IRQ wiring (reconnect after recreating the device state).
        {
            let mut pit = self.pit.borrow_mut();
            *pit = Pit8254::new();
            pit.connect_irq0_to_platform_interrupts(self.interrupts.clone());
        }

        // Reset RTC CMOS to a deterministic baseline and re-publish the RAM size fields.
        {
            let irq8 = PlatformIrqLine::isa(self.interrupts.clone(), 8);
            let mut rtc = self.rtc.borrow_mut();
            *rtc = RtcCmos::new(self.clock.clone(), irq8);
            rtc.set_memory_size_bytes(self.ram_size_bytes);
        }

        // Reset HPET state (MMIO mapping retains the same shared `Rc`).
        {
            let mut hpet = self.hpet.borrow_mut();
            *hpet = hpet::Hpet::new_default(self.clock.clone());
        }

        // Reset PCI config-mechanism latch state (0xCF8) and rerun BIOS POST for deterministic BAR
        // allocation / device reset.
        {
            self.pci_cfg
                .borrow_mut()
                .io_write(aero_devices::pci::PCI_CFG_ADDR_PORT, 4, 0);

            self.pci_intx = PciIntxRouter::new(PciIntxRouterConfig::default());

            let mut pci_cfg = self.pci_cfg.borrow_mut();
            bios_post(pci_cfg.bus_mut(), &mut self.pci_allocator).unwrap();

            // Re-populate `Interrupt Line`/`Interrupt Pin` config-space fields for devices that are
            // wired through this platform's INTx router.
            for src in &self.pci_intx_sources {
                if let Some(cfg) = pci_cfg.bus_mut().device_config_mut(src.bdf) {
                    self.pci_intx
                        .configure_device_intx(src.bdf, Some(src.pin), cfg);
                }
            }
        }

        // Reset selected runtime PCI device models that live outside the `PciBus` (which only owns
        // guest-visible config space for this platform).
        if let Some(uhci) = self.uhci.as_ref() {
            uhci.borrow_mut().reset();
        }
        if let Some(ehci) = self.ehci.as_ref() {
            ehci.borrow_mut().reset();
        }
        if let Some(xhci) = self.xhci.as_ref() {
            xhci.borrow_mut().reset();
        }

        // Reset optional PCI device models that are part of the platform and do not own external
        // state that needs to survive reset.
        if let Some(e1000) = self.e1000.as_ref() {
            let mac = e1000.borrow().mac_addr();
            *e1000.borrow_mut() = E1000Device::new(mac);
        }

        // Reset storage controllers while preserving host-attached backends (disks/ISOs).
        //
        // These devices maintain internal DMA/command state that must be cleared on reset, but the
        // host-provided media backends should not be silently dropped.
        if let Some(nvme) = self.nvme.as_ref() {
            nvme.borrow_mut().reset();
        }
        if let Some(ahci) = self.ahci.as_ref() {
            ahci.borrow_mut().reset();
        }
        if let Some(ide) = self.ide.as_ref() {
            ide.borrow_mut().reset();
        }
        if let Some(virtio_blk) = self.virtio_blk.as_ref() {
            virtio_blk.borrow_mut().reset();
        }

        // Reset host-side tick accumulators.
        self.usb_ns_remainder = 0;
        self.xhci_ns_remainder = 0;

        // Clear any reset requests that were pending before the reset was processed.
        self.reset_events.borrow_mut().clear();
        self.sleep_events.borrow_mut().clear();
    }

    pub fn process_nvme(&mut self) {
        let Some(nvme) = self.nvme.as_ref() else {
            return;
        };

        let bdf = aero_devices::pci::profile::NVME_CONTROLLER.bdf;
        let (command, msi_state, msix_state) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let msi_state = cfg
                .and_then(|cfg| cfg.capability::<MsiCapability>())
                .map(|msi| {
                    (
                        msi.enabled(),
                        msi.message_address(),
                        msi.message_data(),
                        msi.mask_bits(),
                    )
                });
            let msix_state = cfg
                .and_then(|cfg| cfg.capability::<MsixCapability>())
                .map(|msix| (msix.enabled(), msix.function_masked()));
            (command, msi_state, msix_state)
        };

        let mut nvme = nvme.borrow_mut();
        {
            // Keep the NVMe model's view of PCI config state in sync so it can apply bus mastering
            // gating and deliver MSI/MSI-X during `process()`.
            //
            // Note: MSI pending bits are device-managed and must not be overwritten from the
            // canonical PCI config space (which cannot observe them).
            let cfg = nvme.config_mut();
            cfg.set_command(command);
            if let Some((enabled, addr, data, mask)) = msi_state {
                sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
            }
            if let Some((enabled, function_masked)) = msix_state {
                sync_msix_capability_into_config(cfg, enabled, function_masked);
            }
        }
        nvme.process(&mut self.memory);

        // Mirror device-managed MSI pending bits back into the canonical PCI config space so guest
        // config reads observe them. The canonical config space cannot infer pending bits on its
        // own because they are latched by the runtime NVMe device model.
        let pending_bits = nvme
            .config()
            .capability::<MsiCapability>()
            .map(|msi| msi.pending_bits())
            .unwrap_or(0);
        drop(nvme);

        if let Some(cfg) = self.pci_cfg.borrow_mut().bus_mut().device_config_mut(bdf) {
            if let Some(msi) = cfg.capability_mut::<MsiCapability>() {
                msi.set_pending_bits(pending_bits);
            }
        }
    }

    pub fn process_ahci(&mut self) {
        let Some(ahci) = self.ahci.as_ref() else {
            return;
        };

        let bdf = aero_devices::pci::profile::SATA_AHCI_ICH9.bdf;
        let (command, msi_state) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let msi_state = cfg
                .and_then(|cfg| cfg.capability::<MsiCapability>())
                .map(|msi| {
                    (
                        msi.enabled(),
                        msi.message_address(),
                        msi.message_data(),
                        msi.mask_bits(),
                    )
                });
            (command, msi_state)
        };

        let bus_master_enabled = (command & (1 << 2)) != 0;

        // Keep the device's internal view of PCI config state in sync so it can apply bus mastering
        // gating and deliver MSI during `process()`.
        let mut ahci = ahci.borrow_mut();
        {
            // Note: MSI pending bits are device-managed and must not be overwritten from the
            // canonical PCI config space (which cannot observe them).
            let cfg = ahci.config_mut();
            cfg.set_command(command);
            if let Some((enabled, addr, data, mask)) = msi_state {
                sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
            }
        }

        if bus_master_enabled {
            ahci.process(&mut self.memory);
        }

        // Mirror device-managed MSI pending bits back into the canonical PCI config space so guest
        // config reads observe them.
        let pending_bits = ahci
            .config()
            .capability::<MsiCapability>()
            .map(|msi| msi.pending_bits())
            .unwrap_or(0);
        drop(ahci);

        if let Some(cfg) = self.pci_cfg.borrow_mut().bus_mut().device_config_mut(bdf) {
            if let Some(msi) = cfg.capability_mut::<MsiCapability>() {
                msi.set_pending_bits(pending_bits);
            }
        }
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
            let bar4 = cfg.and_then(|cfg| cfg.bar_range(4)).map(|range| range.base);
            (command, bar4)
        };

        {
            let mut ide = ide.borrow_mut();
            ide.config_mut().set_command(command);
            if let Some(bar4_base) = bar4_base {
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
        let (command, msix_enabled, msix_masked) = {
            let mut pci_cfg = self.pci_cfg.borrow_mut();
            match pci_cfg.bus_mut().device_config(bdf) {
                Some(cfg) => {
                    let msix = cfg.capability::<MsixCapability>();
                    (
                        cfg.command(),
                        msix.is_some_and(|msix| msix.enabled()),
                        msix.is_some_and(|msix| msix.function_masked()),
                    )
                }
                None => (0, false, false),
            }
        };

        let mut virtio_blk = virtio_blk.borrow_mut();
        // Keep the virtio transport's internal PCI command register in sync with the platform PCI
        // bus. The PC platform maintains a separate canonical config-space model for enumeration,
        // so the virtio transport must be updated explicitly.
        virtio_blk.set_pci_command(command);
        sync_virtio_msix_from_platform(&mut virtio_blk, msix_enabled, msix_masked);
        if (command & (1 << 2)) == 0 {
            return;
        }
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

        // Only allow the device to DMA when Bus Mastering is enabled (PCI command bit 2).
        let bus_master_enabled = (command & (1 << 2)) != 0;

        // Keep the device model's internal PCI config state in sync with the platform PCI bus.
        //
        // The E1000 model gates DMA on COMMAND.BME (bit 2) by consulting its own PCI config state,
        // while the PC platform maintains a separate canonical config space for enumeration.
        // Mirror the live config (command + BAR bases) into the NIC model before polling so
        // bus-master gating works without needing a general "config write hook".
        //
        // Note: we still mirror the config registers even when bus mastering is disabled so the
        // model's internal state stays coherent with the guest-programmed PCI config space.
        let mut dev = e1000.borrow_mut();
        dev.pci_config_write(0x04, 2, u32::from(command));
        if let Ok(bar0_base) = u32::try_from(bar0_base) {
            dev.pci_config_write(0x10, 4, bar0_base);
        }
        if let Ok(bar1_base) = u32::try_from(bar1_base) {
            dev.pci_config_write(0x14, 4, bar1_base);
        }

        if bus_master_enabled {
            dev.poll(&mut self.memory);
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
            self.ide_irq14_line.set_level(irq14);
            self.ide_irq15_line.set_level(irq15);
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
        self.acpi_pm.borrow_mut().tick(delta_ns);
        self.pit.borrow_mut().advance_ns(delta_ns);
        self.rtc.borrow_mut().tick();

        // Keep the LAPIC timer deterministic: advance time only via `tick`.
        self.interrupts.borrow().tick(delta_ns);

        {
            let mut hpet = self.hpet.borrow_mut();
            let mut interrupts = self.interrupts.borrow_mut();
            hpet.poll(&mut *interrupts);
        }

        // USB controllers advance at 1ms granularity (UHCI frame tick, EHCI micro-frame groups).
        if self.uhci.is_some() || self.ehci.is_some() {
            const NS_PER_MS: u64 = 1_000_000;

            if let Some(uhci) = self.uhci.as_ref() {
                let bdf = aero_devices::pci::profile::USB_UHCI_PIIX3.bdf;
                let (command, bar4_base) = {
                    let mut pci_cfg = self.pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar4_base = cfg
                        .and_then(|cfg| cfg.bar_range(UhciPciDevice::IO_BAR_INDEX))
                        .map(|range| range.base);
                    (command, bar4_base)
                };

                // Keep the UHCI model's view of PCI config state in sync so it can apply bus
                // mastering gating when used via `tick_1ms`.
                let mut uhci = uhci.borrow_mut();
                uhci.config_mut().set_command(command);
                if let Some(bar4_base) = bar4_base {
                    uhci.config_mut()
                        .set_bar_base(UhciPciDevice::IO_BAR_INDEX, bar4_base);
                }
            }

            if let Some(ehci) = self.ehci.as_ref() {
                let bdf = aero_devices::pci::profile::USB_EHCI_ICH9.bdf;
                let (command, bar0_base) = {
                    let mut pci_cfg = self.pci_cfg.borrow_mut();
                    let cfg = pci_cfg.bus_mut().device_config(bdf);
                    let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                    let bar0_base = cfg
                        .and_then(|cfg| cfg.bar_range(EhciPciDevice::MMIO_BAR_INDEX))
                        .map(|range| range.base);
                    (command, bar0_base)
                };

                // Keep the EHCI model's view of PCI config state in sync so it can apply COMMAND
                // gating when used via `tick_1ms`.
                let mut ehci = ehci.borrow_mut();
                ehci.config_mut().set_command(command);
                if let Some(bar0_base) = bar0_base {
                    ehci.config_mut()
                        .set_bar_base(EhciPciDevice::MMIO_BAR_INDEX, bar0_base);
                }
            }

            self.usb_ns_remainder = self.usb_ns_remainder.saturating_add(delta_ns);
            let mut ticks = self.usb_ns_remainder / NS_PER_MS;
            self.usb_ns_remainder %= NS_PER_MS;

            if ticks != 0 {
                let mut uhci = self.uhci.as_ref().map(|dev| dev.borrow_mut());
                let mut ehci = self.ehci.as_ref().map(|dev| dev.borrow_mut());

                while ticks != 0 {
                    if let Some(uhci) = uhci.as_mut() {
                        uhci.tick_1ms(&mut self.memory);
                    }
                    if let Some(ehci) = ehci.as_mut() {
                        ehci.tick_1ms(&mut self.memory);
                    }
                    ticks -= 1;
                }
            }
        }

        if let Some(xhci) = self.xhci.as_ref() {
            const NS_PER_MS: u64 = 1_000_000;
            let bdf = aero_devices::pci::profile::USB_XHCI_QEMU.bdf;
            let (command, msi_state, msix_state) = {
                let mut pci_cfg = self.pci_cfg.borrow_mut();
                let cfg = pci_cfg.bus_mut().device_config(bdf);
                let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
                let msi_state = cfg
                    .and_then(|cfg| cfg.capability::<MsiCapability>())
                    .map(|msi| {
                        (
                            msi.enabled(),
                            msi.message_address(),
                            msi.message_data(),
                            msi.mask_bits(),
                        )
                    });
                let msix_state = cfg
                    .and_then(|cfg| cfg.capability::<MsixCapability>())
                    .map(|msix| (msix.enabled(), msix.function_masked()));
                (command, msi_state, msix_state)
            };

            // Keep the xHCI model's view of PCI config state in sync (including MSI capability
            // state) so it can deliver MSI through `tick_1ms`.
            let mut xhci = xhci.borrow_mut();
            {
                // Note: MSI pending bits are device-managed and must not be overwritten from the
                // canonical PCI config space (which cannot observe them).
                let cfg = xhci.config_mut();
                cfg.set_command(command);
                if let Some((enabled, addr, data, mask)) = msi_state {
                    sync_msi_capability_into_config(cfg, enabled, addr, data, mask);
                }
                if let Some((enabled, function_masked)) = msix_state {
                    sync_msix_capability_into_config(cfg, enabled, function_masked);
                }
            }

            self.xhci_ns_remainder = self.xhci_ns_remainder.saturating_add(delta_ns);
            let mut ticks = self.xhci_ns_remainder / NS_PER_MS;
            self.xhci_ns_remainder %= NS_PER_MS;

            while ticks != 0 {
                xhci.tick_1ms(&mut self.memory);
                ticks -= 1;
            }

            // Mirror device-managed MSI pending bits back into the canonical PCI config space so
            // guest config reads observe them. This is useful even when `delta_ns == 0` (sync-only
            // tick) so callers can observe pending bits latched by the device model.
            let pending_bits = xhci
                .config()
                .capability::<MsiCapability>()
                .map(|msi| msi.pending_bits())
                .unwrap_or(0);
            drop(xhci);

            if let Some(cfg) = self.pci_cfg.borrow_mut().bus_mut().device_config_mut(bdf) {
                if let Some(msi) = cfg.capability_mut::<MsiCapability>() {
                    msi.set_pending_bits(pending_bits);
                }
            }
        }
    }

    pub fn take_reset_events(&mut self) -> Vec<ResetEvent> {
        std::mem::take(&mut *self.reset_events.borrow_mut())
    }

    pub fn take_sleep_events(&mut self) -> Vec<AcpiSleepState> {
        std::mem::take(&mut *self.sleep_events.borrow_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory::SparseMemory;

    fn cmos_read_u8(pc: &mut PcPlatform, index: u8) -> u8 {
        pc.io.write(0x70, 1, u32::from(index));
        pc.io.read(0x71, 1) as u8
    }

    fn cmos_read_u16(pc: &mut PcPlatform, index_lo: u8, index_hi: u8) -> u16 {
        let lo = cmos_read_u8(pc, index_lo);
        let hi = cmos_read_u8(pc, index_hi);
        u16::from(lo) | (u16::from(hi) << 8)
    }

    #[test]
    fn ahci_pci_config_space_exposes_single_msi_capability() {
        let mut dev = AhciPciConfigDevice::new();
        let caps = dev.config_mut().capability_list();
        let msi_count = caps
            .iter()
            .filter(|cap| cap.id == aero_devices::pci::msi::PCI_CAP_ID_MSI)
            .count();
        assert_eq!(msi_count, 1);
    }

    #[test]
    fn xhci_pci_config_space_exposes_single_msix_capability() {
        let mut dev = XhciPciConfigDevice::new();
        let caps = dev.config_mut().capability_list();
        let msix_count = caps.iter().filter(|cap| cap.id == PCI_CAP_ID_MSIX).count();
        assert_eq!(msix_count, 1);
    }

    #[test]
    fn hpet_mmio_size0_is_noop() {
        let clock = ManualClock::new();
        let hpet = Rc::new(RefCell::new(hpet::Hpet::new_default(clock)));
        let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));

        let mut mmio = HpetMmio {
            hpet: hpet.clone(),
            interrupts: interrupts.clone(),
        };

        assert_eq!(memory::MmioHandler::read(&mut mmio, 0, 0), 0);
        memory::MmioHandler::write(&mut mmio, 0, 0, 0);
    }

    #[test]
    fn pc_platform_reset_cmos_reports_configured_ram_size_not_physical_address_space() {
        // Force the Q35-style high-RAM remap by configuring RAM to extend past the ECAM base.
        let ram_size_bytes = PCIE_ECAM_BASE + 8 * 1024 * 1024;

        // Don't allocate multi-GB dense RAM; use a sparse backing.
        let ram = SparseMemory::new(ram_size_bytes).expect("failed to allocate sparse RAM");
        let mut pc = PcPlatform::new_with_config_and_ram(
            Box::new(ram),
            PcPlatformConfig {
                cpu_count: 1,
                enable_hda: false,
                enable_nvme: false,
                enable_ahci: false,
                enable_ide: false,
                enable_e1000: false,
                mac_addr: None,
                enable_uhci: false,
                enable_ehci: false,
                enable_xhci: false,
                enable_virtio_blk: false,
                enable_virtio_msix: false,
            },
        );

        // Ensure the guest-physical address space is larger than the configured contiguous RAM.
        assert!(
            pc.memory.ram().size() > ram_size_bytes,
            "test requires PCI hole remap (phys_size > ram_size_bytes)"
        );

        // The bug being exercised is in `PcPlatform::reset` (it re-creates the RTC/CMOS).
        pc.reset();

        // Expected values, matching `RtcCmos::set_memory_size_bytes`.
        const ONE_MIB: u64 = 1024 * 1024;
        const SIXTEEN_MIB: u64 = 16 * 1024 * 1024;
        const SIXTY_FOUR_KIB: u64 = 64 * 1024;

        let base_kb: u16 = 640;
        let ext_kb =
            (ram_size_bytes.saturating_sub(ONE_MIB) / 1024).min(u64::from(u16::MAX)) as u16;
        let high_blocks = (ram_size_bytes.saturating_sub(SIXTEEN_MIB) / SIXTY_FOUR_KIB)
            .min(u64::from(u16::MAX)) as u16;

        assert_eq!(cmos_read_u16(&mut pc, 0x15, 0x16), base_kb);
        assert_eq!(cmos_read_u16(&mut pc, 0x17, 0x18), ext_kb);
        assert_eq!(cmos_read_u16(&mut pc, 0x30, 0x31), ext_kb);
        assert_eq!(cmos_read_u16(&mut pc, 0x34, 0x35), high_blocks);
    }
}
