//! Canonical full-system "machine" integration layer for Aero.
//!
//! This crate composes the canonical CPU core (`aero_cpu_core`), firmware (`firmware::bios`),
//! physical memory bus (`memory`), and device models (`aero-devices` / `aero-platform`) into a
//! single VM-like interface that is usable from both:
//! - native Rust integration tests, and
//! - `wasm32` builds via `crates/aero-wasm`.
//!
//! The intention is to make "which machine runs in the browser?" an explicit, stable answer:
//! **`aero_machine::Machine`**.
#![forbid(unsafe_code)]

mod guest_time;

pub use guest_time::{GuestTime, DEFAULT_GUEST_CPU_HZ};

use std::cell::RefCell;
use std::fmt;
use std::io::{Cursor, Read, Seek, Write};
use std::rc::Rc;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_cpu_core_with_assists, BatchExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::interrupts::CpuExit;
use aero_cpu_core::state::{CpuMode, CpuState, RFLAGS_IF};
use aero_cpu_core::{AssistReason, CpuCore, Exception};
use aero_devices::a20_gate::A20Gate as A20GateDevice;
use aero_devices::acpi_pm::{
    register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, SharedAcpiPmIo,
};
use aero_devices::clock::ManualClock;
use aero_devices::hpet;
use aero_devices::i8042::{I8042Ports, SharedI8042Controller};
use aero_devices::irq::PlatformIrqLine;
use aero_devices::pci::{
    bios_post, register_pci_config_ports, PciBdf, PciConfigPorts, PciCoreSnapshot, PciDevice,
    PciEcamConfig, PciEcamMmio, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
    PciResourceAllocator, PciResourceAllocatorConfig, SharedPciConfigPorts,
};
use aero_devices::pic8259::register_pic8259_on_platform_interrupts;
use aero_devices::pit8254::{register_pit8254, Pit8254, SharedPit8254};
use aero_devices::reset_ctrl::{ResetCtrl, RESET_CTRL_PORT};
use aero_devices::rtc_cmos::{register_rtc_cmos, RtcCmos, SharedRtcCmos};
use aero_devices::serial::{register_serial16550, Serial16550, SharedSerial16550};
pub use aero_devices_input::Ps2MouseButton;
use aero_interrupts::apic::{IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, LAPIC_MMIO_BASE, LAPIC_MMIO_SIZE};
use aero_net_backend::{FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats, NetworkBackend};
use aero_net_e1000::E1000Device;
use aero_net_pump::tick_e1000;
use aero_pc_platform::{PciBarMmioRouter, PciIoBarHandler, PciIoBarRouter};
use aero_platform::chipset::{A20GateHandle, ChipsetState};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, PlatformInterrupts,
};
use aero_platform::io::{IoPortBus, PortIoDevice as _};
use aero_platform::reset::{ResetKind, ResetLatch};
use aero_snapshot as snapshot;
use firmware::bios::{A20Gate, Bios, BiosBus, BiosConfig, BlockDevice, DiskError, FirmwareMemory};
use memory::{
    DenseMemory, DirtyGuestMemory, DirtyTracker, GuestMemoryError, MapError, MemoryBus as _,
    MmioHandler, PhysicalMemoryBus,
};

mod pci_firmware;
use pci_firmware::SharedPciConfigPortsBiosAdapter;

const FAST_A20_PORT: u16 = 0x92;
const SNAPSHOT_DIRTY_PAGE_SIZE: u32 = 4096;
const DEFAULT_E1000_MAC_ADDR: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

pub mod pc;
pub use pc::{PcMachine, PcMachineConfig};

/// Configuration for [`Machine`].
#[derive(Debug, Clone)]
pub struct MachineConfig {
    /// Guest RAM size in bytes.
    pub ram_size_bytes: u64,
    /// Number of vCPUs (currently must be 1).
    pub cpu_count: u8,
    /// Whether to attach canonical PC platform devices (PIC/APIC/PIT/RTC/PCI/ACPI PM/HPET).
    ///
    /// This is currently opt-in to keep the default machine minimal and deterministic.
    pub enable_pc_platform: bool,
    /// Whether to attach a COM1 16550 serial device at `0x3F8`.
    pub enable_serial: bool,
    /// Whether to attach a legacy i8042 controller at ports `0x60/0x64`.
    pub enable_i8042: bool,
    /// Whether to attach a "fast A20" gate device at port `0x92`.
    pub enable_a20_gate: bool,
    /// Whether to attach a reset control device at port `0xCF9`.
    pub enable_reset_ctrl: bool,
    /// Whether to attach an Intel E1000 (82540EM-ish) PCI NIC.
    ///
    /// Requires [`MachineConfig::enable_pc_platform`].
    pub enable_e1000: bool,
    /// Optional MAC address for the E1000 NIC.
    pub e1000_mac_addr: Option<[u8; 6]>,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            ram_size_bytes: 64 * 1024 * 1024,
            cpu_count: 1,
            enable_pc_platform: false,
            enable_serial: true,
            enable_i8042: true,
            enable_a20_gate: true,
            enable_reset_ctrl: true,
            enable_e1000: false,
            e1000_mac_addr: None,
        }
    }
}

/// A single-step/run invocation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunExit {
    /// The slice completed because `max_insts` was reached.
    Completed { executed: u64 },
    /// The CPU executed `HLT`.
    Halted { executed: u64 },
    /// The guest requested a reset (e.g. via port `0xCF9`).
    ResetRequested { kind: ResetKind, executed: u64 },
    /// Execution stopped because the CPU core needs host assistance.
    Assist { reason: AssistReason, executed: u64 },
    /// Execution stopped due to an exception/fault.
    Exception { exception: Exception, executed: u64 },
    /// Execution stopped due to a fatal CPU exit condition (e.g. triple fault).
    CpuExit { exit: CpuExit, executed: u64 },
}

impl RunExit {
    /// Number of guest instructions executed in this slice (best-effort).
    pub fn executed(&self) -> u64 {
        match *self {
            RunExit::Completed { executed }
            | RunExit::Halted { executed }
            | RunExit::ResetRequested { executed, .. }
            | RunExit::Assist { executed, .. }
            | RunExit::Exception { executed, .. }
            | RunExit::CpuExit { executed, .. } => executed,
        }
    }
}

/// Errors returned when constructing or configuring a [`Machine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineError {
    InvalidCpuCount(u8),
    InvalidDiskSize(usize),
    GuestMemoryTooLarge(u64),
    E1000RequiresPcPlatform,
}

impl fmt::Display for MachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachineError::InvalidCpuCount(count) => {
                write!(
                    f,
                    "unsupported cpu_count {count} (only 1 is supported today)"
                )
            }
            MachineError::InvalidDiskSize(len) => write!(
                f,
                "disk image length {len} is not a multiple of 512 (BIOS sector size)"
            ),
            MachineError::GuestMemoryTooLarge(size) => write!(
                f,
                "guest RAM size {size} bytes does not fit in the current platform's usize"
            ),
            MachineError::E1000RequiresPcPlatform => {
                write!(f, "enable_e1000 requires enable_pc_platform=true")
            }
        }
    }
}

impl std::error::Error for MachineError {}

/// In-memory block device backed by a `Vec<u8>` of 512-byte sectors.
#[derive(Debug, Clone)]
pub struct VecBlockDevice {
    data: Vec<u8>,
}

impl VecBlockDevice {
    pub fn new(mut data: Vec<u8>) -> Result<Self, MachineError> {
        if !data.len().is_multiple_of(512) {
            return Err(MachineError::InvalidDiskSize(data.len()));
        }
        if data.is_empty() {
            // Ensure at least one sector exists so BIOS boot attempts are deterministic.
            data.resize(512, 0);
        }
        Ok(Self { data })
    }

    pub fn from_sector0(sector0: [u8; 512]) -> Self {
        Self {
            data: sector0.to_vec(),
        }
    }
}

impl BlockDevice for VecBlockDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), DiskError> {
        let idx = usize::try_from(lba).map_err(|_| DiskError::OutOfRange)?;
        let start = idx.checked_mul(512).ok_or(DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let src = self.data.get(start..end).ok_or(DiskError::OutOfRange)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        (self.data.len() / 512) as u64
    }
}

struct SystemMemory {
    a20: A20GateHandle,
    inner: RefCell<PhysicalMemoryBus>,
    dirty: DirtyTracker,
}

impl SystemMemory {
    fn new(ram_size_bytes: u64, a20: A20GateHandle) -> Result<Self, MachineError> {
        let ram = DenseMemory::new(ram_size_bytes)
            .map_err(|_| MachineError::GuestMemoryTooLarge(ram_size_bytes))?;
        let (ram, dirty) = DirtyGuestMemory::new(Box::new(ram), SNAPSHOT_DIRTY_PAGE_SIZE);
        let inner = PhysicalMemoryBus::new(Box::new(ram));

        Ok(Self {
            a20,
            inner: RefCell::new(inner),
            dirty,
        })
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20.enabled() {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }

    fn take_dirty_pages(&mut self) -> Vec<u64> {
        self.dirty.take_dirty_pages()
    }

    fn clear_dirty(&mut self) {
        self.dirty.clear_dirty();
    }

    /// Map an MMIO region on the persistent [`PhysicalMemoryBus`] exactly once.
    ///
    /// The machine's physical memory bus lives across `Machine::reset()` calls, so MMIO mappings
    /// are expected to be persistent. Callers may invoke this during every reset; identical
    /// mappings are treated as idempotent, while unexpected overlaps still panic to avoid silently
    /// corrupting the address space.
    #[allow(dead_code)]
    fn map_mmio_once<F>(&mut self, start: u64, len: u64, build: F)
    where
        F: FnOnce() -> Box<dyn memory::MmioHandler>,
    {
        if len == 0 {
            return;
        }

        let end = start
            .checked_add(len)
            .unwrap_or_else(|| panic!("MMIO mapping overflow at 0x{start:016x} (len=0x{len:x})"));

        // Fast path: mapping already exists.
        if self
            .inner
            .borrow()
            .mmio_regions()
            .iter()
            .any(|r| r.start == start && r.end == end)
        {
            return;
        }

        let handler = build();
        let mut inner = self.inner.borrow_mut();
        match inner.map_mmio(start, len, handler) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                // Treat identical overlaps as idempotent, but reject unexpected overlaps to avoid
                // silently corrupting the bus.
                let already_mapped = inner
                    .mmio_regions()
                    .iter()
                    .any(|r| r.start == start && r.end == end);
                if !already_mapped {
                    panic!("unexpected MMIO mapping overlap at 0x{start:016x} (len=0x{len:x})");
                }
            }
            Err(MapError::AddressOverflow) => {
                panic!("MMIO mapping overflow at 0x{start:016x} (len=0x{len:x})")
            }
        }
    }
}

impl A20Gate for SystemMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20.set_enabled(enabled);
    }

    fn a20_enabled(&self) -> bool {
        self.a20.enabled()
    }
}

impl FirmwareMemory for SystemMemory {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        let mut inner = self.inner.borrow_mut();
        match inner.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                // BIOS resets may re-map the same ROM windows. Treat identical overlaps as
                // idempotent, but reject unexpected overlaps to avoid silently corrupting the bus.
                let already_mapped = inner
                    .rom_regions()
                    .iter()
                    .any(|r| r.start == base && r.data.len() == len);
                if !already_mapped {
                    panic!("unexpected ROM mapping overlap at 0x{base:016x}");
                }
            }
            Err(MapError::AddressOverflow) => {
                panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})")
            }
        }
    }
}

impl memory::MemoryBus for SystemMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if self.a20.enabled() {
            self.inner.borrow_mut().read_physical(paddr, buf);
            return;
        }

        let mut inner = self.inner.borrow_mut();
        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            *slot = inner.read_physical_u8(addr);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if self.a20.enabled() {
            self.inner.borrow_mut().write_physical(paddr, buf);
            return;
        }

        let mut inner = self.inner.borrow_mut();
        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            inner.write_physical_u8(addr, byte);
        }
    }
}

impl aero_mmu::MemoryBus for SystemMemory {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        memory::MemoryBus::read_u8(self, paddr)
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        memory::MemoryBus::read_u16(self, paddr)
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        memory::MemoryBus::read_u32(self, paddr)
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        memory::MemoryBus::read_u64(self, paddr)
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        memory::MemoryBus::write_u8(self, paddr, value)
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        memory::MemoryBus::write_u16(self, paddr, value)
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        memory::MemoryBus::write_u32(self, paddr, value)
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        memory::MemoryBus::write_u64(self, paddr, value)
    }
}

struct E1000PciConfigDevice {
    cfg: aero_devices::pci::PciConfigSpace,
}

impl E1000PciConfigDevice {
    fn new() -> Self {
        Self {
            cfg: aero_devices::pci::profile::NIC_E1000_82540EM.build_config_space(),
        }
    }
}

impl PciDevice for E1000PciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.cfg
    }
}

// -----------------------------------------------------------------------------
// PC platform MMIO adapters (LAPIC / IOAPIC / HPET)
// -----------------------------------------------------------------------------

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

struct E1000PciIoBar {
    dev: Rc<RefCell<E1000Device>>,
}

impl PciIoBarHandler for E1000PciIoBar {
    fn io_read(&mut self, offset: u64, size: usize) -> u32 {
        let offset = u32::try_from(offset).unwrap_or(0);
        self.dev.borrow_mut().io_read(offset, size)
    }

    fn io_write(&mut self, offset: u64, size: usize, value: u32) {
        let offset = u32::try_from(offset).unwrap_or(0);
        self.dev.borrow_mut().io_write_reg(offset, size, value);
    }
}

struct PciIoBarWindow {
    router: PciIoBarRouter,
}

impl PciIoBarWindow {
    fn read_all_ones(size: u8) -> u32 {
        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

impl aero_platform::io::PortIoDevice for PciIoBarWindow {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        let size_usize = match size {
            1 | 2 | 4 => size as usize,
            _ => return Self::read_all_ones(size),
        };
        self.router
            .dispatch_read(port, size_usize)
            .unwrap_or_else(|| Self::read_all_ones(size))
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        let size_usize = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };
        let _ = self.router.dispatch_write(port, size_usize, value);
    }
}

/// Canonical Aero machine: CPU + physical memory + port I/O devices + firmware.
pub struct Machine {
    cfg: MachineConfig,
    chipset: ChipsetState,
    reset_latch: ResetLatch,

    cpu: CpuCore,
    assist: AssistContext,
    mmu: aero_mmu::Mmu,
    mem: SystemMemory,
    io: IoPortBus,

    // Optional PC platform devices. These are behind `Rc<RefCell<_>>` so their host wiring
    // survives snapshot restore (devices reset their internal state but preserve callbacks/irq
    // lines).
    platform_clock: Option<ManualClock>,
    interrupts: Option<Rc<RefCell<PlatformInterrupts>>>,
    pit: Option<SharedPit8254>,
    rtc: Option<SharedRtcCmos<ManualClock, PlatformIrqLine>>,
    pci_cfg: Option<SharedPciConfigPorts>,
    pci_intx: Option<Rc<RefCell<PciIntxRouter>>>,
    acpi_pm: Option<SharedAcpiPmIo<ManualClock>>,
    hpet: Option<Rc<RefCell<hpet::Hpet<ManualClock>>>>,
    e1000: Option<Rc<RefCell<E1000Device>>>,

