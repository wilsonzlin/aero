//! Legacy BIOS (HLE) implementation.
//!
//! Interrupt dispatch strategy
//! ---------------------------
//! We implement software interrupts architecturally:
//! - The CPU executes `INT imm8` normally in real mode (push FLAGS/CS/IP,
//!   clear IF/TF, load CS:IP from the IVT).
//! - The IVT points into a tiny BIOS ROM stub (one per interrupt we care
//!   about, plus a shared default handler).
//! - The stub executes `HLT` which the VM treats as a "BIOS hypercall" and
//!   calls back into [`Bios::dispatch_interrupt`], then execution resumes at
//!   the stub's `IRET`.
//!
//! This keeps the CPU-side implementation generic (good for a future JIT),
//! while letting us implement BIOS services as Rust functions.

mod acpi;
mod bda_time;
mod int10;
mod int10_vbe;
mod int1a;
mod interrupts;
mod ivt;
mod pci;
mod post;
mod rom;
mod snapshot;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use aero_cpu_core::state::{CpuState, Segment};
#[cfg(test)]
use memory::{DenseMemory, MapError, PhysicalMemoryBus};

use crate::memory::MemoryBus as FirmwareMemoryBus;
use crate::rtc::{CmosRtc, DateTime};
use crate::video::VideoDevice;

pub use acpi::{AcpiBuilder, AcpiInfo};
pub use bda_time::{BdaTime, BDA_MIDNIGHT_FLAG_ADDR, BDA_TICK_COUNT_ADDR, TICKS_PER_DAY};
pub use interrupts::E820Entry;
pub use pci::{PciConfigSpace, PciDevice};
pub use rom::build_bios_rom;
pub use snapshot::BiosSnapshot;

pub use aero_acpi::AcpiPlacement;

/// Base address of the system BIOS ROM in the 20-bit real-mode memory window.
pub const BIOS_BASE: u64 = 0x000F_0000;
/// Reset-vector alias of the BIOS ROM at the top of the 32-bit physical address space.
pub const BIOS_ALIAS_BASE: u64 = 0xFFFF_0000;
/// Size of the system BIOS ROM mapping (64 KiB).
pub const BIOS_SIZE: usize = 0x10000; // 64KiB
/// Real-mode segment for the system BIOS ROM mapping.
pub const BIOS_SEGMENT: u16 = 0xF000;
/// Offset of the x86 reset vector within the BIOS ROM segment (`F000:FFF0`).
pub const RESET_VECTOR_OFFSET: u64 = 0xFFF0;
/// Conventional reset vector physical address when the BIOS is mapped at [`BIOS_BASE`].
pub const RESET_VECTOR_PHYS: u64 = BIOS_BASE + RESET_VECTOR_OFFSET;
/// Architectural reset vector physical address when the BIOS ROM is aliased at [`BIOS_ALIAS_BASE`].
pub const RESET_VECTOR_ALIAS_PHYS: u64 = BIOS_ALIAS_BASE + RESET_VECTOR_OFFSET;

pub const IVT_BASE: u64 = 0x0000_0000;
pub const BDA_BASE: u64 = 0x0000_0400;

pub const EBDA_BASE: u64 = 0x0009_F000;
pub const EBDA_SIZE: usize = 0x1000;

// Re-export the shared PC platform constants so firmware callers don't need to depend on
// `aero-pc-constants` directly.
pub use aero_pc_constants::{
    PCIE_ECAM_BASE, PCIE_ECAM_END_BUS, PCIE_ECAM_SEGMENT, PCIE_ECAM_SIZE, PCIE_ECAM_START_BUS,
};

pub const INT10_STUB_OFFSET: u16 = 0xE300;
pub const INT13_STUB_OFFSET: u16 = 0xE400;
pub const INT15_STUB_OFFSET: u16 = 0xE600;
pub const INT16_STUB_OFFSET: u16 = 0xE700;
pub const INT1A_STUB_OFFSET: u16 = 0xE900;
pub const DEFAULT_INT_STUB_OFFSET: u16 = 0xEF00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskError {
    OutOfRange,
}

pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), DiskError>;

    fn size_in_sectors(&self) -> u64;
}

/// In-memory block device backed by a `Vec<u8>` of 512-byte sectors.
#[derive(Debug, Clone)]
pub struct InMemoryDisk {
    data: Vec<u8>,
}

impl InMemoryDisk {
    pub fn new(mut data: Vec<u8>) -> Self {
        if !data.len().is_multiple_of(512) {
            let new_len = (data.len() + 511) & !511;
            data.resize(new_len, 0);
        }
        Self { data }
    }

