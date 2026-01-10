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
mod post;
mod rom;
mod snapshot;

use std::collections::VecDeque;
use std::time::Duration;

use crate::memory::MemoryBus;
use crate::rtc::{CmosRtc, DateTime};
use crate::video::VideoDevice;
use machine::{BlockDevice, CpuState, DiskError, FirmwareMemory, MemoryAccess, Segment};

pub use acpi::{AcpiBuilder, AcpiPlacement};
pub use bda_time::{BdaTime, BDA_MIDNIGHT_FLAG_ADDR, BDA_TICK_COUNT_ADDR, TICKS_PER_DAY};
pub use interrupts::E820Entry;
pub use snapshot::BiosSnapshot;

pub const BIOS_BASE: u64 = 0x000F_0000;
pub const BIOS_SIZE: usize = 0x10000; // 64KiB
pub const BIOS_SEGMENT: u16 = 0xF000;

pub const IVT_BASE: u64 = 0x0000_0000;
pub const BDA_BASE: u64 = 0x0000_0400;

pub const EBDA_BASE: u64 = 0x0009_F000;
pub const EBDA_SIZE: usize = 0x1000;

pub const ACPI_TABLE_BASE: u64 = 0x000E_0000;
pub const ACPI_TABLE_SIZE: usize = 0x10000;

pub const INT10_STUB_OFFSET: u16 = 0xE300;
pub const INT13_STUB_OFFSET: u16 = 0xE400;
pub const INT15_STUB_OFFSET: u16 = 0xE600;
pub const INT16_STUB_OFFSET: u16 = 0xE700;
pub const INT1A_STUB_OFFSET: u16 = 0xE900;
pub const DEFAULT_INT_STUB_OFFSET: u16 = 0xEF00;

/// Memory interface required by the BIOS.
pub trait BiosBus: MemoryAccess + FirmwareMemory + machine::A20Gate {}
impl<T: MemoryAccess + FirmwareMemory + machine::A20Gate> BiosBus for T {}

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

impl MemoryBus for BiosMemoryBus<'_> {
    fn read_u8(&self, addr: u64) -> u8 {
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
}

impl Default for BiosConfig {
    fn default() -> Self {
        Self {
            memory_size_bytes: 16 * 1024 * 1024,
            boot_drive: 0x80,
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
    e820_map: Vec<E820Entry>,
    keyboard_queue: VecDeque<u16>,
    tty_output: Vec<u8>,
    /// INT 13h status code from the most recent disk operation (AH=01h).
    last_int13_status: u8,

    /// RSDP physical address (if ACPI tables were built).
    rsdp_addr: Option<u64>,

    acpi_builder: Box<dyn AcpiBuilder>,
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
            e820_map: Vec::new(),
            keyboard_queue: VecDeque::new(),
            tty_output: Vec::new(),
            last_int13_status: 0,
            rsdp_addr: None,
            acpi_builder: Box::new(acpi::FirmwareAcpiBuilder::default()),
        }
    }

    /// Initialize BDA time fields from the RTC.
    pub fn init<M: MemoryBus + ?Sized>(&mut self, memory: &mut M) {
        self.bda_time.write_to_bda(memory);
    }