    bios: Bios,
    disk: VecBlockDevice,
    network_backend: Option<Box<dyn NetworkBackend>>,

    serial: Option<SharedSerial16550>,
    i8042: Option<SharedI8042Controller>,
    serial_log: Vec<u8>,
    ps2_mouse_buttons: u8,

    next_snapshot_id: u64,
    last_snapshot_id: Option<u64>,

    /// Deterministic guest time accumulator used when converting CPU cycles (TSC ticks) into
    /// nanoseconds for platform device ticking.
    guest_time: GuestTime,
}

impl Machine {
    pub fn new(cfg: MachineConfig) -> Result<Self, MachineError> {
        if cfg.cpu_count != 1 {
            return Err(MachineError::InvalidCpuCount(cfg.cpu_count));
        }
        if cfg.enable_e1000 && !cfg.enable_pc_platform {
            return Err(MachineError::E1000RequiresPcPlatform);
        }

        let chipset = ChipsetState::new(false);
        let mem = SystemMemory::new(cfg.ram_size_bytes, chipset.a20())?;

        let mut machine = Self {
            cfg,
            chipset,
            reset_latch: ResetLatch::new(),
            cpu: CpuCore::new(CpuMode::Real),
            assist: AssistContext::default(),
            mmu: aero_mmu::Mmu::new(),
            mem,
            io: IoPortBus::new(),
            platform_clock: None,
            interrupts: None,
            pit: None,
            rtc: None,
            pci_cfg: None,
            pci_intx: None,
            acpi_pm: None,
            hpet: None,
            e1000: None,
            bios: Bios::new(BiosConfig::default()),
            disk: VecBlockDevice::new(Vec::new()).expect("empty disk is valid"),
            network_backend: None,
            serial: None,
            i8042: None,
            serial_log: Vec::new(),
            ps2_mouse_buttons: 0,
            next_snapshot_id: 1,
            last_snapshot_id: None,
            guest_time: GuestTime::default(),
        };

        machine.reset();
        Ok(machine)
    }

    fn map_pc_platform_mmio_regions(&mut self) {
        if !self.cfg.enable_pc_platform {
            return;
        }

        let (Some(interrupts), Some(hpet), Some(pci_cfg)) = (&self.interrupts, &self.hpet, &self.pci_cfg) else {
            return;
        };

        let interrupts = interrupts.clone();
        let hpet = hpet.clone();
        let pci_cfg = pci_cfg.clone();

        self.mem.map_mmio_once(LAPIC_MMIO_BASE, LAPIC_MMIO_SIZE, || {
            Box::new(LapicMmio {
                interrupts: interrupts.clone(),
            })
        });
        self.mem.map_mmio_once(IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, || {
            Box::new(IoApicMmio {
                interrupts: interrupts.clone(),
            })
        });
        self.mem.map_mmio_once(hpet::HPET_MMIO_BASE, hpet::HPET_MMIO_SIZE, || {
            Box::new(HpetMmio {
                hpet: hpet.clone(),
                interrupts: interrupts.clone(),
            })
        });

        let ecam_cfg = PciEcamConfig {
            segment: firmware::bios::PCIE_ECAM_SEGMENT,
            start_bus: firmware::bios::PCIE_ECAM_START_BUS,
            end_bus: firmware::bios::PCIE_ECAM_END_BUS,
        };
        let ecam_len = ecam_cfg.window_size_bytes();
        self.mem.map_mmio_once(firmware::bios::PCIE_ECAM_BASE, ecam_len, || {
            Box::new(PciEcamMmio::new(pci_cfg, ecam_cfg))
        });
    }

    /// Returns the current CPU state.
    pub fn cpu(&self) -> &CpuState {
        &self.cpu.state
    }