    pub fn from_boot_sector(sector: [u8; 512]) -> Self {
        Self {
            data: sector.to_vec(),
        }
    }
}

impl BlockDevice for InMemoryDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), DiskError> {
        let start = lba.checked_mul(512).ok_or(DiskError::OutOfRange)? as usize;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        if end > self.data.len() {
            return Err(DiskError::OutOfRange);
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        (self.data.len() / 512) as u64
    }
}

pub trait FirmwareMemory {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>);
}

pub trait A20Gate {
    fn set_a20_enabled(&mut self, enabled: bool);
    fn a20_enabled(&self) -> bool;
}

/// Memory interface required by the BIOS.
pub trait BiosBus: memory::MemoryBus + FirmwareMemory + A20Gate {}
impl<T: memory::MemoryBus + FirmwareMemory + A20Gate> BiosBus for T {}

/// Adapter that lets BIOS code reuse helpers written against the firmware-side [`MemoryBus`]
/// abstraction (used by the INT 10h text/VBE implementation).
pub(super) struct BiosMemoryBus<'a> {
    bus: &'a mut dyn BiosBus,
}

impl<'a> BiosMemoryBus<'a> {
    pub(super) fn new(bus: &'a mut dyn BiosBus) -> Self {
        Self { bus }
    }
}

impl FirmwareMemoryBus for BiosMemoryBus<'_> {
    fn read_u8(&mut self, addr: u64) -> u8 {
        self.bus.read_u8(addr)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.bus.write_u8(addr, value);
    }
}

#[derive(Debug, Clone)]
pub struct BiosConfig {
    /// Total guest RAM size.
    pub memory_size_bytes: u64,
    /// BIOS drive number exposed in `DL` when jumping to the boot sector.
    pub boot_drive: u8,
    /// Number of virtual CPUs exposed via SMBIOS and ACPI.
    pub cpu_count: u8,
    /// Whether to build and publish ACPI tables during POST.
    pub enable_acpi: bool,
    /// Fixed placement contract for ACPI tables written during POST.
    pub acpi_placement: AcpiPlacement,
    /// Mapping of PCI PIRQ[A-D] -> platform GSI used by both the ACPI DSDT `_PRT`
    /// and PCI Interrupt Line programming during enumeration.
    pub pirq_to_gsi: [u32; 4],
}

impl Default for BiosConfig {
    fn default() -> Self {
        // RSDP must live in the standard BIOS scan region (< 1MiB) and be 16-byte aligned.
        // We keep it in the EBDA so guests can find it by scanning the first KiB.
        let acpi_placement = AcpiPlacement {
            rsdp_addr: EBDA_BASE + 0x100,
            ..Default::default()
        };
        Self {
            memory_size_bytes: 16 * 1024 * 1024,
            boot_drive: 0x80,
            cpu_count: 1,
            enable_acpi: true,
            acpi_placement,
            // Match the default routing in `aero_acpi::AcpiConfig`.
            pirq_to_gsi: [10, 11, 12, 13],
        }
    }
}

/// Constructor input for [`Bios`].
///
/// This exists so both the legacy `Bios::new(rtc)` call sites and the newer
/// `Bios::new(config)` call sites can coexist while the codebase is still
/// converging on a single BIOS configuration surface.
#[derive(Debug, Clone)]
pub struct BiosInit {
    pub config: BiosConfig,
    pub rtc: CmosRtc,
}

impl From<BiosConfig> for BiosInit {
    fn from(config: BiosConfig) -> Self {
        let rtc = CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0));
        Self { config, rtc }
    }
}

impl From<CmosRtc> for BiosInit {
    fn from(rtc: CmosRtc) -> Self {
        Self {
            config: BiosConfig::default(),
            rtc,
        }
    }
}

pub struct Bios {
    pub rtc: CmosRtc,
    pub video: VideoDevice,
    bda_time: BdaTime,

    config: BiosConfig,
    acpi_builder: Box<dyn AcpiBuilder>,
    e820_map: Vec<E820Entry>,
    pci_devices: Vec<PciDevice>,
    keyboard_queue: VecDeque<u16>,
    /// Cached value for INT 10h AH=0F "Get current video mode" for snapshotting.
    ///
    /// The real-mode-visible source of truth lives in the BIOS Data Area (0x0449). This field
    /// exists so BIOS snapshots can restore the last reported mode without needing a memory bus.
    video_mode: u8,
    tty_output: Vec<u8>,
    /// INT 13h status code from the most recent disk operation (AH=01h).
    last_int13_status: u8,

    /// RSDP physical address (if ACPI tables were built).
    rsdp_addr: Option<u64>,
    acpi_reclaimable: Option<(u64, u64)>,
    acpi_nvs: Option<(u64, u64)>,