    pub fn advance_time<M: MemoryBus + ?Sized>(&mut self, memory: &mut M, delta: Duration) {
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

    pub fn set_acpi_builder(&mut self, builder: Box<dyn AcpiBuilder>) {
        self.acpi_builder = builder;
    }

    pub fn push_key(&mut self, key: u16) {
        self.keyboard_queue.push_back(key);
    }

    pub fn post(&mut self, cpu: &mut CpuState, bus: &mut dyn BiosBus, disk: &mut dyn BlockDevice) {
        self.post_impl(cpu, bus, disk);
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
}

fn seg(selector: u16) -> Segment {
    Segment { selector }
}

fn disk_err_to_int13_status(err: DiskError) -> u8 {
    match err {
        DiskError::OutOfRange => 0x04, // sector not found / out of range
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use machine::{InMemoryDisk, MemoryAccess, PhysicalMemory, FLAG_IF};

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
        assert_eq!(rom_image[off + 0], 0xEA);
        assert_eq!(
            &rom_image[off + 1..off + 5],
            &[0x00, 0xE0, 0x00, 0xF0]
        );

        // Fallback stub at F000:E000: `cli; hlt; jmp $-2`.
        let stub = 0xE000usize;
        assert_eq!(&rom_image[stub..stub + 4], &[0xFA, 0xF4, 0xEB, 0xFE]);

        // ROM signature (optional).
        assert_eq!(&rom_image[BIOS_SIZE - 2..], &[0x55, 0xAA]);
    }

    #[test]
    fn post_initializes_ivt_vectors() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::default();
        let mut mem = PhysicalMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk);

        let read_vec = |mem: &PhysicalMemory, v: u8| -> (u16, u16) {
            let addr = (v as u64) * 4;
            (
                MemoryAccess::read_u16(mem, addr),
                MemoryAccess::read_u16(mem, addr + 2),
            )
        };

        assert_eq!(read_vec(&mem, 0x10), (INT10_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mem, 0x13), (INT13_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mem, 0x15), (INT15_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mem, 0x16), (INT16_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mem, 0x1A), (INT1A_STUB_OFFSET, BIOS_SEGMENT));
    }

    #[test]
    fn post_initializes_bda_and_ebda() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::default();
        let mut mem = PhysicalMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk);

        let ebda_segment = MemoryAccess::read_u16(&mem, BDA_BASE + 0x0E);
        assert_eq!(ebda_segment, (EBDA_BASE / 16) as u16);

        let base_mem_kb = MemoryAccess::read_u16(&mem, BDA_BASE + 0x13);
        assert_eq!(base_mem_kb, (EBDA_BASE / 1024) as u16);

        let ebda_kb = MemoryAccess::read_u16(&mem, EBDA_BASE);
        assert_eq!(ebda_kb, (EBDA_SIZE / 1024) as u16);
    }

    #[test]
    fn post_loads_boot_sector_and_sets_cpu_state() {
        let mut bios = Bios::new(BiosConfig {
            memory_size_bytes: 16 * 1024 * 1024,
            boot_drive: 0x80,
        });
        let mut cpu = CpuState::default();
        let mut mem = PhysicalMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0xAA));

        bios.post(&mut cpu, &mut mem, &mut disk);

        let loaded = mem.read_bytes(0x7C00, 512);
        assert_eq!(loaded[..510], vec![0xAA; 510]);
        assert_eq!(loaded[510], 0x55);
        assert_eq!(loaded[511], 0xAA);

        assert_eq!(cpu.cs.selector, 0x0000);
        assert_eq!(cpu.rip, 0x7C00);
        assert_eq!(cpu.rsp, 0x7C00);
        assert_eq!(cpu.rdx as u8, 0x80);
        assert_ne!(cpu.rflags & FLAG_IF, 0);
    }

    #[test]
    fn post_builds_acpi_rsdp_in_ebda() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::default();
        let mut mem = PhysicalMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk);

        let rsdp_addr = bios.rsdp_addr().expect("RSDP should be built");
        assert_eq!(rsdp_addr, EBDA_BASE + 0x100);

        let rsdp = mem.read_bytes(rsdp_addr, 36);
        assert_eq!(&rsdp[0..8], b"RSD PTR ");

        let checksum20 = rsdp[0..20]
            .iter()
            .copied()
            .fold(0u8, u8::wrapping_add);
        assert_eq!(checksum20, 0);
        let checksum36 = rsdp.iter().copied().fold(0u8, u8::wrapping_add);
        assert_eq!(checksum36, 0);

        let rsdt_addr = u32::from_le_bytes(rsdp[16..20].try_into().unwrap()) as u64;
        assert!(
            rsdt_addr >= ACPI_TABLE_BASE && rsdt_addr < ACPI_TABLE_BASE + ACPI_TABLE_SIZE as u64
        );
    }
}