    /// Mutable access to the current CPU state (debug/testing only).
    pub fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu.state
    }

    /// Replace the attached disk image.
    pub fn set_disk_image(&mut self, bytes: Vec<u8>) -> Result<(), MachineError> {
        self.disk = VecBlockDevice::new(bytes)?;
        Ok(())
    }

    /// Install/replace the host-side network backend used by any emulated NICs.
    ///
    /// Note: this backend is *external* state (e.g. a live tunnel connection) and is intentionally
    /// not included in snapshots. Callers should either:
    /// - re-attach after restoring a snapshot, or
    /// - call [`Machine::detach_network`] before snapshotting to make the lifecycle explicit.
    pub fn set_network_backend(&mut self, backend: Box<dyn NetworkBackend>) {
        self.network_backend = Some(backend);
    }

    /// Attach a ring-buffer-backed L2 tunnel network backend (NET_TX / NET_RX).
    pub fn attach_l2_tunnel_rings<TX: FrameRing + 'static, RX: FrameRing + 'static>(
        &mut self,
        tx: TX,
        rx: RX,
    ) {
        self.set_network_backend(Box::new(L2TunnelRingBackend::new(tx, rx)));
    }

    /// Convenience for native callers using [`aero_ipc::ring::RingBuffer`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn attach_l2_tunnel_rings_native(
        &mut self,
        tx: aero_ipc::ring::RingBuffer,
        rx: aero_ipc::ring::RingBuffer,
    ) {
        self.attach_l2_tunnel_rings(tx, rx);
    }

    /// Convenience for WASM/browser callers using [`aero_ipc::wasm::SharedRingBuffer`].
    #[cfg(target_arch = "wasm32")]
    pub fn attach_l2_tunnel_rings_wasm(
        &mut self,
        tx: aero_ipc::wasm::SharedRingBuffer,
        rx: aero_ipc::wasm::SharedRingBuffer,
    ) {
        self.attach_l2_tunnel_rings(tx, rx);
    }

    /// Detach (drop) any currently installed network backend.
    pub fn detach_network(&mut self) {
        self.network_backend = None;
    }

    /// Return statistics for the currently attached `NET_TX`/`NET_RX` ring backend (if present).
    pub fn network_backend_l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        self.network_backend
            .as_ref()
            .and_then(|backend| backend.l2_ring_stats())
    }

    /// Debug/testing helper: read a single guest physical byte.
    pub fn read_physical_u8(&mut self, paddr: u64) -> u8 {
        self.mem.read_u8(paddr)
    }

    /// Debug/testing helper: read a little-endian u16 from guest physical memory.
    pub fn read_physical_u16(&mut self, paddr: u64) -> u16 {
        self.mem.read_u16(paddr)
    }

    /// Debug/testing helper: read a little-endian u32 from guest physical memory.
    pub fn read_physical_u32(&mut self, paddr: u64) -> u32 {
        self.mem.read_u32(paddr)
    }

    /// Debug/testing helper: read a little-endian u64 from guest physical memory.
    pub fn read_physical_u64(&mut self, paddr: u64) -> u64 {
        self.mem.read_u64(paddr)
    }

    /// Debug/testing helper: read a range of guest physical memory into a new buffer.
    pub fn read_physical_bytes(&mut self, paddr: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.mem.read_physical(paddr, &mut out);
        out
    }

    /// Debug/testing helper: write a single guest physical byte.
    pub fn write_physical_u8(&mut self, paddr: u64, value: u8) {
        self.mem.write_u8(paddr, value);
    }

    /// Debug/testing helper: write a little-endian u16 to guest physical memory.
    pub fn write_physical_u16(&mut self, paddr: u64, value: u16) {
        self.mem.write_u16(paddr, value);
    }

    /// Debug/testing helper: write a little-endian u32 to guest physical memory.
    pub fn write_physical_u32(&mut self, paddr: u64, value: u32) {
        self.mem.write_u32(paddr, value);
    }

    /// Debug/testing helper: write a little-endian u64 to guest physical memory.
    pub fn write_physical_u64(&mut self, paddr: u64, value: u64) {
        self.mem.write_u64(paddr, value);
    }

    /// Debug/testing helper: write a slice into guest physical memory.
    pub fn write_physical(&mut self, paddr: u64, data: &[u8]) {
        self.mem.write_physical(paddr, data);
    }

    /// Debug/testing helper: read from an I/O port.
    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        self.io.read(port, size)
    }

    /// Debug/testing helper: write to an I/O port.
    pub fn io_write(&mut self, port: u16, size: u8, value: u32) {
        self.io.write(port, size, value);
    }

    /// Returns the shared manual clock backing platform timer devices, if the PC platform is
    /// enabled.
    pub fn platform_clock(&self) -> Option<ManualClock> {
        self.platform_clock.clone()
    }

    /// Returns the platform interrupt controller complex (PIC + IOAPIC + LAPIC), if present.
    pub fn platform_interrupts(&self) -> Option<Rc<RefCell<PlatformInterrupts>>> {
        self.interrupts.clone()
    }

    /// Returns the PCI config mechanism #1 ports device, if present.
    pub fn pci_config_ports(&self) -> Option<SharedPciConfigPorts> {
        self.pci_cfg.clone()
    }

    /// Returns the PCI INTx router, if present.
    pub fn pci_intx_router(&self) -> Option<Rc<RefCell<PciIntxRouter>>> {
        self.pci_intx.clone()
    }

    /// Returns the PIT 8254 device, if present.
    pub fn pit(&self) -> Option<SharedPit8254> {
        self.pit.clone()
    }

    /// Returns the RTC CMOS device, if present.
    pub fn rtc(&self) -> Option<SharedRtcCmos<ManualClock, PlatformIrqLine>> {
        self.rtc.clone()
    }

    /// Returns the ACPI PM I/O device, if present.
    pub fn acpi_pm(&self) -> Option<SharedAcpiPmIo<ManualClock>> {
        self.acpi_pm.clone()
    }

    /// Returns the HPET device, if present.
    pub fn hpet(&self) -> Option<Rc<RefCell<hpet::Hpet<ManualClock>>>> {
        self.hpet.clone()
    }

    /// Returns the E1000 NIC device, if present.
    pub fn e1000(&self) -> Option<Rc<RefCell<E1000Device>>> {
        self.e1000.clone()
    }

    /// Advance deterministic platform time and poll any timer devices.
    ///
    /// This is used by [`Machine::run_slice`] to keep PIT/RTC/HPET/LAPIC timers progressing
    /// deterministically (based on executed CPU cycles, including while the CPU is halted), and
    /// is also exposed for tests and debugging.
    pub fn tick_platform(&mut self, delta_ns: u64) {
        if delta_ns == 0 {
            return;
        }
        if let Some(clock) = &self.platform_clock {
            clock.advance_ns(delta_ns);
        }

        if let Some(acpi_pm) = &self.acpi_pm {
            acpi_pm.borrow_mut().advance_ns(delta_ns);
        }

        if let Some(pit) = &self.pit {
            pit.borrow_mut().advance_ns(delta_ns);
        }

        if let Some(rtc) = &self.rtc {
            rtc.borrow_mut().tick();
        }

        if let Some(interrupts) = &self.interrupts {
            interrupts.borrow().tick(delta_ns);
        }

        if let (Some(hpet), Some(interrupts)) = (&self.hpet, &self.interrupts) {
            let mut hpet = hpet.borrow_mut();
            let mut interrupts = interrupts.borrow_mut();
            hpet.poll(&mut *interrupts);
        }
    }

    fn tick_platform_from_cycles(&mut self, cycles: u64) {
        if self.platform_clock.is_none() {
            return;
        }

        if cycles == 0 {
            return;
        }

        let tsc_hz = self.cpu.time.tsc_hz();
        if tsc_hz == 0 {
            return;
        }

        if self.guest_time.cpu_hz() != tsc_hz {
            // If the caller changes the deterministic TSC frequency, preserve continuity by
            // resynchronizing the fractional remainder from the pre-batch TSC value.
            let tsc_before = self.cpu.state.msr.tsc.wrapping_sub(cycles);
            self.guest_time = GuestTime::new(tsc_hz);
            self.guest_time.resync_from_tsc(tsc_before);
        }

        let delta_ns = self.guest_time.advance_guest_time_for_instructions(cycles);
        if delta_ns != 0 {
            self.tick_platform(delta_ns);
        }
    }

    fn idle_tick_platform_1ms(&mut self) {
        if self.platform_clock.is_none() {
            return;
        }

        // Only tick while halted when maskable interrupts are enabled; otherwise HLT is expected to
        // be terminal (until NMI/SMI/reset, which we do not model yet).
        if (self.cpu.state.rflags() & aero_cpu_core::state::RFLAGS_IF) == 0 {
            return;
        }

        let tsc_hz = self.cpu.time.tsc_hz();
        if tsc_hz == 0 {
            return;
        }

        // Advance 1ms worth of CPU cycles while halted so timer devices can wake the CPU.
        let cycles = (tsc_hz / 1000).max(1);
        self.cpu.time.advance_cycles(cycles);
        self.cpu.state.msr.tsc = self.cpu.time.read_tsc();
        self.tick_platform_from_cycles(cycles);
    }

    fn resync_guest_time_from_tsc(&mut self) {
        let tsc_hz = self.cpu.time.tsc_hz();
        if self.guest_time.cpu_hz() != tsc_hz {
            self.guest_time = GuestTime::new(tsc_hz);
        }
        self.guest_time.resync_from_tsc(self.cpu.state.msr.tsc);
    }

    fn sync_pci_intx_sources_to_interrupts(&mut self) {
        let (Some(pci_intx), Some(interrupts)) = (&self.pci_intx, &self.interrupts) else {
            return;
        };

        // E1000 legacy INTx (level-triggered).
        if let Some(e1000) = &self.e1000 {
            let bdf: PciBdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
            let pin = PciInterruptPin::IntA;

            let mut level = e1000.borrow().irq_level();

            // Respect PCI command register Interrupt Disable bit (bit 10).
            if let Some(pci_cfg) = &self.pci_cfg {
                let intx_disabled = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    pci_cfg
                        .bus_mut()
                        .device_config(bdf)
                        .is_some_and(|cfg| (cfg.command() & (1 << 10)) != 0)
                };
                if intx_disabled {
                    level = false;
                }
            }

            let mut pci_intx = pci_intx.borrow_mut();
            let mut interrupts = interrupts.borrow_mut();
            pci_intx.set_intx_level(bdf, pin, level, &mut *interrupts);
        }
    }
    /// Take (drain) all serial output accumulated so far.
    pub fn take_serial_output(&mut self) -> Vec<u8> {
        self.flush_serial();
        std::mem::take(&mut self.serial_log)
    }

    /// Return a copy of the serial output accumulated so far without draining it.
    ///
    /// This is intentionally a cloning API: callers that only need a byte count should prefer
    /// [`Machine::serial_output_len`].
    pub fn serial_output_bytes(&mut self) -> Vec<u8> {
        self.flush_serial();
        self.serial_log.clone()
    }

    /// Return the number of bytes currently buffered in the serial output log.
    ///
    /// This is a cheap alternative to [`Machine::take_serial_output`] for callers that only need a
    /// byte count (e.g. UI progress indicators) and want to avoid copying large buffers.
    pub fn serial_output_len(&mut self) -> u64 {
        self.flush_serial();
        u64::try_from(self.serial_log.len()).unwrap_or(u64::MAX)
    }

    /// Inject a browser-style keyboard code into the i8042 controller, if present.
    pub fn inject_browser_key(&mut self, code: &str, pressed: bool) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_browser_key(code, pressed);
        }
    }

    /// Inject relative mouse motion into the i8042 controller, if present.
    ///
    /// `dx` is positive to the right and `dy` is positive down (browser-style). The underlying PS/2
    /// mouse model converts this into PS/2 packet coordinates (+Y is up).
    pub fn inject_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_mouse_motion(dx, dy, wheel);
        }
    }

    /// Inject a PS/2 mouse button transition into the i8042 controller, if present.
    pub fn inject_mouse_button(&mut self, button: Ps2MouseButton, pressed: bool) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_mouse_button(button, pressed);
        }
    }

    pub fn inject_mouse_left(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Left, pressed);
    }

    pub fn inject_mouse_right(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Right, pressed);
    }

    pub fn inject_mouse_middle(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Middle, pressed);
    }

    /// Inject a PS/2 mouse motion event into the i8042 controller, if present.
    ///
    /// Coordinate conventions:
    /// - `dy > 0` means cursor moved up.
    /// - `wheel > 0` means wheel moved up.
    pub fn inject_ps2_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        // `aero_devices_input::Ps2Mouse` expects browser-style +Y=down internally.
        self.inject_mouse_motion(dx, -dy, wheel);
    }

    /// Inject a PS/2 mouse button state into the i8042 controller, if present.
    ///
    /// `buttons` is a bitmask:
    /// - bit 0: left
    /// - bit 1: right
    /// - bit 2: middle
    pub fn inject_ps2_mouse_buttons(&mut self, buttons: u8) {
        let buttons = buttons & 0x07;
        // `ps2_mouse_buttons` is a host-side cache used to compute transitions. Certain lifecycle
        // events (e.g. snapshot restore) can make the cached value stale relative to the guest
        // device state; in that case we "invalidate" the cache by setting any bits outside the
        // 3-button mask and force a full resync on the next injection call.
        let force = (self.ps2_mouse_buttons & !0x07) != 0;
        let changed = (self.ps2_mouse_buttons ^ buttons) & 0x07;
        if !force && changed == 0 {
            return;
        }

        if force || (changed & 0x01) != 0 {
            self.inject_mouse_button(Ps2MouseButton::Left, (buttons & 0x01) != 0);
        }
        if force || (changed & 0x02) != 0 {
            self.inject_mouse_button(Ps2MouseButton::Right, (buttons & 0x02) != 0);
        }
        if force || (changed & 0x04) != 0 {
            self.inject_mouse_button(Ps2MouseButton::Middle, (buttons & 0x04) != 0);
        }

        self.ps2_mouse_buttons = buttons;
    }

    pub fn take_snapshot_full(&mut self) -> snapshot::Result<Vec<u8>> {
        self.take_snapshot_with_options(snapshot::SaveOptions::default())
    }

    pub fn save_snapshot_full_to<W: Write + Seek>(&mut self, w: &mut W) -> snapshot::Result<()> {
        self.save_snapshot_to(w, snapshot::SaveOptions::default())
    }

    pub fn take_snapshot_dirty(&mut self) -> snapshot::Result<Vec<u8>> {
        let mut options = snapshot::SaveOptions::default();
        options.ram.mode = snapshot::RamMode::Dirty;
        self.take_snapshot_with_options(options)
    }

    pub fn save_snapshot_dirty_to<W: Write + Seek>(&mut self, w: &mut W) -> snapshot::Result<()> {
        let mut options = snapshot::SaveOptions::default();
        options.ram.mode = snapshot::RamMode::Dirty;
        self.save_snapshot_to(w, options)
    }

    pub fn restore_snapshot_bytes(&mut self, bytes: &[u8]) -> snapshot::Result<()> {
        self.restore_snapshot_from_checked(&mut Cursor::new(bytes))
    }

    pub fn restore_snapshot_from<R: Read>(&mut self, r: &mut R) -> snapshot::Result<()> {
        snapshot::restore_snapshot(r, self)
    }

    pub fn restore_snapshot_from_checked<R: Read + Seek>(
        &mut self,
        r: &mut R,
    ) -> snapshot::Result<()> {
        // Restoring a snapshot is conceptually "rewinding time", so discard any accumulated host
        // output/state from the current execution.
        self.detach_network();
        self.flush_serial();
        if let Some(uart) = &self.serial {
            let _ = uart.borrow_mut().take_tx();
        }
        self.serial_log.clear();
        self.reset_latch.clear();

        let expected_parent_snapshot_id = self.last_snapshot_id;
        snapshot::restore_snapshot_with_options(
            r,
            self,
            snapshot::RestoreOptions {
                expected_parent_snapshot_id,
            },
        )
    }

    fn save_snapshot_to<W: Write + Seek>(
        &mut self,
        w: &mut W,
        options: snapshot::SaveOptions,
    ) -> snapshot::Result<()> {
        self.flush_serial();
        snapshot::save_snapshot(w, self, options)
    }

    fn take_snapshot_with_options(
        &mut self,
        options: snapshot::SaveOptions,
    ) -> snapshot::Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        self.save_snapshot_to(&mut cursor, options)?;
        Ok(cursor.into_inner())
    }

    /// Reset the machine and transfer control to firmware POST (boot sector).
    pub fn reset(&mut self) {
        self.reset_latch.clear();
        self.serial_log.clear();
        self.ps2_mouse_buttons = 0;
        self.guest_time.reset();

        // Reset chipset lines.
        self.chipset.a20().set_enabled(false);

        // Rebuild port I/O devices for deterministic power-on state.
        self.io = IoPortBus::new();

        if self.cfg.enable_pc_platform {
            // PC platform shared device instances must remain stable across resets because MMIO
            // mappings in the physical memory bus persist. Reset device state in-place while
            // keeping `Rc` identities stable.

            // Deterministic clock: reset back to 0 ns.
            let clock = match &self.platform_clock {
                Some(clock) => {
                    clock.set_ns(0);
                    clock.clone()
                }
                None => {
                    let clock = ManualClock::new();
                    self.platform_clock = Some(clock.clone());
                    clock
                }
            };

            // Interrupt controller complex (PIC + IOAPIC + LAPIC).
            let interrupts: Rc<RefCell<PlatformInterrupts>> = match &self.interrupts {
                Some(ints) => {
                    ints.borrow_mut().reset();
                    ints.clone()
                }
                None => {
                    let ints = Rc::new(RefCell::new(PlatformInterrupts::new()));
                    self.interrupts = Some(ints.clone());
                    ints
                }
            };

            PlatformInterrupts::register_imcr_ports(&mut self.io, interrupts.clone());
            register_pic8259_on_platform_interrupts(&mut self.io, interrupts.clone());

            // PIT 8254.
            let pit: SharedPit8254 = match &self.pit {
                Some(pit) => {
                    *pit.borrow_mut() = Pit8254::new();
                    pit.clone()
                }
                None => {
                    let pit: SharedPit8254 = Rc::new(RefCell::new(Pit8254::new()));
                    self.pit = Some(pit.clone());
                    pit
                }
            };
            pit.borrow_mut()
                .connect_irq0_to_platform_interrupts(interrupts.clone());
            register_pit8254(&mut self.io, pit.clone());

            // RTC CMOS.
            let rtc_irq8 = PlatformIrqLine::isa(interrupts.clone(), 8);
            let rtc: SharedRtcCmos<ManualClock, PlatformIrqLine> = match &self.rtc {
                Some(rtc) => {
                    *rtc.borrow_mut() = RtcCmos::new(clock.clone(), rtc_irq8);
                    rtc.clone()
                }
                None => {
                    let rtc: SharedRtcCmos<ManualClock, PlatformIrqLine> =
                        Rc::new(RefCell::new(RtcCmos::new(clock.clone(), rtc_irq8)));
                    self.rtc = Some(rtc.clone());
                    rtc
                }
            };
            rtc.borrow_mut()
                .set_memory_size_bytes(self.cfg.ram_size_bytes);
            register_rtc_cmos(&mut self.io, rtc.clone());

            // ACPI PM. Wire SCI to ISA IRQ9.
            let acpi_pm: SharedAcpiPmIo<ManualClock> = match &self.acpi_pm {
                Some(acpi_pm) => {
                    acpi_pm.borrow_mut().reset();
                    acpi_pm.clone()
                }
                None => {
                    // Wire ACPI PM to the shared deterministic platform clock so `PM_TMR`
                    // progresses only when the host advances `ManualClock` (via
                    // `Machine::tick_platform`).
                    let acpi_pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
                        AcpiPmConfig::default(),
                        AcpiPmCallbacks {
                            sci_irq: Box::new(PlatformIrqLine::isa(interrupts.clone(), 9)),
                            request_power_off: None,
                        },
                        clock.clone(),
                    )));
                    self.acpi_pm = Some(acpi_pm.clone());
                    acpi_pm
                }
            };
            register_acpi_pm(&mut self.io, acpi_pm.clone());

            // PCI config ports (config mechanism #1).
            let pci_cfg: SharedPciConfigPorts = match &self.pci_cfg {
                Some(pci_cfg) => {
                    *pci_cfg.borrow_mut() = PciConfigPorts::new();
                    pci_cfg.clone()
                }
                None => {
                    let pci_cfg: SharedPciConfigPorts =
                        Rc::new(RefCell::new(PciConfigPorts::new()));
                    self.pci_cfg = Some(pci_cfg.clone());
                    pci_cfg
                }
            };
            register_pci_config_ports(&mut self.io, pci_cfg.clone());

            // PCI INTx router.
            let pci_intx: Rc<RefCell<PciIntxRouter>> = match &self.pci_intx {
                Some(pci_intx) => {
                    *pci_intx.borrow_mut() = PciIntxRouter::new(PciIntxRouterConfig::default());
                    pci_intx.clone()
                }
                None => {
                    let pci_intx = Rc::new(RefCell::new(PciIntxRouter::new(
                        PciIntxRouterConfig::default(),
                    )));
                    self.pci_intx = Some(pci_intx.clone());
                    pci_intx
                }
            };

            // HPET.
            let hpet: Rc<RefCell<hpet::Hpet<ManualClock>>> = match &self.hpet {
                Some(hpet) => {
                    *hpet.borrow_mut() = hpet::Hpet::new_default(clock.clone());
                    hpet.clone()
                }
                None => {
                    let hpet = Rc::new(RefCell::new(hpet::Hpet::new_default(clock.clone())));
                    self.hpet = Some(hpet.clone());
                    hpet
                }
            };

            let e1000 = if self.cfg.enable_e1000 {
                let mac = self.cfg.e1000_mac_addr.unwrap_or(DEFAULT_E1000_MAC_ADDR);
                pci_cfg.borrow_mut().bus_mut().add_device(
                    aero_devices::pci::profile::NIC_E1000_82540EM.bdf,
                    Box::new(E1000PciConfigDevice::new()),
                );

                match &self.e1000 {
                    Some(e1000) => {
                        // Reset in-place while keeping the `Rc` identity stable for any persistent
                        // MMIO mappings.
                        *e1000.borrow_mut() = E1000Device::new(mac);
                        Some(e1000.clone())
                    }
                    None => Some(Rc::new(RefCell::new(E1000Device::new(mac)))),
                }
            } else {
                None
            };

            // Allocate PCI BAR resources and enable decoding so devices are reachable via MMIO/PIO
            // immediately after reset (without requiring the guest OS to assign BARs first).
            let pci_allocator_cfg = PciResourceAllocatorConfig::default();
            {
                let mut pci_cfg = pci_cfg.borrow_mut();
                let mut allocator = PciResourceAllocator::new(pci_allocator_cfg.clone());
                // `bios_post` is deterministic and keeps existing fixed BAR bases intact.
                bios_post(pci_cfg.bus_mut(), &mut allocator)
                    .expect("PCI BIOS POST resource assignment should succeed");
            }

            // Map the PCI MMIO window used by `PciResourceAllocator` so BAR relocation is reflected
            // immediately without needing dynamic MMIO unmap/remap support in `PhysicalMemoryBus`.
            self.mem
                .map_mmio_once(pci_allocator_cfg.mmio_base, pci_allocator_cfg.mmio_size, || {
                    let mut router = PciBarMmioRouter::new(pci_allocator_cfg.mmio_base, pci_cfg.clone());
                    if let Some(e1000) = e1000.clone() {
                        router.register_shared_handler(
                            aero_devices::pci::profile::NIC_E1000_82540EM.bdf,
                            0,
                            e1000,
                        );
                    }
                    Box::new(router)
                });

            // Register a dispatcher for the PCI I/O window used by `PciResourceAllocator`.
            //
            // The router consults the live PCI config space on each access, so BAR programming and
            // command register gating take effect immediately.
            let io_base = u16::try_from(pci_allocator_cfg.io_base)
                .expect("PCI IO window base must fit in u16");
            let io_size = u16::try_from(pci_allocator_cfg.io_size)
                .expect("PCI IO window size must fit in u16");
            let mut io_router = PciIoBarRouter::new(pci_cfg.clone());
            if let Some(e1000) = e1000.clone() {
                io_router.register_handler(
                    aero_devices::pci::profile::NIC_E1000_82540EM.bdf,
                    1,
                    E1000PciIoBar { dev: e1000 },
                );
            }
            self.io.register_range(io_base, io_size, Box::new(PciIoBarWindow { router: io_router }));

            // Ensure options stay populated (for the first reset).
            self.platform_clock = Some(clock);
            self.interrupts = Some(interrupts);
            self.pit = Some(pit);
            self.rtc = Some(rtc);
            self.pci_cfg = Some(pci_cfg);
            self.pci_intx = Some(pci_intx);
            self.acpi_pm = Some(acpi_pm);
            self.hpet = Some(hpet);
            self.e1000 = e1000;

            // MMIO mappings persist in the physical bus; ensure the canonical PC regions exist.
            self.map_pc_platform_mmio_regions();
        } else {
            self.platform_clock = None;
            self.interrupts = None;
            self.pit = None;
            self.rtc = None;
            self.pci_cfg = None;
            self.pci_intx = None;
            self.acpi_pm = None;
            self.hpet = None;
            self.e1000 = None;
        }

        if self.cfg.enable_serial {
            let uart: SharedSerial16550 = Rc::new(RefCell::new(Serial16550::new(0x3F8)));
            register_serial16550(&mut self.io, uart.clone());
            self.serial = Some(uart);
        } else {
            self.serial = None;
        }

        if self.cfg.enable_a20_gate {
            let dev = A20GateDevice::with_reset_sink(self.chipset.a20(), self.reset_latch.clone());
            self.io.register(FAST_A20_PORT, Box::new(dev));
        }

        if self.cfg.enable_reset_ctrl {
            self.io.register(
                RESET_CTRL_PORT,
                Box::new(ResetCtrl::new(self.reset_latch.clone())),
            );
        }

        if self.cfg.enable_i8042 {
            let ports = I8042Ports::new();
            // If the PC platform interrupt controller is enabled, wire i8042 IRQ1/IRQ12 pulses
            // into it so the guest can receive keyboard/mouse interrupts.
            if let Some(interrupts) = &self.interrupts {
                ports.connect_irqs_to_platform_interrupts(interrupts.clone());
            }
            let ctrl = ports.controller();
            aero_devices::i8042::register_i8042(&mut self.io, ctrl.clone());

            ctrl.borrow_mut().set_system_control_sink(Box::new(
                aero_devices::i8042::PlatformSystemControlSink::with_reset_sink(
                    self.chipset.a20(),
                    self.reset_latch.clone(),
                ),
            ));

            self.i8042 = Some(ctrl);
        } else {
            self.i8042 = None;
        }

        self.assist = AssistContext::default();
        self.cpu = CpuCore::new(CpuMode::Real);
        self.guest_time = GuestTime::new_from_cpu(&self.cpu);
        self.mmu = aero_mmu::Mmu::new();

        // Run firmware POST (in Rust) to initialize IVT/BDA, map BIOS stubs, and load the boot
        // sector into RAM.
        self.bios = Bios::new(BiosConfig {
            memory_size_bytes: self.cfg.ram_size_bytes,
            cpu_count: self.cfg.cpu_count,
            ..Default::default()
        });
        let bus: &mut dyn BiosBus = &mut self.mem;
        if let Some(pci_cfg) = &self.pci_cfg {
            let mut pci = SharedPciConfigPortsBiosAdapter::new(pci_cfg.clone());
            self.bios
                .post_with_pci(&mut self.cpu.state, bus, &mut self.disk, Some(&mut pci));
        } else {
            self.bios.post(&mut self.cpu.state, bus, &mut self.disk);
        }
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        self.mem.clear_dirty();
    }

    /// Poll the E1000 + network backend bridge once.
    ///
    /// This is safe to call even when E1000 is disabled; it will no-op.
    pub fn poll_network(&mut self) {
        let Some(e1000) = &self.e1000 else {
            return;
        };

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let command = self
            .pci_cfg
            .as_ref()
            .and_then(|pci_cfg| {
                let mut pci_cfg = pci_cfg.borrow_mut();
                pci_cfg.bus_mut().device_config(bdf).map(|cfg| cfg.command())
            })
            .unwrap_or(0);

        // Keep the device model's internal PCI command register in sync with the platform PCI bus.
        //
        // The E1000 model gates DMA on COMMAND.BME (bit 2) by consulting its own PCI config state,
        // while the machine maintains a separate canonical config space for enumeration.
        //
        // The shared `aero-net-pump` helper assumes the NIC's internal PCI command state is already
        // up to date.
        let mut nic = e1000.borrow_mut();
        nic.pci_config_write(0x04, 2, u32::from(command));

        const MAX_FRAMES_PER_POLL: usize = aero_net_pump::DEFAULT_MAX_FRAMES_PER_POLL;
        if let Some(backend) = self.network_backend.as_mut() {
            tick_e1000(
                &mut *nic,
                &mut self.mem,
                backend,
                MAX_FRAMES_PER_POLL,
                MAX_FRAMES_PER_POLL,
            );
        } else {
            // Keep the device model making forward progress even when no host network backend is
            // attached (e.g. for deterministic guest tests). Any guest TX frames are dropped.
            let mut no_backend = ();
            tick_e1000(
                &mut *nic,
                &mut self.mem,
                &mut no_backend,
                MAX_FRAMES_PER_POLL,
                MAX_FRAMES_PER_POLL,
            );
        }
    }

    /// Run the CPU for at most `max_insts` guest instructions.
    pub fn run_slice(&mut self, max_insts: u64) -> RunExit {
        let mut executed = 0u64;
        // Keep Tier-0 instruction gating coherent with the CPUID surface that assists expose to the
        // guest.
        let cfg = Tier0Config::from_cpuid(&self.assist.features);
        while executed < max_insts {
            if let Some(kind) = self.reset_latch.take() {
                self.flush_serial();
                return RunExit::ResetRequested { kind, executed };
            }

            // Keep the core's A20 view coherent with the chipset latch.
            self.cpu.state.a20_enabled = self.chipset.a20().enabled();

            self.poll_network();

            // Synchronize PCI INTx sources (e.g. E1000) into the platform interrupt controller
            // *before* we poll for pending vectors. This must happen even when the guest cannot
            // currently accept maskable interrupts (IF=0 / interrupt shadow) so level-triggered
            // lines remain asserted until delivery is possible.
            self.sync_pci_intx_sources_to_interrupts();

            // Poll the platform interrupt controller (PIC/IOAPIC+LAPIC) and enqueue at most one
            // pending external interrupt vector into the CPU core.
            //
            // Tier-0 only delivers interrupts that are already present in
            // `cpu.pending.external_interrupts`; it does not poll an interrupt controller itself.
            //
            // Keep this polling bounded so a level-triggered interrupt line that remains asserted
            // cannot cause an unbounded growth of the external interrupt FIFO when the guest has
            // interrupts masked (IF=0) or otherwise cannot accept delivery yet.
            const MAX_QUEUED_EXTERNAL_INTERRUPTS: usize = 1;
            let _ = self.poll_platform_interrupt(MAX_QUEUED_EXTERNAL_INTERRUPTS);

            let remaining = max_insts - executed;
            let mut bus = aero_cpu_core::PagingBus::new_with_io(&mut self.mem, &mut self.io);
            std::mem::swap(&mut self.mmu, bus.mmu_mut());

            let batch = run_batch_cpu_core_with_assists(
                &cfg,
                &mut self.assist,
                &mut self.cpu,
                &mut bus,
                remaining,
            );
            std::mem::swap(&mut self.mmu, bus.mmu_mut());
            executed = executed.saturating_add(batch.executed);

            // Deterministically advance platform time based on executed CPU cycles.
            self.tick_platform_from_cycles(batch.executed);

            match batch.exit {
                BatchExit::Completed => {
                    self.flush_serial();
                    return RunExit::Completed { executed };
                }
                BatchExit::Branch => continue,
                BatchExit::Halted => {
                    // After advancing timers, poll again so any newly-due timer interrupts are
                    // injected into `cpu.pending.external_interrupts`.
                    //
                    // Only poll after the batch when we are going to re-enter execution within the
                    // same `run_slice` call. This avoids acknowledging interrupts at the end of a
                    // slice boundary (e.g. after an `STI` interrupt shadow expires) when the CPU
                    // will not execute another instruction until the host calls `run_slice` again.
                    if self.poll_platform_interrupt(MAX_QUEUED_EXTERNAL_INTERRUPTS) {
                        continue;
                    }

                    // When halted, advance platform time so timer interrupts can wake the CPU.
                    self.idle_tick_platform_1ms();
                    if self.poll_platform_interrupt(MAX_QUEUED_EXTERNAL_INTERRUPTS) {
                        continue;
                    }
                    self.flush_serial();
                    return RunExit::Halted { executed };
                }
                BatchExit::BiosInterrupt(vector) => {
                    self.handle_bios_interrupt(vector);
                }
                BatchExit::Assist(reason) => {
                    self.flush_serial();
                    return RunExit::Assist { reason, executed };
                }
                BatchExit::Exception(exception) => {
                    self.flush_serial();
                    return RunExit::Exception {
                        exception,
                        executed,
                    };
                }
                BatchExit::CpuExit(exit) => {
                    self.flush_serial();
                    return RunExit::CpuExit { exit, executed };
                }
            }
        }

        self.flush_serial();
        RunExit::Completed { executed }
    }

    fn handle_bios_interrupt(&mut self, vector: u8) {
        // Keep the core's A20 view coherent with the chipset latch while executing BIOS services.
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios
            .dispatch_interrupt(vector, &mut self.cpu.state, bus, &mut self.disk);
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
    }

    fn poll_platform_interrupt(&mut self, max_queued: usize) -> bool {
        if self.cpu.pending.external_interrupts.len() >= max_queued {
            return false;
        }

        // Only acknowledge/present a maskable interrupt to the CPU when it can be delivered.
        //
        // The platform interrupt controller (PIC/IOAPIC+LAPIC) latches interrupts until the CPU
        // performs an acknowledge handshake. If we acknowledge while the CPU is unable to accept
        // delivery (IF=0, interrupt shadow, pending exception), we could incorrectly clear the
        // controller and lose the interrupt.
        if self.cpu.pending.has_pending_event()
            || (self.cpu.state.rflags() & RFLAGS_IF) == 0
            || self.cpu.pending.interrupt_inhibit() != 0
        {
            return false;
        }

        let Some(interrupts) = &self.interrupts else {
            return false;
        };

        let mut interrupts = interrupts.borrow_mut();
        let vector = PlatformInterruptController::get_pending(&*interrupts);
        let Some(vector) = vector else {
            return false;
        };

        PlatformInterruptController::acknowledge(&mut *interrupts, vector);
        self.cpu.pending.inject_external_interrupt(vector);
        true
    }

    fn flush_serial(&mut self) {
        let Some(uart) = &self.serial else {
            return;
        };
        let mut uart = uart.borrow_mut();
        let tx = uart.take_tx();
        if !tx.is_empty() {
            self.serial_log.extend_from_slice(&tx);
        }
    }
}