    /// SMBIOS Entry Point Structure physical address (if SMBIOS tables were built).
    smbios_eps_addr: Option<u32>,
}

impl Bios {
    pub fn new(init: impl Into<BiosInit>) -> Self {
        let BiosInit { config, rtc } = init.into();
        Self::new_with_rtc(config, rtc)
    }

    pub fn new_with_rtc(config: BiosConfig, rtc: CmosRtc) -> Self {
        let bda_time = BdaTime::from_rtc(&rtc);
        Self {
            rtc,
            video: VideoDevice::new(),
            bda_time,
            config,
            acpi_builder: Box::new(acpi::AeroAcpiBuilder),
            e820_map: Vec::new(),
            pci_devices: Vec::new(),
            keyboard_queue: VecDeque::new(),
            video_mode: 0x03,
            tty_output: Vec::new(),
            last_int13_status: 0,
            rsdp_addr: None,
            acpi_reclaimable: None,
            acpi_nvs: None,
            smbios_eps_addr: None,
        }
    }

    /// Initialize BDA time fields from the RTC.
    pub fn init<M: FirmwareMemoryBus + ?Sized>(&mut self, memory: &mut M) {
        self.bda_time.write_to_bda(memory);
    }

    pub fn advance_time<M: FirmwareMemoryBus + ?Sized>(&mut self, memory: &mut M, delta: Duration) {
        self.rtc.advance(delta);
        self.bda_time.advance(memory, delta);
    }

    pub fn config(&self) -> &BiosConfig {
        &self.config
    }

    pub fn tty_output(&self) -> &[u8] {
        &self.tty_output
    }

    pub fn rsdp_addr(&self) -> Option<u64> {
        self.rsdp_addr
    }

    pub fn smbios_eps_addr(&self) -> Option<u32> {
        self.smbios_eps_addr
    }

    pub fn pci_devices(&self) -> &[PciDevice] {
        &self.pci_devices
    }

    pub fn push_key(&mut self, key: u16) {
        self.keyboard_queue.push_back(key);
    }

    pub fn post(&mut self, cpu: &mut CpuState, bus: &mut dyn BiosBus, disk: &mut dyn BlockDevice) {
        self.post_with_pci(cpu, bus, disk, None);
    }

    pub fn post_with_pci(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
        pci: Option<&mut dyn PciConfigSpace>,
    ) {
        self.post_impl(cpu, bus, disk, pci);
    }

    pub fn dispatch_interrupt(
        &mut self,
        vector: u8,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
    ) {
        interrupts::dispatch_interrupt(self, vector, cpu, bus, disk);
    }

    pub fn set_acpi_builder(&mut self, builder: Box<dyn AcpiBuilder>) {
        self.acpi_builder = builder;
    }
}

fn set_real_mode_seg(seg: &mut Segment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

fn disk_err_to_int13_status(err: DiskError) -> u8 {
    match err {
        DiskError::OutOfRange => 0x04, // sector not found / out of range
    }
}

#[cfg(test)]
pub(super) struct TestMemory {
    a20_enabled: bool,
    inner: PhysicalMemoryBus,
}

#[cfg(test)]
impl TestMemory {
    pub(super) fn new(size: u64) -> Self {
        let ram = DenseMemory::new(size).expect("guest RAM allocation failed");
        Self {
            a20_enabled: false,
            inner: PhysicalMemoryBus::new(Box::new(ram)),
        }
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }

    pub(super) fn read_bytes(&mut self, paddr: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.read_physical(paddr, &mut out);
        out
    }
}

#[cfg(test)]
impl A20Gate for TestMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }
}

#[cfg(test)]
impl FirmwareMemory for TestMemory {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        match self.inner.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                let already_mapped = self
                    .inner
                    .rom_regions()
                    .iter()
                    .any(|r| r.start == base && r.data.len() == len);
                if !already_mapped {
                    panic!("unexpected ROM mapping overlap at 0x{base:016x}");
                }
            }
            Err(MapError::AddressOverflow) => {
                panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})");
            }
        }
    }
}