impl snapshot::SnapshotSource for Machine {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        let snapshot_id = self.next_snapshot_id;
        self.next_snapshot_id = self.next_snapshot_id.saturating_add(1);

        #[cfg(target_arch = "wasm32")]
        let created_unix_ms = 0u64;
        #[cfg(not(target_arch = "wasm32"))]
        let created_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);

        let meta = snapshot::SnapshotMeta {
            snapshot_id,
            parent_snapshot_id: self.last_snapshot_id,
            created_unix_ms,
            label: None,
        };
        self.last_snapshot_id = Some(snapshot_id);
        meta
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::cpu_state_from_cpu_core(&self.cpu)
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::mmu_state_from_cpu_core(&self.cpu)
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        const V1: u16 = 1;
        let mut devices = Vec::new();

        // Firmware snapshot: required for deterministic BIOS interrupt behavior.
        let bios_snapshot = self.bios.snapshot();
        let mut bios_bytes = Vec::new();
        if bios_snapshot.encode(&mut bios_bytes).is_ok() {
            devices.push(snapshot::DeviceState {
                id: snapshot::DeviceId::BIOS,
                version: V1,
                flags: 0,
                data: bios_bytes,
            });
        }

        // Memory/chipset glue.
        devices.push(snapshot::DeviceState {
            id: snapshot::DeviceId::MEMORY,
            version: V1,
            flags: 0,
            data: vec![self.chipset.a20().enabled() as u8],
        });

        // Accumulated serial output (drained from the UART by `Machine::run_slice`).
        devices.push(snapshot::DeviceState {
            id: snapshot::DeviceId::SERIAL,
            version: V1,
            flags: 0,
            data: self.serial_log.clone(),
        });

        // Optional PC platform devices.
        //
        // Note: We snapshot the combined PIC + IOAPIC + LAPIC router state via `PlatformInterrupts`.
        // Prefer the dedicated `DeviceId::PLATFORM_INTERRUPTS` id; keep accepting the historical
        // `DeviceId::APIC` id for backward compatibility when restoring older snapshots.
        if let Some(interrupts) = &self.interrupts {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PLATFORM_INTERRUPTS,
                &*interrupts.borrow(),
            ));
        }
        if let Some(pit) = &self.pit {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PIT,
                &*pit.borrow(),
            ));
        }
        if let Some(rtc) = &self.rtc {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::RTC,
                &*rtc.borrow(),
            ));
        }
        // PCI core state (config ports + INTx router).
        //
        // Canonical full-machine snapshots store these as separate outer device entries to avoid
        // `DEVICES` duplicate `(id, version, flags)` collisions:
        // - `DeviceId::PCI_CFG` for `PciConfigPorts` (`PCPT`)
        // - `DeviceId::PCI_INTX` for `PciIntxRouter` (`INTX`)
        if let Some(pci_cfg) = &self.pci_cfg {
            // Canonical outer ID for legacy PCI config mechanism #1 ports (`0xCF8/0xCFC`) and
            // PCI bus config-space state.
            //
            // NOTE: `PciConfigPorts` snapshots cover both the config mechanism #1 address latch
            // and the per-device config space/BAR state, so this one entry is sufficient to
            // restore guest-programmed BARs and command bits.
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PCI_CFG,
                &*pci_cfg.borrow(),
            ));
        }
        if let Some(pci_intx) = &self.pci_intx {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PCI_INTX,
                &*pci_intx.borrow(),
            ));
        }
        if let Some(e1000) = &self.e1000 {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::E1000,
                &*e1000.borrow(),
            ));
        }
        if let Some(acpi_pm) = &self.acpi_pm {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::ACPI_PM,
                &*acpi_pm.borrow(),
            ));
        }
        if let Some(hpet) = &self.hpet {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::HPET,
                &*hpet.borrow(),
            ));
        }

        if let Some(ctrl) = &self.i8042 {
            let ctrl = ctrl.borrow();
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::I8042,
                &*ctrl,
            ));
        }

        // CPU_INTERNAL: non-architectural Tier-0 bookkeeping required for deterministic resume.
        let cpu_internal = snapshot::cpu_internal_state_from_cpu_core(&self.cpu);
        devices.push(
            cpu_internal
                .to_device_state()
                .expect("CpuInternalState::to_device_state should be infallible"),
        );
        devices
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        usize::try_from(self.cfg.ram_size_bytes).unwrap_or(0)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if end > self.cfg.ram_size_bytes {
            return Err(snapshot::SnapshotError::Corrupt("ram read out of range"));
        }
        self.mem
            .inner
            .borrow()
            .ram
            .read_into(offset, buf)
            .map_err(|_err: GuestMemoryError| {
                snapshot::SnapshotError::Corrupt("ram read failed")
            })?;
        Ok(())
    }

    fn dirty_page_size(&self) -> u32 {
        SNAPSHOT_DIRTY_PAGE_SIZE
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(self.mem.take_dirty_pages())
    }
}

impl snapshot::SnapshotTarget for Machine {
    fn restore_meta(&mut self, meta: snapshot::SnapshotMeta) {
        self.last_snapshot_id = Some(meta.snapshot_id);
        self.next_snapshot_id = self
            .next_snapshot_id
            .max(meta.snapshot_id.saturating_add(1));
    }

    fn restore_cpu_state(&mut self, state: snapshot::CpuState) {
        snapshot::apply_cpu_state_to_cpu_core(&state, &mut self.cpu);
    }

    fn restore_mmu_state(&mut self, state: snapshot::MmuState) {
        snapshot::apply_mmu_state_to_cpu_core(&state, &mut self.cpu);
        self.cpu.time.set_tsc(self.cpu.state.msr.tsc);
    }

    fn restore_device_states(&mut self, states: Vec<snapshot::DeviceState>) {
        use std::collections::HashMap;

        // Reset pending CPU bookkeeping to a deterministic baseline, so restores from older
        // snapshots (that lack `CPU_INTERNAL`) still clear stale pending state.
        self.cpu.pending = Default::default();

        // Restore ordering must be explicit and independent of snapshot file ordering so device
        // state is deterministic (especially for interrupt lines and PCI INTx routing).
        let mut by_id: HashMap<snapshot::DeviceId, snapshot::DeviceState> =
            HashMap::with_capacity(states.len());
        for state in states {
            // Snapshot format already rejects duplicate (id, version, flags) tuples; for multiple
            // entries with the same outer ID (forward-compatible versions), prefer the first one.
            by_id.entry(state.id).or_insert(state);
        }

        // Firmware snapshot: required for deterministic BIOS interrupt behaviour.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::BIOS) {
            if state.version == 1 {
                if let Ok(snapshot) =
                    firmware::bios::BiosSnapshot::decode(&mut Cursor::new(&state.data))
                {
                    self.bios.restore_snapshot(snapshot, &mut self.mem);
                }
            }
        }