#[cfg(test)]
impl memory::MemoryBus for TestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if self.a20_enabled {
            self.inner.read_physical(paddr, buf);
            return;
        }

        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            *slot = self.inner.read_physical_u8(addr);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if self.a20_enabled {
            self.inner.write_physical(paddr, buf);
            return;
        }

        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            self.inner.write_physical_u8(addr, byte);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_cpu_core::state::{gpr, RFLAGS_IF};

    fn boot_sector(pattern: u8) -> [u8; 512] {
        let mut sector = [pattern; 512];
        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    #[test]
    fn bios_rom_contains_a_reset_vector_far_jump() {
        let rom_image = rom::build_bios_rom();
        assert_eq!(rom_image.len(), BIOS_SIZE);

        // Reset vector at F000:FFF0 should be a FAR JMP to F000:E000.
        let off = 0xFFF0usize;
        assert_eq!(rom_image[off], 0xEA);
        assert_eq!(&rom_image[off + 1..off + 5], &[0x00, 0xE0, 0x00, 0xF0]);

        // Fallback stub at F000:E000: `cli; hlt; jmp $-2`.
        let stub = 0xE000usize;
        assert_eq!(&rom_image[stub..stub + 4], &[0xFA, 0xF4, 0xEB, 0xFE]);

        // ROM signature (optional).
        assert_eq!(&rom_image[BIOS_SIZE - 2..], &[0x55, 0xAA]);
    }

    #[test]
    fn post_initializes_ivt_vectors() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk);

        let read_vec = |mem: &mut TestMemory, v: u8| -> (u16, u16) {
            let addr = (v as u64) * 4;
            (mem.read_u16(addr), mem.read_u16(addr + 2))
        };

        assert_eq!(read_vec(&mut mem, 0x10), (INT10_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mut mem, 0x13), (INT13_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mut mem, 0x15), (INT15_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mut mem, 0x16), (INT16_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mut mem, 0x1A), (INT1A_STUB_OFFSET, BIOS_SEGMENT));
    }

    #[test]
    fn post_initializes_bda_and_ebda() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk);

        let ebda_segment = mem.read_u16(BDA_BASE + 0x0E);
        assert_eq!(ebda_segment, (EBDA_BASE / 16) as u16);

        let base_mem_kb = mem.read_u16(BDA_BASE + 0x13);
        assert_eq!(base_mem_kb, (EBDA_BASE / 1024) as u16);

        let ebda_kb = mem.read_u16(EBDA_BASE);
        assert_eq!(ebda_kb, (EBDA_SIZE / 1024) as u16);
    }

    #[test]
    fn post_loads_boot_sector_and_sets_cpu_state() {
        let mut bios = Bios::new(BiosConfig {
            memory_size_bytes: 16 * 1024 * 1024,
            boot_drive: 0x80,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0xAA));

        bios.post(&mut cpu, &mut mem, &mut disk);

        let loaded = mem.read_bytes(0x7C00, 512);
        assert_eq!(loaded[..510], vec![0xAA; 510]);
        assert_eq!(loaded[510], 0x55);
        assert_eq!(loaded[511], 0xAA);

        assert_eq!(cpu.segments.cs.selector, 0x0000);
        assert_eq!(cpu.rip(), 0x7C00);
        assert_eq!(cpu.gpr[gpr::RSP] as u16, 0x7C00);
        assert_eq!(cpu.gpr[gpr::RDX] as u8, 0x80);
        assert!(cpu.get_flag(RFLAGS_IF));
    }

    #[test]
    fn post_maps_bios_rom_at_the_reset_vector_alias() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk);

        // BIOS POST enables A20 before handing control to the boot sector.
        assert!(mem.a20_enabled());

        // The ROM is mirrored at the architectural reset-vector alias (0xFFFF_FFF0).
        assert_eq!(mem.read_u8(RESET_VECTOR_ALIAS_PHYS), 0xEA);
        assert_eq!(
            mem.read_bytes(RESET_VECTOR_ALIAS_PHYS, 5),
            vec![0xEA, 0x00, 0xE0, 0x00, 0xF0]
        );
    }

    #[test]
    fn post_builds_acpi_rsdp_in_ebda() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk);

        let rsdp_addr = bios.rsdp_addr().expect("RSDP should be built");
        assert_eq!(rsdp_addr, EBDA_BASE + 0x100);

        let rsdp = mem.read_bytes(rsdp_addr, 36);
        assert_eq!(&rsdp[0..8], b"RSD PTR ");

        let checksum20 = rsdp[0..20].iter().copied().fold(0u8, u8::wrapping_add);
        assert_eq!(checksum20, 0);
        let checksum36 = rsdp.iter().copied().fold(0u8, u8::wrapping_add);
        assert_eq!(checksum36, 0);

        let rsdt_addr = u32::from_le_bytes(rsdp[16..20].try_into().unwrap()) as u64;
        let (reclaim_base, reclaim_len) = bios
            .acpi_reclaimable
            .expect("ACPI reclaimable window should be tracked");
        assert!(reclaim_len > 0);
        assert!(rsdt_addr >= reclaim_base && rsdt_addr < reclaim_base + reclaim_len);
    }
}