        // Memory/chipset glue.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::MEMORY) {
            if state.version == 1 {
                let enabled = state.data.first().copied().unwrap_or(0) != 0;
                self.chipset.a20().set_enabled(enabled);
                self.cpu.state.a20_enabled = enabled;
            }
        }

        // Accumulated serial output.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::SERIAL) {
            if state.version == 1 {
                if let Some(uart) = &self.serial {
                    let _ = uart.borrow_mut().take_tx();
                }
                self.serial_log = state.data;
            }
        }

        // Optional PC platform devices.

        // 1) Restore interrupt controller complex first.
        //
        // Prefer the dedicated `PLATFORM_INTERRUPTS` id, but accept the historical `APIC` id for
        // backward compatibility with older snapshots.
        let mut restored_interrupts = false;
        let interrupts_state = by_id
            .remove(&snapshot::DeviceId::PLATFORM_INTERRUPTS)
            .or_else(|| by_id.remove(&snapshot::DeviceId::APIC));
        if let (Some(interrupts), Some(state)) = (&self.interrupts, interrupts_state) {
            let mut interrupts = interrupts.borrow_mut();
            restored_interrupts =
                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *interrupts)
                    .is_ok();
        }

        let mut restored_pci_intx = false;
        // 2) Restore PCI devices (config ports + INTx router).
        //
        // Canonical full-machine snapshots store these as separate outer device entries:
        // - `DeviceId::PCI_CFG` for `PciConfigPorts` (`PCPT`)
        // - `DeviceId::PCI_INTX` for `PciIntxRouter` (`INTX`)
        //
        // Backward compatibility: older snapshots stored one or both of these under the historical
        // `DeviceId::PCI` entry, either:
        // - as a combined `PciCoreSnapshot` wrapper (`PCIC`) containing both `PCPT` + `INTX`, or
        // - as a single `PCPT` (`PciConfigPorts`) payload, or
        // - as a single `INTX` (`PciIntxRouter`) payload.
        let pci_state = by_id.remove(&snapshot::DeviceId::PCI);
        let mut pci_cfg_state = by_id.remove(&snapshot::DeviceId::PCI_CFG);
        let mut pci_intx_state = by_id.remove(&snapshot::DeviceId::PCI_INTX);

        if let Some(state) = pci_state {
            if let (Some(pci_cfg), Some(pci_intx)) = (&self.pci_cfg, &self.pci_intx) {
                // Prefer decoding the combined PCI core wrapper (`PCIC`) first. If decoding fails,
                // treat `DeviceId::PCI` as the legacy `PCPT`/`INTX` payload.
                let core_result = {
                    let mut pci_cfg = pci_cfg.borrow_mut();
                    let mut pci_intx = pci_intx.borrow_mut();
                    let mut core = PciCoreSnapshot::new(&mut pci_cfg, &mut pci_intx);
                    snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut core)
                };

                match core_result {
                    Ok(()) => {
                        restored_pci_intx = true;
                        // If a dedicated `PCI_CFG` entry is also present, prefer it for config ports
                        // even if the combined core wrapper applied successfully.
                        if let Some(cfg_state) = pci_cfg_state.take() {
                            let mut cfg_ports = pci_cfg.borrow_mut();
                            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &cfg_state,
                                &mut *cfg_ports,
                            );
                        }
                    }
                    Err(_) => {
                        // If a dedicated `PCI_CFG` entry is present, prefer it for config ports.
                        if let Some(cfg_state) = pci_cfg_state.take() {
                            let mut cfg_ports = pci_cfg.borrow_mut();
                            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &cfg_state,
                                &mut *cfg_ports,
                            );
                        } else {
                            let mut cfg_ports = pci_cfg.borrow_mut();
                            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                                &state,
                                &mut *cfg_ports,
                            );
                        }

                        // Backward compatibility: some snapshots stored `PciIntxRouter` (`INTX`)
                        // directly under the historical `DeviceId::PCI`.
                        let mut pci_intx = pci_intx.borrow_mut();
                        if snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                            &state,
                            &mut *pci_intx,
                        )
                        .is_ok()
                        {
                            restored_pci_intx = true;
                        }
                    }
                }
            } else if let Some(pci_cfg) = &self.pci_cfg {
                // Config ports only. Prefer the dedicated `PCI_CFG` entry if present.
                let mut cfg_ports = pci_cfg.borrow_mut();
                if let Some(cfg_state) = pci_cfg_state.take() {
                    let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                        &cfg_state,
                        &mut *cfg_ports,
                    );
                } else {
                    let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                        &state,
                        &mut *cfg_ports,
                    );
                }
            }
        } else {
            // No legacy PCI entry; restore config ports from the canonical `PCI_CFG` entry.
            if let (Some(pci_cfg), Some(cfg_state)) = (&self.pci_cfg, pci_cfg_state.take()) {
                let mut cfg_ports = pci_cfg.borrow_mut();
                let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                    &cfg_state,
                    &mut *cfg_ports,
                );
            }
        }

        // If we haven't restored the INTx router yet, fall back to a canonical/legacy `PCI_INTX`
        // entry.
        if !restored_pci_intx {
            if let (Some(pci_intx), Some(intx_state)) = (&self.pci_intx, pci_intx_state.take()) {
                let mut pci_intx = pci_intx.borrow_mut();
                restored_pci_intx = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                    &intx_state,
                    &mut *pci_intx,
                )
                .is_ok();
            }
        }

        // 3) After restoring both the interrupt controller and the PCI INTx router, re-drive any
        // asserted level-triggered GSIs into the interrupt sink.
        if restored_interrupts && restored_pci_intx {
            if let (Some(pci_intx), Some(interrupts)) = (&self.pci_intx, &self.interrupts) {
                let pci_intx = pci_intx.borrow();
                let mut interrupts = interrupts.borrow_mut();
                pci_intx.sync_levels_to_sink(&mut *interrupts);
            }
        }

        // 4) Restore PIT + RTC + ACPI PM (these can drive IRQ lines during load_state()).
        if let (Some(pit), Some(state)) = (&self.pit, by_id.remove(&snapshot::DeviceId::PIT)) {
            let mut pit = pit.borrow_mut();
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *pit);
        }
        if let (Some(rtc), Some(state)) = (&self.rtc, by_id.remove(&snapshot::DeviceId::RTC)) {
            let mut rtc = rtc.borrow_mut();
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *rtc);
        }
        if let (Some(acpi_pm), Some(state)) =
            (&self.acpi_pm, by_id.remove(&snapshot::DeviceId::ACPI_PM))
        {
            let mut acpi_pm = acpi_pm.borrow_mut();
            let _ =
                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *acpi_pm);
        }

        // 5) Restore HPET.
        let mut restored_hpet = false;
        if let (Some(hpet), Some(state)) = (&self.hpet, by_id.remove(&snapshot::DeviceId::HPET)) {
            let mut hpet = hpet.borrow_mut();
            restored_hpet =
                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *hpet)
                    .is_ok();
        }

        // 6) After HPET restore, re-drive any level-triggered lines implied by restored interrupt
        // status immediately.
        if restored_hpet {
            if let (Some(hpet), Some(interrupts)) = (&self.hpet, &self.interrupts) {
                let mut hpet = hpet.borrow_mut();
                let mut interrupts = interrupts.borrow_mut();
                hpet.sync_levels_to_sink(&mut *interrupts);
            }
        }

        // Restore E1000 after the interrupt controller + PCI INTx router so any restored
        // interrupt level can be re-driven into the sink immediately.
        if let (Some(e1000), Some(state)) = (&self.e1000, by_id.remove(&snapshot::DeviceId::E1000))
        {
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                &state,
                &mut *e1000.borrow_mut(),
            );
        }

        // Restore i8042 after the interrupt controller complex so any restored IRQ pulses are
        // delivered into the correct sink state.
        if let (Some(ctrl), Some(state)) = (&self.i8042, by_id.remove(&snapshot::DeviceId::I8042)) {
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                &state,
                &mut *ctrl.borrow_mut(),
            );
        }

        // Re-drive PCI INTx levels derived from restored device state (e.g. E1000). This is
        // required because `IoSnapshot::load_state()` cannot access the interrupt sink directly,
        // and some device models surface their INTx level via polling rather than storing it in
        // the router snapshot.
        self.sync_pci_intx_sources_to_interrupts();

        // CPU_INTERNAL: machine-defined CPU bookkeeping (interrupt shadow + external interrupt FIFO).
        if let Some(state) = by_id.remove(&snapshot::DeviceId::CPU_INTERNAL) {
            if state.version == snapshot::CpuInternalState::VERSION {
                if let Ok(decoded) = snapshot::CpuInternalState::from_device_state(&state) {
                    snapshot::apply_cpu_internal_state_to_cpu_core(&decoded, &mut self.cpu);
                }
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: snapshot::DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        usize::try_from(self.cfg.ram_size_bytes).unwrap_or(0)
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> snapshot::Result<()> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if end > self.cfg.ram_size_bytes {
            return Err(snapshot::SnapshotError::Corrupt("ram write out of range"));
        }
        self.mem
            .inner
            .borrow_mut()
            .ram
            .write_from(offset, data)
            .map_err(|_err: GuestMemoryError| {
                snapshot::SnapshotError::Corrupt("ram write failed")
            })?;
        Ok(())
    }

    fn post_restore(&mut self) -> snapshot::Result<()> {
        // Network backends are external host state (e.g. live proxy connections) and are not part
        // of the snapshot format. Ensure we always drop any previously attached backend after
        // restoring, even if the caller bypasses the `Machine::restore_snapshot_*` helper methods
        // and drives snapshot restore directly via `aero_snapshot::restore_snapshot`.
        self.detach_network();
        // `inject_ps2_mouse_buttons` maintains a host-side "previous buttons" cache to synthesize
        // per-button transitions from an absolute mask. Snapshot restore rewinds guest time, so
        // force the next injection call to re-sync all 3 buttons (including transitions to the
        // released state) regardless of the cached value.
        self.ps2_mouse_buttons = 0xFF;
        self.reset_latch.clear();
        self.assist = AssistContext::default();
        // Snapshots restore RAM and paging control registers, but do not capture the MMU's internal
        // translation cache (TLB). Since `Machine` keeps a persistent MMU to warm the TLB across
        // batches, reset it here so restored execution never uses stale translations.
        self.mmu = aero_mmu::Mmu::new();
        self.cpu.state.sync_mmu(&mut self.mmu);
        self.mem.clear_dirty();
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        self.resync_guest_time_from_tsc();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_cpu_core::state::{gpr, CR0_PE, CR0_PG};
    use aero_devices::pci::PciInterruptPin;
    use pretty_assertions::assert_eq;
    use std::io::{Cursor, Read};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn build_serial_boot_sector(message: &[u8]) -> [u8; 512] {
        let mut sector = [0u8; 512];
        let mut i = 0usize;

        // mov dx, 0x3f8
        sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;

        for &b in message {
            // mov al, imm8
            sector[i..i + 2].copy_from_slice(&[0xB0, b]);
            i += 2;
            // out dx, al
            sector[i] = 0xEE;
            i += 1;
        }

        // hlt
        sector[i] = 0xF4;

        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    fn build_paged_serial_boot_sector(message: &[u8]) -> [u8; 512] {
        assert!(!message.is_empty());
        assert!(message.len() <= 32, "test boot sector message too long");

        // Identity-map the code page (0x7000) so execution continues after enabling paging.
        //
        // Map a separate linear page (0x4000) to a different physical page (0x2000) containing
        // the output message. If paging is not active, the guest will read from physical 0x4000
        // instead and the serial output will not match.
        const PD_BASE: u16 = 0x1000;
        const PT_BASE: u16 = 0x3000;
        const MSG_PHYS_BASE: u16 = 0x2000;
        const MSG_LINEAR_BASE: u16 = 0x4000;

        let mut sector = [0u8; 512];
        let mut i = 0usize;

        // xor ax, ax
        sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
        i += 2;
        // mov ds, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
        i += 2;

        // Write the message bytes into a physical RAM page (MSG_PHYS_BASE).
        for (off, &b) in message.iter().enumerate() {
            let addr = MSG_PHYS_BASE.wrapping_add(off as u16);
            // mov byte ptr [addr], imm8
            sector[i..i + 5].copy_from_slice(&[0xC6, 0x06, addr as u8, (addr >> 8) as u8, b]);
            i += 5;
        }

        // PDE[0] -> page table at PT_BASE (present + RW).
        let pde0: u32 = (PT_BASE as u32) | 0x3;
        // 66 c7 06 <disp16> <imm32>
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (PD_BASE & 0xFF) as u8,
            (PD_BASE >> 8) as u8,
            (pde0 & 0xFF) as u8,
            ((pde0 >> 8) & 0xFF) as u8,
            ((pde0 >> 16) & 0xFF) as u8,
            ((pde0 >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // PTE[MSG_LINEAR_BASE >> 12] -> MSG_PHYS_BASE (present + RW).
        let pte_msg_off = PT_BASE.wrapping_add(((MSG_LINEAR_BASE as u32 >> 12) * 4) as u16);
        let pte_msg: u32 = (MSG_PHYS_BASE as u32) | 0x3;
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (pte_msg_off & 0xFF) as u8,
            (pte_msg_off >> 8) as u8,
            (pte_msg & 0xFF) as u8,
            ((pte_msg >> 8) & 0xFF) as u8,
            ((pte_msg >> 16) & 0xFF) as u8,
            ((pte_msg >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // PTE[0x7000 >> 12] -> 0x7000 (code page identity map; present + RW).
        let pte_code_off = PT_BASE.wrapping_add(((0x7000u32 >> 12) * 4) as u16);
        let pte_code: u32 = 0x7000 | 0x3;
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (pte_code_off & 0xFF) as u8,
            (pte_code_off >> 8) as u8,
            (pte_code & 0xFF) as u8,
            ((pte_code >> 8) & 0xFF) as u8,
            ((pte_code >> 16) & 0xFF) as u8,
            ((pte_code >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // mov eax, PD_BASE (32-bit immediate)
        sector[i..i + 6].copy_from_slice(&[
            0x66,
            0xB8,
            (PD_BASE & 0xFF) as u8,
            (PD_BASE >> 8) as u8,
            0x00,
            0x00,
        ]);
        i += 6;
        // mov cr3, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xD8]);
        i += 3;

        // mov eax, cr0
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x20, 0xC0]);
        i += 3;
        // or eax, 0x8000_0000
        sector[i..i + 6].copy_from_slice(&[0x66, 0x0D, 0x00, 0x00, 0x00, 0x80]);
        i += 6;
        // mov cr0, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xC0]);
        i += 3;

        // mov dx, 0x3f8
        sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;

        for (off, _) in message.iter().enumerate() {
            let addr = MSG_LINEAR_BASE.wrapping_add(off as u16);
            // mov al, moffs8
            sector[i..i + 3].copy_from_slice(&[0xA0, addr as u8, (addr >> 8) as u8]);
            i += 3;
            // out dx, al
            sector[i] = 0xEE;
            i += 1;
        }

        // hlt
        sector[i] = 0xF4;

        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    fn build_long_mode_paged_serial_boot_sector(message: &[u8]) -> [u8; 512] {
        assert!(!message.is_empty());
        assert!(
            message.len() <= 64,
            "test boot sector message too long (must fit in disp8 addressing)"
        );

        // This boot sector:
        // - writes `message` into a *different physical page* (`MSG_PHYS_BASE`)
        // - sets up 4-level (long mode) paging mapping:
        //     - code page @ 0x7000  -> physical 0x7000 (identity)
        //     - msg page  @ 0x4000  -> physical MSG_PHYS_BASE
        // - enables IA-32e long mode (PAE + EFER.LME + CR0.PG + CR0.PE)
        // - jumps to a 64-bit code segment and prints the message via COM1.
        //
        // If paging translation is not active, the guest will read from physical 0x4000 (the page
        // table page) instead of the message bytes, and the serial output will not match.
        const PML4_BASE: u16 = 0x1000;
        const PDPT_BASE: u16 = 0x2000;
        const PD_BASE: u16 = 0x3000;
        const PT_BASE: u16 = 0x4000;
        const MSG_PHYS_BASE: u16 = 0x5000;
        const MSG_LINEAR_BASE: u32 = 0x4000;

        // GDT + GDTR pointer are embedded in the boot sector (loaded at 0x7C00).
        const GDTR_OFF: usize = 0x1E0;
        const GDT_OFF: usize = GDTR_OFF + 6;

        let mut sector = [0u8; 512];
        let mut i = 0usize;

        fn write_dword(sector: &mut [u8; 512], i: &mut usize, addr: u16, value: u32) {
            // 66 c7 06 <disp16> <imm32>
            sector[*i..*i + 9].copy_from_slice(&[
                0x66,
                0xC7,
                0x06,
                (addr & 0xFF) as u8,
                (addr >> 8) as u8,
                (value & 0xFF) as u8,
                ((value >> 8) & 0xFF) as u8,
                ((value >> 16) & 0xFF) as u8,
                ((value >> 24) & 0xFF) as u8,
            ]);
            *i += 9;
        }

        // xor ax, ax
        sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
        i += 2;
        // mov ds, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
        i += 2;

        // Write the message bytes into a physical RAM page (MSG_PHYS_BASE).
        for (off, &b) in message.iter().enumerate() {
            let addr = MSG_PHYS_BASE.wrapping_add(off as u16);
            // mov byte ptr [addr], imm8
            sector[i..i + 5].copy_from_slice(&[0xC6, 0x06, addr as u8, (addr >> 8) as u8, b]);
            i += 5;
        }

        // Build long mode page tables. We only populate the entries needed for:
        // - the boot sector code/data page at 0x7000, and
        // - the message page at 0x4000.
        //
        // Write the low dword and explicitly zero the high dword so we don't rely on RAM being
        // pre-zeroed.
        let pml4e0: u32 = (PDPT_BASE as u32) | 0x7;
        write_dword(&mut sector, &mut i, PML4_BASE, pml4e0);
        write_dword(&mut sector, &mut i, PML4_BASE.wrapping_add(4), 0);

        let pdpte0: u32 = (PD_BASE as u32) | 0x7;
        write_dword(&mut sector, &mut i, PDPT_BASE, pdpte0);
        write_dword(&mut sector, &mut i, PDPT_BASE.wrapping_add(4), 0);

        let pde0: u32 = (PT_BASE as u32) | 0x7;
        write_dword(&mut sector, &mut i, PD_BASE, pde0);
        write_dword(&mut sector, &mut i, PD_BASE.wrapping_add(4), 0);

        let pte_msg_off = PT_BASE.wrapping_add(((MSG_LINEAR_BASE >> 12) * 8) as u16);
        let pte_msg: u32 = (MSG_PHYS_BASE as u32) | 0x7;
        write_dword(&mut sector, &mut i, pte_msg_off, pte_msg);
        write_dword(&mut sector, &mut i, pte_msg_off.wrapping_add(4), 0);

        let pte_code_off = PT_BASE.wrapping_add(((0x7000u32 >> 12) * 8) as u16);
        let pte_code: u32 = 0x7000 | 0x7;
        write_dword(&mut sector, &mut i, pte_code_off, pte_code);
        write_dword(&mut sector, &mut i, pte_code_off.wrapping_add(4), 0);

        // lgdt [0x7C00 + GDTR_OFF]
        let gdtr_addr: u16 = 0x7C00u16.wrapping_add(GDTR_OFF as u16);
        sector[i..i + 5].copy_from_slice(&[
            0x0F,
            0x01,
            0x16,
            gdtr_addr as u8,
            (gdtr_addr >> 8) as u8,
        ]);
        i += 5;

        // Enable CR4.PAE (bit 5) for long mode paging.
        // mov eax, cr4
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x20, 0xE0]);
        i += 3;
        // or eax, 0x20
        sector[i..i + 4].copy_from_slice(&[0x66, 0x83, 0xC8, 0x20]);
        i += 4;
        // mov cr4, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xE0]);
        i += 3;

        // Set IA32_EFER.LME via WRMSR (MSR 0xC000_0080).
        // mov ecx, 0xC000_0080
        sector[i..i + 6].copy_from_slice(&[0x66, 0xB9, 0x80, 0x00, 0x00, 0xC0]);
        i += 6;
        // mov eax, 0x0000_0100 (LME)
        sector[i..i + 6].copy_from_slice(&[0x66, 0xB8, 0x00, 0x01, 0x00, 0x00]);
        i += 6;
        // mov edx, 0
        sector[i..i + 6].copy_from_slice(&[0x66, 0xBA, 0x00, 0x00, 0x00, 0x00]);
        i += 6;
        // wrmsr
        sector[i..i + 2].copy_from_slice(&[0x0F, 0x30]);
        i += 2;

        // mov eax, PML4_BASE
        sector[i..i + 6].copy_from_slice(&[
            0x66,
            0xB8,
            (PML4_BASE & 0xFF) as u8,
            (PML4_BASE >> 8) as u8,
            0x00,
            0x00,
        ]);
        i += 6;
        // mov cr3, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xD8]);
        i += 3;

        // Enable protected mode + paging (CR0.PE | CR0.PG).
        // mov eax, cr0
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x20, 0xC0]);
        i += 3;
        // or eax, 0x8000_0001
        sector[i..i + 6].copy_from_slice(&[0x66, 0x0D, 0x01, 0x00, 0x00, 0x80]);
        i += 6;
        // mov cr0, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xC0]);
        i += 3;

        // Far jump to 64-bit code segment (selector 0x08). This is a 16-bit far jump (offset16 +
        // selector16) because we're still executing 16-bit code at this point. Keep the target
        // within the 64KiB window.
        let long_mode_entry = 0x7C00u16.wrapping_add((i + 5) as u16);
        sector[i..i + 5].copy_from_slice(&[
            0xEA,
            (long_mode_entry & 0xFF) as u8,
            (long_mode_entry >> 8) as u8,
            0x08,
            0x00,
        ]);
        i += 5;

        // ---- 64-bit code (long mode) --------------------------------------------------------

        // mov ax, 0x10
        sector[i..i + 4].copy_from_slice(&[0x66, 0xB8, 0x10, 0x00]);
        i += 4;
        // mov ds, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
        i += 2;
        // mov es, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
        i += 2;
        // mov ss, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD0]);
        i += 2;

        // mov edx, 0x3f8
        sector[i..i + 5].copy_from_slice(&[0xBA, 0xF8, 0x03, 0x00, 0x00]);
        i += 5;
        // mov esi, MSG_LINEAR_BASE
        sector[i..i + 5].copy_from_slice(&[
            0xBE,
            (MSG_LINEAR_BASE & 0xFF) as u8,
            ((MSG_LINEAR_BASE >> 8) & 0xFF) as u8,
            ((MSG_LINEAR_BASE >> 16) & 0xFF) as u8,
            ((MSG_LINEAR_BASE >> 24) & 0xFF) as u8,
        ]);
        i += 5;

        for (off, _) in message.iter().enumerate() {
            let disp = u8::try_from(off).unwrap_or(0);
            // mov al, byte ptr [rsi + disp8]
            sector[i..i + 3].copy_from_slice(&[0x8A, 0x46, disp]);
            i += 3;
            // out dx, al
            sector[i] = 0xEE;
            i += 1;
        }

        // hlt
        sector[i] = 0xF4;

        // ---- GDTR + GDT ---------------------------------------------------------------------

        // GDTR (limit=u16, base=u32) at 0x7C00 + GDTR_OFF.
        let gdt_base = 0x7C00u32 + (GDT_OFF as u32);
        let gdt_limit: u16 = (3 * 8 - 1) as u16;
        sector[GDTR_OFF..GDTR_OFF + 6].copy_from_slice(&[
            (gdt_limit & 0xFF) as u8,
            (gdt_limit >> 8) as u8,
            (gdt_base & 0xFF) as u8,
            ((gdt_base >> 8) & 0xFF) as u8,
            ((gdt_base >> 16) & 0xFF) as u8,
            ((gdt_base >> 24) & 0xFF) as u8,
        ]);

        // Null descriptor.
        sector[GDT_OFF..GDT_OFF + 8].fill(0);
        // 64-bit code descriptor (base=0, limit=4GB, L=1, D=0).
        sector[GDT_OFF + 8..GDT_OFF + 16]
            .copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0xAF, 0x00]);
        // Data descriptor (base=0, limit=4GB).
        sector[GDT_OFF + 16..GDT_OFF + 24]
            .copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0x8F, 0x00]);

        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    #[test]
    fn boots_mbr_and_writes_to_serial() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let boot = build_serial_boot_sector(b"OK\n");
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();

        for _ in 0..100 {
            match m.run_slice(10_000) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let out = m.take_serial_output();
        assert_eq!(out, b"OK\n");
    }

    #[test]
    fn snapshot_restore_drops_network_backend_even_when_restoring_via_snapshot_crate() {
        struct DropBackend {
            dropped: Arc<AtomicUsize>,
        }

        impl aero_net_backend::NetworkBackend for DropBackend {
            fn transmit(&mut self, _frame: Vec<u8>) {}
        }

        impl Drop for DropBackend {
            fn drop(&mut self) {
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }

        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };
        let mut m = Machine::new(cfg).unwrap();
        let snap = m.take_snapshot_full().unwrap();

        let dropped = Arc::new(AtomicUsize::new(0));
        m.set_network_backend(Box::new(DropBackend {
            dropped: dropped.clone(),
        }));

        // Restore via the snapshot crate directly (bypasses `Machine::restore_snapshot_*` helpers).
        snapshot::restore_snapshot(&mut Cursor::new(&snap), &mut m).unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn paging_translation_and_io_work_together() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let boot = build_paged_serial_boot_sector(b"OK\n");
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();

        // Run at least two slices with paging enabled so the machine-level bus/MMU can be reused
        // across `run_slice` calls.
        match m.run_slice(15) {
            RunExit::Completed { .. } => {}
            other => panic!("unexpected exit: {other:?}"),
        }
        assert_ne!(
            m.cpu().control.cr0 & aero_cpu_core::state::CR0_PG,
            0,
            "expected paging to be enabled after first slice"
        );

        for _ in 0..200 {
            match m.run_slice(15) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let out = m.take_serial_output();
        assert_eq!(out, b"OK\n");
    }

    #[test]
    fn long_mode_paging_translation_and_io_work_together() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let boot = build_long_mode_paged_serial_boot_sector(b"LM\n");
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();

        for _ in 0..200 {
            match m.run_slice(50_000) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let out = m.take_serial_output();
        assert_eq!(out, b"LM\n");
    }

    #[test]
    fn snapshot_restore_syncs_time_source_with_ia32_tsc() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.time.set_tsc(0x1234);
        src.cpu.state.msr.tsc = 0x1234;
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert_eq!(restored.cpu.state.msr.tsc, 0x1234);
        assert_eq!(restored.cpu.time.read_tsc(), 0x1234);
    }

    #[test]
    fn snapshot_restore_flushes_persistent_mmu_tlb() {
        // Regression test: snapshots restore RAM + paging control registers, but the machine keeps
        // a persistent `aero_mmu::Mmu` with an internal TLB cache. If we restore a snapshot without
        // flushing the MMU, stale translations from "after the snapshot" can be used even when the
        // paging register values (CR0/CR3/CR4/EFER) match, breaking determinism.
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        };
        let mut m = Machine::new(cfg).unwrap();

        // Build a simple 32-bit paging setup:
        //  - PD[0] -> PT
        //  - PT[0] -> code page (linear 0x0000_0000)
        //  - PT[1] -> data page (linear 0x0000_1000), patched later
        let pd_base = 0x1000u64;
        let pt_base = 0x2000u64;
        let code_page = 0x3000u64;
        let page_a = 0x4000u64;
        let page_b = 0x5000u64;

        const PTE_P: u32 = 1 << 0;
        const PTE_RW: u32 = 1 << 1;
        let flags = PTE_P | PTE_RW;

        // Code:
        //   mov eax, dword ptr [0x0000_1000]   ; populate TLB
        //   invlpg [0x0000_1000]               ; flush and re-walk after PTE patch
        //   mov eax, dword ptr [0x0000_1000]   ; populate TLB with new mapping
        //   hlt
        let code: [u8; 18] = [
            0xA1, 0x00, 0x10, 0x00, 0x00, // mov eax, [0x1000]
            0x0F, 0x01, 0x3D, 0x00, 0x10, 0x00, 0x00, // invlpg [0x1000]
            0xA1, 0x00, 0x10, 0x00, 0x00, // mov eax, [0x1000]
            0xF4, // hlt
        ];

        {
            let mut phys = m.mem.inner.borrow_mut();
            phys.write_physical_u32(pd_base, (pt_base as u32) | flags);
            phys.write_physical_u32(pt_base, (code_page as u32) | flags);
            phys.write_physical_u32(pt_base + 4, (page_a as u32) | flags);

            phys.write_physical_u32(page_a, 0x1111_1111);
            phys.write_physical_u32(page_b, 0x2222_2222);

            phys.write_physical(code_page, &code);
        }

        // Jump directly into 32-bit paging mode without relying on BIOS/boot code.
        m.cpu = CpuCore::new(CpuMode::Protected);
        m.cpu.state.control.cr3 = pd_base;
        m.cpu.state.control.cr0 = CR0_PE | CR0_PG;
        m.cpu.state.control.cr4 = 0;
        m.cpu.state.update_mode();
        m.cpu.state.set_rip(0);

        // Execute the first load to populate the TLB with the page-A mapping.
        assert_eq!(m.run_slice(1), RunExit::Completed { executed: 1 });
        assert_eq!(m.cpu.state.read_gpr32(gpr::RAX), 0x1111_1111);

        // Force RIP back to 0 so the post-restore load happens *without* INVLPG.
        m.cpu.state.set_rip(0);
        let snap = m.take_snapshot_full().unwrap();

        // Patch the PTE so linear 0x1000 now maps to page B.
        m.mem
            .inner
            .borrow_mut()
            .write_physical_u32(pt_base + 4, (page_b as u32) | flags);

        // Run the rest of the code, which executes INVLPG + a second load to populate the TLB with
        // the page-B mapping.
        assert!(matches!(m.run_slice(10), RunExit::Halted { .. }));
        assert_eq!(m.cpu.state.read_gpr32(gpr::RAX), 0x2222_2222);

        // Restoring the snapshot should clear the MMU cache so the next load observes page A.
        m.restore_snapshot_bytes(&snap).unwrap();
        m.cpu.state.write_gpr32(gpr::RAX, 0);
        assert_eq!(m.run_slice(1), RunExit::Completed { executed: 1 });
        assert_eq!(m.cpu.state.read_gpr32(gpr::RAX), 0x1111_1111);
    }

    #[test]
    fn snapshot_restore_roundtrips_cpu_internal_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.inhibit_interrupts_for_one_instruction();
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.cpu.pending.set_interrupt_inhibit(0);
        restored.cpu.pending.inject_external_interrupt(0x33);
        restored.cpu.pending.raise_software_interrupt(0x80, 0);
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert!(!restored.cpu.pending.has_pending_event());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 1);
        assert_eq!(
            restored
                .cpu
                .pending
                .external_interrupts
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0x20, 0x21]
        );
    }

    fn strip_cpu_internal_device_state(bytes: &[u8]) -> Vec<u8> {
        const FILE_HEADER_LEN: usize = 16;
        const SECTION_HEADER_LEN: usize = 16;

        let mut r = Cursor::new(bytes);
        let mut file_header = [0u8; FILE_HEADER_LEN];
        r.read_exact(&mut file_header).unwrap();

        let mut out = Vec::with_capacity(bytes.len());
        out.extend_from_slice(&file_header);

        let mut removed = 0usize;

        while (r.position() as usize) < bytes.len() {
            let mut section_header = [0u8; SECTION_HEADER_LEN];
            // Valid snapshots end cleanly at EOF.
            if let Err(e) = r.read_exact(&mut section_header) {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    break;
                }
                panic!("failed to read section header: {e}");
            }

            let id = u32::from_le_bytes(section_header[0..4].try_into().unwrap());
            let version = u16::from_le_bytes(section_header[4..6].try_into().unwrap());
            let flags = u16::from_le_bytes(section_header[6..8].try_into().unwrap());
            let len = u64::from_le_bytes(section_header[8..16].try_into().unwrap());

            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload).unwrap();

            if id != snapshot::SectionId::DEVICES.0 {
                out.extend_from_slice(&section_header);
                out.extend_from_slice(&payload);
                continue;
            }

            let mut pr = Cursor::new(&payload);
            let mut count_bytes = [0u8; 4];
            pr.read_exact(&mut count_bytes).unwrap();
            let count = u32::from_le_bytes(count_bytes) as usize;

            let mut kept = Vec::new();
            for _ in 0..count {
                let mut dev_header = [0u8; 16];
                pr.read_exact(&mut dev_header).unwrap();
                let dev_id = u32::from_le_bytes(dev_header[0..4].try_into().unwrap());
                let dev_len = u64::from_le_bytes(dev_header[8..16].try_into().unwrap());
                let mut dev_data = vec![0u8; dev_len as usize];
                pr.read_exact(&mut dev_data).unwrap();

                if dev_id == snapshot::DeviceId::CPU_INTERNAL.0 {
                    removed += 1;
                    continue;
                }

                let mut bytes = Vec::with_capacity(dev_header.len() + dev_data.len());
                bytes.extend_from_slice(&dev_header);
                bytes.extend_from_slice(&dev_data);
                kept.push(bytes);
            }

            assert_eq!(
                pr.position() as usize,
                payload.len(),
                "devices section parse did not consume full payload"
            );

            let mut new_payload = Vec::new();
            let new_count: u32 = kept.len().try_into().unwrap();
            new_payload.extend_from_slice(&new_count.to_le_bytes());
            for dev in kept {
                new_payload.extend_from_slice(&dev);
            }
            let new_len: u64 = new_payload.len().try_into().unwrap();

            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&version.to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&new_len.to_le_bytes());
            out.extend_from_slice(&new_payload);
        }

        assert!(removed > 0, "snapshot did not contain a CPU_INTERNAL entry");
        out
    }

    #[test]
    fn restore_snapshot_without_cpu_internal_clears_pending_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.set_interrupt_inhibit(7);
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        let snap = src.take_snapshot_full().unwrap();
        let snap_without_cpu_internal = strip_cpu_internal_device_state(&snap);

        let mut restored = Machine::new(cfg).unwrap();
        restored.cpu.pending.set_interrupt_inhibit(1);
        restored.cpu.pending.inject_external_interrupt(0x33);
        restored.cpu.pending.raise_software_interrupt(0x80, 0);

        restored
            .restore_snapshot_bytes(&snap_without_cpu_internal)
            .unwrap();

        assert!(!restored.cpu.pending.has_pending_event());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 0);
        assert!(restored.cpu.pending.external_interrupts.is_empty());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 0);
    }

    #[test]
    fn snapshot_restore_preserves_cpu_internal_interrupt_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        src.cpu.pending.inhibit_interrupts_for_one_instruction();

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        // Ensure restore does not merge with pre-existing state.
        restored.cpu.pending.inject_external_interrupt(0x99);
        restored.cpu.pending.set_interrupt_inhibit(0);

        restored.restore_snapshot_bytes(&snap).unwrap();

        let restored_irqs: Vec<u8> = restored
            .cpu
            .pending
            .external_interrupts
            .iter()
            .copied()
            .collect();
        assert_eq!(restored_irqs, vec![0x20, 0x21]);
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 1);
    }

    #[test]
    fn inject_keyboard_and_mouse_produces_i8042_output_bytes() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        m.inject_browser_key("KeyA", true);
        m.inject_browser_key("KeyA", false);

        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);

        // Enable mouse reporting so injected motion generates stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        m.inject_mouse_motion(10, 5, 0);
        let packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(packet, vec![0x28, 10, 0xFB]);
    }

    #[test]
    fn inject_ps2_mouse_motion_inverts_dy() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        // Enable mouse reporting so injected motion generates stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        // `inject_ps2_mouse_motion` expects dy>0 as "up". The underlying PS/2 mouse model expects
        // browser-style dy>0 as "down", so Machine must invert it.
        m.inject_ps2_mouse_motion(0, 5, 0);
        let packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(packet, vec![0x08, 0x00, 0x05]);
    }

    #[test]
    fn snapshot_restore_preserves_i8042_pending_output_bytes() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.inject_browser_key("KeyA", true);
        src.inject_browser_key("KeyA", false);
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x00);
    }

    #[test]
    fn snapshot_restore_preserves_i8042_output_port_and_pending_write() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        let ctrl = src.i8042.as_ref().expect("i8042 enabled").clone();
        {
            let mut dev = ctrl.borrow_mut();
            // Set an initial output-port value.
            dev.write_port(0x64, 0xD1);
            dev.write_port(0x60, 0x03);

            // Leave an in-flight "write output port" pending write.
            dev.write_port(0x64, 0xD1);
        }

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        let mut dev = ctrl.borrow_mut();

        // Verify output port preserved.
        dev.write_port(0x64, 0xD0);
        assert_eq!(dev.read_port(0x60), 0x03);

        // Verify pending write preserved and targets the output port.
        dev.write_port(0x60, 0x01);
        dev.write_port(0x64, 0xD0);
        assert_eq!(dev.read_port(0x60), 0x01);
    }

    #[test]
    fn restoring_i8042_state_resynchronizes_platform_a20() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let src = Machine::new(cfg.clone()).unwrap();
        let ctrl = src.i8042.as_ref().expect("i8042 enabled").clone();

        // Save a snapshot with A20 disabled in the controller output port.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD1);
            dev.write_port(0x60, 0x01);
        }
        assert!(!src.chipset.a20().enabled());

        let state = {
            let dev = ctrl.borrow();
            snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::I8042,
                &*dev,
            )
        };

        // Simulate restoring into an environment where A20 is currently enabled.
        let mut restored = Machine::new(cfg).unwrap();
        restored.chipset.a20().set_enabled(true);
        assert!(restored.chipset.a20().enabled());

        snapshot::SnapshotTarget::restore_device_states(&mut restored, vec![state]);

        assert!(!restored.chipset.a20().enabled());
    }

    #[test]
    fn i8042_injection_apis_are_noops_when_disabled() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_i8042: false,
            ..Default::default()
        })
        .unwrap();

        // Should not panic.
        m.inject_browser_key("KeyA", true);
        m.inject_mouse_motion(1, 2, 3);
        m.inject_mouse_button(Ps2MouseButton::Left, true);

        assert!(m.i8042.is_none());
        let devices = snapshot::SnapshotSource::device_states(&m);
        assert!(
            devices.iter().all(|d| d.id != snapshot::DeviceId::I8042),
            "i8042 device state should not be emitted when disabled"
        );
    }

    #[test]
    fn dirty_snapshot_roundtrip_preserves_i8042_pending_output_bytes() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut vm = Machine::new(cfg.clone()).unwrap();
        vm.inject_browser_key("KeyA", true);
        vm.inject_browser_key("KeyA", false);
        let base = vm.take_snapshot_full().unwrap();

        vm.inject_browser_key("KeyB", true);
        vm.inject_browser_key("KeyB", false);
        let diff = vm.take_snapshot_dirty().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&base).unwrap();
        restored.restore_snapshot_bytes(&diff).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x30);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xB0);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x00);
    }

    #[test]
    fn dirty_tracking_includes_device_writes_to_ram() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 16 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        // `Machine::new` performs a reset which clears dirty pages.
        assert!(m.mem.take_dirty_pages().is_empty());

        // Simulate a DMA/device write by bypassing the CPU memory wrapper and writing directly to
        // the underlying physical bus.
        m.mem
            .inner
            .borrow_mut()
            .write_physical(0x2000, &[0xAA, 0xBB, 0xCC, 0xDD]);

        assert_eq!(m.mem.take_dirty_pages(), vec![2]);

        // Drain semantics.
        assert!(m.mem.take_dirty_pages().is_empty());
    }

    #[test]
    fn dirty_snapshot_includes_device_writes_to_ram() {
        let cfg = MachineConfig {
            ram_size_bytes: 16 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        let base = src.take_snapshot_full().unwrap();

        // Simulate a DMA/device write by bypassing `SystemMemory` and writing directly to the
        // physical bus RAM backend.
        let addr = 0x2000u64;
        let data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        src.mem.inner.borrow_mut().write_physical(addr, &data);

        // Take a dirty snapshot diff and ensure the restored VM observes the change.
        let diff = src.take_snapshot_dirty().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&base).unwrap();
        restored.restore_snapshot_bytes(&diff).unwrap();

        assert_eq!(restored.read_physical_bytes(addr, data.len()), data);
    }

    #[test]
    fn snapshot_restore_clears_ps2_mouse_button_cache() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();
        m.inject_ps2_mouse_buttons(0x01);
        assert_eq!(m.ps2_mouse_buttons, 0x01);

        let snap = m.take_snapshot_full().unwrap();

        // Mutate the cache so we can verify restore resets it.
        m.inject_ps2_mouse_buttons(0x07);
        assert_eq!(m.ps2_mouse_buttons, 0x07);

        m.restore_snapshot_bytes(&snap).unwrap();
        assert_eq!(m.ps2_mouse_buttons, 0xFF);

        // Next injection should re-sync and clear the invalid marker.
        m.inject_ps2_mouse_buttons(0x00);
        assert_eq!(m.ps2_mouse_buttons, 0x00);
    }

    #[test]
    fn snapshot_restore_allows_resyncing_ps2_mouse_buttons_to_pressed_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        // Take a snapshot with mouse reporting enabled so button injections generate packets.
        let mut src = Machine::new(cfg.clone()).unwrap();
        {
            let ctrl = src.i8042.as_ref().expect("i8042 enabled").clone();
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(src.io.read_u8(0x60), 0xFA); // mouse ACK

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        // Post-restore the cache is invalid; the first absolute mask should force a resync.
        assert_eq!(restored.ps2_mouse_buttons, 0xFF);

        restored.inject_ps2_mouse_buttons(0x01); // left pressed

        // The first generated packet should reflect the left button down and no movement.
        let packet: Vec<u8> = (0..3).map(|_| restored.io.read_u8(0x60)).collect();
        assert_eq!(packet, vec![0x09, 0x00, 0x00]);
    }

    fn write_ivt_entry(m: &mut Machine, vector: u8, offset: u16, segment: u16) {
        let addr = u64::from(vector) * 4;
        let bytes = [
            (offset & 0xFF) as u8,
            (offset >> 8) as u8,
            (segment & 0xFF) as u8,
            (segment >> 8) as u8,
        ];
        m.mem.inner.borrow_mut().write_physical(addr, &bytes);
    }

    fn init_real_mode_cpu(m: &mut Machine, entry_ip: u16, rflags: u64) {
        fn set_real_segment(seg: &mut aero_cpu_core::state::Segment, selector: u16) {
            seg.selector = selector;
            seg.base = u64::from(selector) << 4;
            seg.limit = 0xFFFF;
            seg.access = 0;
        }

        m.cpu.pending = Default::default();
        set_real_segment(&mut m.cpu.state.segments.cs, 0);
        set_real_segment(&mut m.cpu.state.segments.ds, 0);
        set_real_segment(&mut m.cpu.state.segments.es, 0);
        set_real_segment(&mut m.cpu.state.segments.ss, 0);
        m.cpu.state.set_stack_ptr(0x8000);
        m.cpu.state.set_rip(u64::from(entry_ip));
        m.cpu.state.set_rflags(rflags);
        m.cpu.state.halted = false;

        // Ensure the real-mode IVT is in use.
        m.cpu.state.tables.idtr.base = 0;
        m.cpu.state.tables.idtr.limit = 0x03FF;
    }

    #[test]
    fn pc_platform_irq_is_delivered_to_cpu_core() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        // Simple handler for IRQ0 (vector 0x20): write a byte to RAM and IRET.
        //
        // mov byte ptr [0x2000], 0xAA
        // iret
        const HANDLER_IP: u16 = 0x1100;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(HANDLER_IP), &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF]);
        write_ivt_entry(&mut m, 0x20, HANDLER_IP, 0x0000);

        // Program CPU at 0x1000 with a small NOP sled.
        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(ENTRY_IP), &[0x90, 0x90, 0x90, 0x90, 0x90]);
        m.mem.inner.borrow_mut().write_physical(0x2000, &[0x00]);

        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);

        // Configure the legacy PIC to use the standard remapped offsets and unmask IRQ0.
        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            for irq in 0..16 {
                ints.pic_mut().set_masked(irq, irq != 0);
            }

            ints.raise_irq(aero_platform::interrupts::InterruptInput::IsaIrq(0));
        }

        // Simulate the CPU being halted: Tier-0 should wake it once the interrupt vector is delivered.
        m.cpu.state.halted = true;

        // Sanity: the interrupt controller sees the pending vector.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );

        // Run a few instructions; the interrupt should be injected and delivered before the first
        // guest instruction executes.
        let exit = m.run_slice(5);
        assert_eq!(exit, RunExit::Completed { executed: 5 });
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
        assert!(
            !m.cpu.state.halted,
            "CPU should wake from HLT once IRQ is delivered"
        );
    }

    #[test]
    fn pc_platform_irq_is_not_acknowledged_during_interrupt_shadow() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        // Simple handler for IRQ0 (vector 0x20): write a byte to RAM and IRET.
        const HANDLER_IP: u16 = 0x1100;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(HANDLER_IP), &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF]);
        write_ivt_entry(&mut m, 0x20, HANDLER_IP, 0x0000);

        // Program CPU at 0x1000 with enough NOPs to cover the instruction budgets below.
        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(ENTRY_IP), &[0x90; 32]);
        m.mem.inner.borrow_mut().write_physical(0x2000, &[0x00]);

        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);
        m.cpu.pending.inhibit_interrupts_for_one_instruction();

        // Configure the legacy PIC to use the standard remapped offsets and unmask IRQ0.
        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            for irq in 0..16 {
                ints.pic_mut().set_masked(irq, irq != 0);
            }
            ints.raise_irq(aero_platform::interrupts::InterruptInput::IsaIrq(0));
        }

        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );

        // While the interrupt shadow is active, the machine should not poll/acknowledge the PIC.
        assert_eq!(m.run_slice(1), RunExit::Completed { executed: 1 });
        assert_eq!(m.cpu.pending.interrupt_inhibit(), 0);
        assert!(m.cpu.pending.external_interrupts.is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );
        assert_eq!(m.read_physical_u8(0x2000), 0x00);

        // Once the shadow expires, the pending IRQ should be acknowledged + delivered.
        let _ = m.run_slice(10);
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
    }

    #[test]
    fn pc_platform_mmio_mappings_route_ioapic_interrupts_in_apic_mode() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            // Keep the machine minimal for deterministic MMIO + interrupt routing assertions.
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();
        // Exercise stable `Rc` identities and idempotent MMIO mappings across resets.
        m.reset();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        interrupts
            .borrow_mut()
            .set_mode(aero_platform::interrupts::PlatformInterruptMode::Apic);

        // Program IOAPIC redirection entry for GSI10 -> vector 0x60 (active-low, level-triggered).
        const GSI: u32 = 10;
        const VECTOR: u32 = 0x60;
        let low: u32 = VECTOR | (1 << 13) | (1 << 15); // polarity low + level triggered
        let redtbl_low = 0x10u32 + GSI * 2;
        let redtbl_high = redtbl_low + 1;

        {
            let mut bus = m.mem.inner.borrow_mut();
            bus.write_physical_u32(IOAPIC_MMIO_BASE, redtbl_low);
            bus.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, low);
            bus.write_physical_u32(IOAPIC_MMIO_BASE, redtbl_high);
            bus.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, 0);
        }

        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        interrupts
            .borrow_mut()
            .raise_irq(aero_platform::interrupts::InterruptInput::Gsi(GSI));

        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(VECTOR as u8)
        );

        // Smoke test LAPIC + HPET MMIO mappings as well.
        let svr = m.mem.inner.borrow_mut().read_physical_u32(LAPIC_MMIO_BASE + 0xF0);
        assert_eq!(svr & 0x1FF, 0x1FF);

        let caps = m
            .mem
            .inner
            .borrow_mut()
            .read_physical_u64(hpet::HPET_MMIO_BASE);
        assert_eq!((caps >> 16) & 0xFFFF, 0x8086);
    }

    #[test]
    fn pc_platform_irq_is_not_acknowledged_when_interrupts_disabled() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            ..Default::default()
        })
        .unwrap();

        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(ENTRY_IP), &[0x90, 0x90, 0x90, 0x90]);
        init_real_mode_cpu(&mut m, ENTRY_IP, 0);

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            for irq in 0..16 {
                ints.pic_mut().set_masked(irq, irq != 0);
            }
            ints.raise_irq(aero_platform::interrupts::InterruptInput::IsaIrq(0));
        }

        // Halted + IF=0: the CPU cannot accept maskable interrupts, so the machine should not
        // acknowledge or enqueue the interrupt vector.
        m.cpu.state.halted = true;
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );
        let exit = m.run_slice(5);
        assert_eq!(exit, RunExit::Halted { executed: 0 });
        assert!(m.cpu.pending.external_interrupts.is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(0x20)
        );
    }

    #[test]
    fn pc_e1000_intx_is_synced_and_delivered_to_cpu_core() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        let pci_intx = m.pci_intx_router().expect("pc platform enabled");

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Install a trivial real-mode ISR for the expected vector.
        //
        // mov byte ptr [0x2000], 0xAA
        // iret
        const HANDLER_IP: u16 = 0x1100;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(HANDLER_IP), &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF]);
        write_ivt_entry(&mut m, expected_vector, HANDLER_IP, 0x0000);

        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(ENTRY_IP), &[0x90; 32]);
        m.mem.inner.borrow_mut().write_physical(0x2000, &[0x00]);

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Assert E1000 INTx level by enabling + setting a cause bit.
        let e1000 = m.e1000().expect("e1000 enabled");
        {
            let mut dev = e1000.borrow_mut();
            dev.mmio_write_reg(0x00D0, 4, aero_net_e1000::ICR_TXDW); // IMS
            dev.mmio_write_reg(0x00C8, 4, aero_net_e1000::ICR_TXDW); // ICS
            assert!(dev.irq_level());
        }

        // Prior to running a slice, the INTx level has not been synced into the platform
        // interrupt controller yet.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        // With IF=0, `run_slice` must not acknowledge the interrupt, but it should still sync PCI
        // INTx sources so the PIC sees the asserted line.
        init_real_mode_cpu(&mut m, ENTRY_IP, 0);
        m.cpu.state.halted = true;
        let exit = m.run_slice(5);
        assert_eq!(exit, RunExit::Halted { executed: 0 });
        assert!(m.cpu.pending.external_interrupts.is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            Some(expected_vector)
        );
        assert_eq!(m.read_physical_u8(0x2000), 0x00);

        // Once IF is set, the queued/pending interrupt should be delivered into the CPU core and
        // the handler should run.
        m.cpu.state.set_rflags(RFLAGS_IF);
        m.cpu.state.halted = true;
        let _ = m.run_slice(5);
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
        assert!(
            !m.cpu.state.halted,
            "CPU should wake from HLT once PCI INTx is delivered"
        );
    }

    #[test]
    fn pc_e1000_intx_is_delivered_via_ioapic_in_apic_mode() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        interrupts
            .borrow_mut()
            .set_mode(aero_platform::interrupts::PlatformInterruptMode::Apic);

        let pci_intx = m.pci_intx_router().expect("pc platform enabled");
        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);

        // Program IOAPIC entry for this GSI -> vector 0x60 (active-low, level-triggered).
        const VECTOR: u8 = 0x60;
        let low: u32 = u32::from(VECTOR) | (1 << 13) | (1 << 15); // polarity low + level triggered
        let redtbl_low = 0x10u32 + gsi * 2;
        let redtbl_high = redtbl_low + 1;
        {
            let mut bus = m.mem.inner.borrow_mut();
            bus.write_physical_u32(IOAPIC_MMIO_BASE + 0x00, redtbl_low);
            bus.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, low);
            bus.write_physical_u32(IOAPIC_MMIO_BASE + 0x00, redtbl_high);
            bus.write_physical_u32(IOAPIC_MMIO_BASE + 0x10, 0);
        }

        // Install a trivial real-mode ISR for the vector.
        //
        // mov byte ptr [0x2000], 0xAA
        // iret
        const HANDLER_IP: u16 = 0x1100;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(HANDLER_IP), &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF]);
        write_ivt_entry(&mut m, VECTOR, HANDLER_IP, 0x0000);

        // Program CPU at 0x1000 with enough NOPs to cover the instruction budgets below.
        const ENTRY_IP: u16 = 0x1000;
        m.mem
            .inner
            .borrow_mut()
            .write_physical(u64::from(ENTRY_IP), &[0x90; 32]);
        m.mem.inner.borrow_mut().write_physical(0x2000, &[0x00]);

        init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);

        // Assert E1000 INTx level by enabling + setting a cause bit.
        let e1000 = m.e1000().expect("e1000 enabled");
        {
            let mut dev = e1000.borrow_mut();
            dev.mmio_write_reg(0x00D0, 4, aero_net_e1000::ICR_TXDW); // IMS
            dev.mmio_write_reg(0x00C8, 4, aero_net_e1000::ICR_TXDW); // ICS
            assert!(dev.irq_level());
        }

        // Before the machine runs a slice, the INTx level has not been synced into the platform.
        assert_eq!(
            PlatformInterruptController::get_pending(&*interrupts.borrow()),
            None
        );

        // Simulate the CPU being halted: Tier-0 should wake it once the interrupt vector is
        // delivered (via IOAPIC + LAPIC).
        m.cpu.state.halted = true;
        let _ = m.run_slice(10);
        assert_eq!(m.read_physical_u8(0x2000), 0xAA);
        assert!(
            !m.cpu.state.halted,
            "CPU should wake from HLT once PCI INTx is delivered via IOAPIC"
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn machine_e1000_tx_ring_requires_bus_master_and_transmits_to_ring_backend() {
        use aero_ipc::ring::RingBuffer;
        use memory::MemoryBus as _;
        use std::sync::Arc;

        // Host rings (NET_TX is guest->host).
        let tx_ring = Arc::new(RingBuffer::new(16 * 1024));
        let rx_ring = Arc::new(RingBuffer::new(16 * 1024));

        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform: true,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: true,
            ..Default::default()
        })
        .unwrap();

        m.attach_l2_tunnel_rings(tx_ring.clone(), rx_ring);

        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;

        // BAR0 should be assigned by the machine's PCI BIOS POST helper.
        let bar0_base = {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg
                .bus_mut()
                .device_config(bdf)
                .and_then(|cfg| cfg.bar_range(0))
                .expect("missing E1000 BAR0")
                .base
        };

        // Guest memory layout.
        let tx_ring_base = 0x1000u64;
        let pkt_base = 0x2000u64;

        // Minimum Ethernet frame length: dst MAC (6) + src MAC (6) + ethertype (2).
        const MIN_L2_FRAME_LEN: usize = 14;
        let frame = vec![0x11u8; MIN_L2_FRAME_LEN];

        // Write packet bytes into guest RAM.
        m.mem.inner.borrow_mut().write_physical(pkt_base, &frame);

        // Legacy TX descriptor: buffer_addr + length + cmd(EOP|RS).
        let mut desc = [0u8; 16];
        desc[0..8].copy_from_slice(&pkt_base.to_le_bytes());
        desc[8..10].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        desc[10] = 0; // CSO
        desc[11] = (1 << 0) | (1 << 3); // EOP|RS
        desc[12] = 0; // status
        desc[13] = 0; // CSS
        desc[14..16].copy_from_slice(&0u16.to_le_bytes());
        m.mem.inner.borrow_mut().write_physical(tx_ring_base, &desc);

        // Program E1000 TX registers over MMIO (BAR0).
        {
            let mem = &mut *m.mem.inner.borrow_mut();
            mem.write_u32(bar0_base + 0x3800, tx_ring_base as u32); // TDBAL
            mem.write_u32(bar0_base + 0x3804, 0); // TDBAH
            mem.write_u32(bar0_base + 0x3808, 16 * 4); // TDLEN (4 descriptors)
            mem.write_u32(bar0_base + 0x3810, 0); // TDH
            mem.write_u32(bar0_base + 0x3818, 0); // TDT
            mem.write_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN

            // Doorbell: advance tail to include descriptor 0.
            mem.write_u32(bar0_base + 0x3818, 1); // TDT = 1
        }

        // Enable PCI decoding but keep bus mastering disabled.
        {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("E1000 device missing from PCI bus");
            // bit0 = IO space, bit1 = memory space
            cfg.set_command(0x3);
        }

        // Poll once: without BME, the E1000 model must not DMA, so no frame should appear.
        m.poll_network();
        assert!(tx_ring.try_pop().is_err(), "unexpected TX frame without bus mastering enabled");

        // Now enable Bus Mastering and poll again; the descriptor should be processed and the
        // resulting frame should appear on NET_TX.
        {
            let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
            let mut pci_cfg = pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("E1000 device missing from PCI bus");
            // bit0 = IO space, bit1 = memory space, bit2 = bus master
            cfg.set_command(0x7);
        }

        m.poll_network();
        assert_eq!(tx_ring.try_pop(), Ok(frame));
    }

    #[test]
    fn snapshot_restore_preserves_cpu_internal_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.inhibit_interrupts_for_one_instruction();
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        src.cpu.pending.inject_external_interrupt(0x22);

        let expected_inhibit = src.cpu.pending.interrupt_inhibit();
        let expected_external = src.cpu.pending.external_interrupts.clone();

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert_eq!(restored.cpu.pending.interrupt_inhibit(), expected_inhibit);
        assert_eq!(restored.cpu.pending.external_interrupts, expected_external);
    }
}
