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
mod eltorito;
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

pub use acpi::{AcpiBuilder, AcpiInfo, BiosAcpiError};
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

// Keyboard buffer layout within the BIOS Data Area (segment 0x40).
//
// The BDA models a ring buffer of 16-bit "keystroke words" (scan code in AH, ASCII in AL). The
// legacy convention uses `head == tail` to indicate an empty buffer, so one slot is intentionally
// left unused to avoid ambiguity between "empty" and "full".
const BDA_KEYBOARD_FLAGS_OFFSET: u64 = 0x17; // 0x40:0x17 -> 0x417 absolute
const BDA_KEYBOARD_BUF_HEAD_OFFSET: u64 = 0x1A; // 0x40:0x1A -> 0x41A absolute
const BDA_KEYBOARD_BUF_TAIL_OFFSET: u64 = 0x1C; // 0x40:0x1C -> 0x41C absolute
const BDA_KEYBOARD_BUF_START: u16 = 0x001E; // 0x40:0x1E -> 0x41E absolute
const BDA_KEYBOARD_BUF_END: u16 = 0x003E; // 0x40:0x3E -> 0x43E absolute (exclusive)
const BDA_KEYBOARD_BUF_START_PTR_OFFSET: u64 = 0x80; // 0x40:0x80 -> 0x480 absolute
const BDA_KEYBOARD_BUF_END_PTR_OFFSET: u64 = 0x82; // 0x40:0x82 -> 0x482 absolute
const KEYBOARD_QUEUE_CAPACITY: usize =
    ((BDA_KEYBOARD_BUF_END - BDA_KEYBOARD_BUF_START) / 2).saturating_sub(1) as usize;

pub const EBDA_BASE: u64 = 0x0009_F000;
pub const EBDA_SIZE: usize = 0x1000;

/// Maximum size of the BIOS "TTY output" debug buffer.
///
/// The BIOS records INT 10h AH=0Eh teletype output and fatal error messages here so tests and
/// debuggers can inspect guest-visible output. Keep this bounded to avoid untrusted guests growing
/// host memory unboundedly.
pub const MAX_TTY_OUTPUT_BYTES: usize = 64 * 1024; // 64KiB

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
/// IVT vector 0x1F: pointer to the 8x8 graphics character table (font).
pub const VGA_FONT_8X8_OFFSET: u16 = 0xC000;
/// Offset of the built-in 8x16 font table returned by INT 10h AH=11h AL=30h ("Get Font
/// Information").
pub const VGA_FONT_8X16_OFFSET: u16 = 0xD000;
/// IVT vector 0x1E: diskette parameter table pointer.
pub const DISKETTE_PARAM_TABLE_OFFSET: u16 = 0xE100;
/// IVT vector 0x41/0x46: legacy fixed disk parameter table pointer(s).
pub const FIXED_DISK_PARAM_TABLE_OFFSET: u16 = 0xE110;
/// IVT vector 0x1D: legacy video parameter table pointer.
pub const VIDEO_PARAM_TABLE_OFFSET: u16 = 0xE140;

// ---------------------------------------------------------------------------------------------
// Legacy VGA text-mode framebuffer helpers.
// ---------------------------------------------------------------------------------------------
//
// The emulator/harness may or may not model a full VGA PCI device, but most PC firmware and boot
// code can still surface fatal errors by writing directly into the legacy VGA text buffer.
//
// We keep this minimal and conservative:
// - always target the color text window at physical 0xB8000,
// - always render on line 0, and
// - always use attribute 0x07 (light gray on black).
const VGA_TEXT_BASE: u64 = 0x000B_8000;
const VGA_TEXT_COLS: usize = 80;
const VGA_TEXT_ATTR: u8 = 0x07;

fn render_message_to_vga_text_line0(bus: &mut dyn BiosBus, msg: &str) {
    let mut line = [0u8; VGA_TEXT_COLS * 2];
    for pair in line.chunks_exact_mut(2) {
        pair[0] = b' ';
        pair[1] = VGA_TEXT_ATTR;
    }

    for (col, &b) in msg.as_bytes().iter().take(VGA_TEXT_COLS).enumerate() {
        if b == b'\n' || b == b'\r' {
            break;
        }
        line[col * 2] = b;
        // attribute byte already set to VGA_TEXT_ATTR
    }

    bus.write_physical(VGA_TEXT_BASE, &line);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskError {
    OutOfRange,
}

/// El Torito boot media type (as reported by INT 13h AH=4Bh).
///
/// This is a subset of the classic El Torito BIOS boot specification media type encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(dead_code)]
pub(super) enum ElToritoBootMediaType {
    /// El Torito "no emulation" mode.
    NoEmulation = 0x00,
    /// 1.2MiB floppy emulation.
    Floppy1200KiB = 0x01,
    /// 1.44MiB floppy emulation.
    Floppy1440KiB = 0x02,
    /// 2.88MiB floppy emulation.
    Floppy2880KiB = 0x03,
    /// Hard disk emulation.
    HardDisk = 0x04,
}

/// Cached El Torito CD boot metadata captured during POST.
///
/// When the BIOS boots via an El Torito boot catalog entry, boot images (e.g. ISOLINUX) may query
/// this information via INT 13h AH=4Bh ("El Torito disk emulation services").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ElToritoBootInfo {
    pub(super) media_type: ElToritoBootMediaType,
    /// Boot drive number passed to the boot image (DL).
    pub(super) boot_drive: u8,
    /// BIOS controller index for the boot device (usually 0).
    pub(super) controller_index: u8,
    /// Boot catalog sector (LBA) on the CD-ROM image (if known).
    pub(super) boot_catalog_lba: Option<u32>,
    /// Boot image start sector (RBA/LBA) on the CD-ROM image (if known).
    pub(super) boot_image_lba: Option<u32>,
    /// Real-mode segment the boot image was loaded to (if known).
    pub(super) load_segment: Option<u16>,
    /// Number of 512-byte sectors loaded for the initial boot image (if known).
    pub(super) sector_count: Option<u16>,
}

/// ISO9660 / ATAPI CD-ROM sector size used by El Torito and INT 13h CD extensions.
pub const CDROM_SECTOR_SIZE: usize = 2048;

/// BIOS disk sector size used by the legacy INT 13h block-device path.
pub const BIOS_SECTOR_SIZE: usize = 512;

/// Minimal 512-byte-sector read interface used by the legacy BIOS INT 13h implementation.
///
/// # Canonical trait note
///
/// This trait is **firmware-specific** and should not be used as a general-purpose disk image or
/// device/controller abstraction. Outside of BIOS code, prefer the canonical synchronous disk
/// traits (`aero_storage::StorageBackend` / `aero_storage::VirtualDisk`) and adapt as needed. See
/// `docs/20-storage-trait-consolidation.md`.
pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; BIOS_SECTOR_SIZE]) -> Result<(), DiskError>;

    fn size_in_sectors(&self) -> u64;
}

/// Minimal 2048-byte-sector read interface used by the legacy BIOS INT 13h CD-ROM path.
///
/// This models an El Torito / ATAPI CD-ROM device exposed via BIOS drive numbers `0xE0..`.
///
/// The BIOS INT 13h EDD semantics for CD-ROMs are *not* the same as for hard disks:
/// - AH=48 must report `bytes/sector = 2048`.
/// - AH=42 must interpret the DAP's `LBA` and `count` in **2048-byte sectors** (not 512-byte
///   "virtual sectors").
///
/// Keeping this as a separate trait makes the sector size contract explicit and prevents
/// accidental mixing of 512-byte disk semantics with 2048-byte CD-ROM semantics.
pub trait CdromDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; CDROM_SECTOR_SIZE])
        -> Result<(), DiskError>;

    /// Total number of 2048-byte sectors addressable via `LBA` in [`CdromDevice::read_sector`].
    fn size_in_sectors(&self) -> u64;
}

/// In-memory block device backed by a `Vec<u8>` of 512-byte sectors.
#[derive(Debug, Clone)]
pub struct InMemoryDisk {
    data: Vec<u8>,
}

impl InMemoryDisk {
    pub fn new(mut data: Vec<u8>) -> Self {
        if !data.len().is_multiple_of(BIOS_SECTOR_SIZE) {
            let new_len = (data.len() + (BIOS_SECTOR_SIZE - 1)) & !(BIOS_SECTOR_SIZE - 1);
            data.resize(new_len, 0);
        }
        Self { data }
    }

    pub fn from_boot_sector(sector: [u8; BIOS_SECTOR_SIZE]) -> Self {
        Self {
            data: sector.to_vec(),
        }
    }
}

impl BlockDevice for InMemoryDisk {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; BIOS_SECTOR_SIZE]) -> Result<(), DiskError> {
        let start = lba
            .checked_mul(BIOS_SECTOR_SIZE as u64)
            .ok_or(DiskError::OutOfRange)? as usize;
        let end = start
            .checked_add(BIOS_SECTOR_SIZE)
            .ok_or(DiskError::OutOfRange)?;
        if end > self.data.len() {
            return Err(DiskError::OutOfRange);
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        (self.data.len() / BIOS_SECTOR_SIZE) as u64
    }
}

/// In-memory CD-ROM device backed by a `Vec<u8>` of 2048-byte sectors.
#[derive(Debug, Clone)]
pub struct InMemoryCdrom {
    data: Vec<u8>,
}

impl InMemoryCdrom {
    pub fn new(mut data: Vec<u8>) -> Self {
        if !data.len().is_multiple_of(CDROM_SECTOR_SIZE) {
            let new_len = (data.len() + (CDROM_SECTOR_SIZE - 1)) & !(CDROM_SECTOR_SIZE - 1);
            data.resize(new_len, 0);
        }
        Self { data }
    }
}

impl CdromDevice for InMemoryCdrom {
    fn read_sector(
        &mut self,
        lba: u64,
        buf: &mut [u8; CDROM_SECTOR_SIZE],
    ) -> Result<(), DiskError> {
        let start = lba
            .checked_mul(CDROM_SECTOR_SIZE as u64)
            .ok_or(DiskError::OutOfRange)? as usize;
        let end = start
            .checked_add(CDROM_SECTOR_SIZE)
            .ok_or(DiskError::OutOfRange)?;
        if end > self.data.len() {
            return Err(DiskError::OutOfRange);
        }
        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        (self.data.len() / CDROM_SECTOR_SIZE) as u64
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

    fn read_u16(&mut self, addr: u64) -> u16 {
        self.bus.read_u16(addr)
    }

    fn write_u16(&mut self, addr: u64, value: u16) {
        self.bus.write_u16(addr, value);
    }

    fn read_u32(&mut self, addr: u64) -> u32 {
        self.bus.read_u32(addr)
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        self.bus.write_u32(addr, value);
    }

    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        // Forward bulk reads to the canonical guest memory bus so callers like the VBE framebuffer
        // clear path don't devolve into byte-at-a-time MMIO accesses.
        self.bus.read_physical(paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        // Forward bulk writes to the canonical guest memory bus so callers like the VBE
        // framebuffer clear path can take advantage of efficient RAM copies / aligned MMIO writes.
        self.bus.write_physical(paddr, buf);
    }

    fn read_bytes(&mut self, addr: u64, out: &mut [u8]) {
        // `FirmwareMemoryBus` exposes both `read_physical` and `read_bytes`; keep them consistent
        // and bulk-friendly.
        self.read_physical(addr, out);
    }

    fn write_bytes(&mut self, addr: u64, bytes: &[u8]) {
        self.write_physical(addr, bytes);
    }
}

/// BIOS boot device class used by the (optional) host-configured boot-order policy.
///
/// Note: Aero’s BIOS boot selection is currently driven solely by [`BiosConfig::boot_drive`]
/// (i.e. an explicit `DL` value). This enum and the related config fields exist primarily for
/// snapshot forward-compatibility with a future multi-device boot-selection implementation.
///
/// This is intentionally a small, stable enum so it can be snapshotted as a compact `u8` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BiosBootDevice {
    /// Boot from a BIOS fixed disk (`DL=0x80..`).
    Hdd = 0,
    /// Boot from an El Torito CD-ROM (`DL=0xE0..`).
    Cdrom = 1,
    /// Boot from a floppy disk (`DL=0x00..`).
    Floppy = 2,
}

#[derive(Debug, Clone)]
pub struct BiosConfig {
    /// Total guest RAM size.
    pub memory_size_bytes: u64,
    /// BIOS drive number exposed in `DL` when jumping to the boot sector.
    pub boot_drive: u8,
    /// Number of virtual CPUs exposed via SMBIOS and ACPI.
    pub cpu_count: u8,
    /// Deterministic seed used to generate the SMBIOS Type 1 "System UUID".
    ///
    /// Real OSes (notably Windows) use this UUID as part of machine identity.
    /// Keeping the default as `0` preserves deterministic tests while letting
    /// runtimes choose stable per-VM identities by overriding this value.
    pub smbios_uuid_seed: u64,
    /// Whether to build and publish ACPI tables during POST.
    pub enable_acpi: bool,
    /// Fixed placement contract for ACPI tables written during POST.
    pub acpi_placement: AcpiPlacement,
    /// Mapping of PCI PIRQ[A-D] -> platform GSI used by both the ACPI DSDT `_PRT`
    /// and PCI Interrupt Line programming during enumeration.
    pub pirq_to_gsi: [u32; 4],
    /// Optional override for the VBE linear framebuffer base address reported by the BIOS.
    ///
    /// When unset, the BIOS keeps the default RAM-backed base address
    /// ([`crate::video::vbe::VbeDevice::LFB_BASE_DEFAULT`]).
    pub vbe_lfb_base: Option<u32>,

    /// Optional host-configurable boot-order policy (currently unused by the BIOS boot path).
    ///
    /// The classic BIOS boot drive number passed in `DL` is stored separately in
    /// [`BiosConfig::boot_drive`], and **that value is what Aero BIOS currently uses** to select the
    /// boot path during POST (HDD/floppy boot sector vs El Torito CD boot).
    ///
    /// This list is snapshotted/restored to keep the BIOS snapshot format forward-compatible with a
    /// future “try devices in order” selection algorithm.
    pub boot_order: Vec<BiosBootDevice>,
    /// BIOS drive number to use when booting from a CD-ROM device class (boot-order policy).
    ///
    /// The conventional range for El Torito CD-ROM boot devices is `0xE0..=0xEF`.
    ///
    /// When [`BiosConfig::boot_from_cd_if_present`] is set and a CD-ROM backend is provided, POST
    /// will attempt an El Torito boot using this drive number.
    pub cd_boot_drive: u8,
    /// Host convenience policy flag for “CD-first when present” boot selection.
    ///
    /// When set and a CD-ROM backend is provided to [`Bios::post`], POST will attempt to boot from
    /// CD-ROM first (using [`BiosConfig::cd_boot_drive`]) and fall back to the configured
    /// [`BiosConfig::boot_drive`] on failure.
    pub boot_from_cd_if_present: bool,
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
            smbios_uuid_seed: 0,
            enable_acpi: true,
            acpi_placement,
            // Match the default routing in `aero_acpi::AcpiConfig`.
            pirq_to_gsi: aero_pci_routing::DEFAULT_PIRQ_TO_GSI,
            vbe_lfb_base: None,
            boot_order: vec![BiosBootDevice::Hdd],
            cd_boot_drive: 0xE0,
            boot_from_cd_if_present: false,
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
    unhandled_interrupt_log_count: u32,
    /// Cached value for INT 10h AH=0F "Get current video mode" for snapshotting.
    ///
    /// The real-mode-visible source of truth lives in the BIOS Data Area (0x0449). This field
    /// exists so BIOS snapshots can restore the last reported mode without needing a memory bus.
    video_mode: u8,
    tty_output: Vec<u8>,
    /// Start offset of the valid window within [`Bios::tty_output`].
    ///
    /// We maintain a rolling log by advancing this offset as the buffer overflows. Once the start
    /// offset grows large enough, we compact the vector to avoid unbounded capacity growth.
    tty_output_start: usize,
    /// INT 13h status code from the most recent disk operation (AH=01h).
    last_int13_status: u8,
    /// El Torito CD boot metadata captured during POST.
    el_torito_boot_info: Option<ElToritoBootInfo>,

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
        let mut video = VideoDevice::new();
        if let Some(base) = config.vbe_lfb_base {
            video.vbe.lfb_base = base;
        }
        Self {
            rtc,
            video,
            bda_time,
            config,
            acpi_builder: Box::new(acpi::AeroAcpiBuilder),
            e820_map: Vec::new(),
            pci_devices: Vec::new(),
            keyboard_queue: VecDeque::new(),
            unhandled_interrupt_log_count: 0,
            video_mode: 0x03,
            tty_output: Vec::new(),
            tty_output_start: 0,
            last_int13_status: 0,
            el_torito_boot_info: None,
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

    /// Return the BIOS-cached "current video mode" value used by INT 10h AH=0F ("Get Current Video
    /// Mode") and BIOS snapshots.
    ///
    /// The authoritative guest-visible source of truth is the BIOS Data Area (BDA) at `0x0449`, but
    /// this cached value exists so snapshot/restore and host-side introspection can reason about
    /// the active legacy VGA mode without requiring a memory bus.
    pub fn cached_video_mode(&self) -> u8 {
        self.video_mode
    }

    /// Returns `true` if the most recent boot path was an El Torito CD-ROM boot.
    ///
    /// This is intended for host-level integrations that want to report which device firmware
    /// actually booted from (for example when using the "CD-first when present" policy).
    pub fn booted_from_cdrom(&self) -> bool {
        self.el_torito_boot_info.is_some()
    }

    /// Returns the configured BIOS boot drive number used when transferring control to the boot
    /// sector / El Torito boot image.
    pub fn boot_drive(&self) -> u8 {
        self.config.boot_drive
    }

    /// Returns whether the host-configured firmware "CD-first when present" policy is enabled.
    pub fn boot_from_cd_if_present(&self) -> bool {
        self.config.boot_from_cd_if_present
    }

    /// Returns the BIOS drive number to use for CD-ROM boot when the "CD-first when present" policy
    /// is enabled.
    pub fn cd_boot_drive(&self) -> u8 {
        self.config.cd_boot_drive
    }

    /// Set the BIOS boot drive number exposed in `DL` when transferring control to the boot
    /// sector.
    ///
    /// This is host-controlled policy (e.g. "boot from HDD" vs "boot from CD") rather than guest
    /// state. The value is stored in the BIOS configuration so it is captured/restored by BIOS
    /// snapshots and can be inherited by higher-level machine reset logic.
    ///
    /// This value is also used during firmware POST to populate BIOS Data Area (BDA) drive count
    /// fields.
    ///
    /// Note: Changing this does not retroactively update the already-initialized BDA fields.
    /// Callers that want the change to take effect for firmware POST/boot should perform a reset.
    pub fn set_boot_drive(&mut self, boot_drive: u8) {
        self.config.boot_drive = boot_drive;
    }

    /// Configure the host boot policy flag for "boot from CD-ROM first when present".
    ///
    /// When enabled and a CD-ROM backend is provided to [`Bios::post`], firmware POST will attempt
    /// an El Torito boot from [`BiosConfig::cd_boot_drive`] and fall back to the configured
    /// [`BiosConfig::boot_drive`] on failure.
    ///
    /// This is host-controlled policy rather than guest-visible state; it is stored in the BIOS
    /// configuration so snapshots can preserve it and higher-level machine reset logic can
    /// re-apply it deterministically.
    pub fn set_boot_from_cd_if_present(&mut self, enabled: bool) {
        self.config.boot_from_cd_if_present = enabled;
    }

    /// Set the BIOS drive number to use for CD-ROM boot when
    /// [`BiosConfig::boot_from_cd_if_present`] is enabled.
    pub fn set_cd_boot_drive(&mut self, cd_boot_drive: u8) {
        self.config.cd_boot_drive = cd_boot_drive;
    }

    pub fn tty_output(&self) -> &[u8] {
        let start = self.tty_output_start.min(self.tty_output.len());
        &self.tty_output[start..]
    }

    /// Clear the BIOS "TTY output" buffer.
    ///
    /// This buffer is a best-effort debug aid used by the HLE BIOS to capture:
    /// - INT 10h teletype output (AH=0Eh), and
    /// - early-boot panic strings (e.g. when the boot sector cannot be loaded).
    ///
    /// It is not intended to be a stable, user-facing console API.
    pub fn clear_tty_output(&mut self) {
        self.tty_output.clear();
        self.tty_output_start = 0;
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
        // Keep the queue bounded to the amount we can mirror into the BIOS Data Area keyboard
        // buffer (see `sync_keyboard_bda`). When full, drop the oldest entry to preserve the most
        // recent input.
        if KEYBOARD_QUEUE_CAPACITY == 0 {
            return;
        }
        if self.keyboard_queue.len() >= KEYBOARD_QUEUE_CAPACITY {
            // Drop enough entries to make space for the new key even if callers somehow exceeded
            // the cap (e.g. restored from an older snapshot format).
            let overflow = self
                .keyboard_queue
                .len()
                .saturating_add(1)
                .saturating_sub(KEYBOARD_QUEUE_CAPACITY);
            for _ in 0..overflow {
                self.keyboard_queue.pop_front();
            }
        }
        self.keyboard_queue.push_back(key);
    }

    /// Synchronize the BIOS keyboard queue into the conventional BIOS Data Area (BDA) keyboard
    /// ring buffer.
    ///
    /// This is a best-effort compatibility mirror so software that polls the BDA head/tail pointers
    /// (0x40:0x1A/0x1C) can observe pending keys without invoking INT 16h.
    pub fn sync_keyboard_bda(&self, bus: &mut dyn BiosBus) {
        interrupts::sync_keyboard_bda(self, bus);
    }

    pub fn post(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
        cdrom: Option<&mut dyn CdromDevice>,
    ) {
        self.post_impl(cpu, bus, disk, cdrom, None);
    }

    pub fn post_with_pci(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
        cdrom: Option<&mut dyn CdromDevice>,
        pci: Option<&mut dyn PciConfigSpace>,
    ) {
        self.post_impl(cpu, bus, disk, cdrom, pci);
    }

    pub fn post_with_cdrom(
        &mut self,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
        cdrom: &mut dyn CdromDevice,
    ) {
        self.post(cpu, bus, disk, Some(cdrom));
    }

    pub fn dispatch_interrupt(
        &mut self,
        vector: u8,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
        cdrom: Option<&mut dyn CdromDevice>,
    ) {
        interrupts::dispatch_interrupt(self, vector, cpu, bus, disk, cdrom);
    }

    pub fn dispatch_interrupt_with_cdrom(
        &mut self,
        vector: u8,
        cpu: &mut CpuState,
        bus: &mut dyn BiosBus,
        disk: &mut dyn BlockDevice,
        cdrom: Option<&mut dyn CdromDevice>,
    ) {
        interrupts::dispatch_interrupt_with_cdrom(self, vector, cpu, bus, disk, cdrom);
    }

    pub fn set_acpi_builder(&mut self, builder: Box<dyn AcpiBuilder>) {
        self.acpi_builder = builder;
    }

    fn push_tty_byte(&mut self, byte: u8) {
        self.push_tty_bytes(std::slice::from_ref(&byte));
    }

    fn push_tty_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        let max = MAX_TTY_OUTPUT_BYTES;

        // If a single write is larger than the whole buffer, keep only the tail.
        if bytes.len() >= max {
            self.tty_output.clear();
            self.tty_output_start = 0;
            self.tty_output
                .extend_from_slice(&bytes[bytes.len().saturating_sub(max)..]);
            return;
        }

        // Keep the start offset in-bounds, even if other code left it inconsistent.
        let mut start = self.tty_output_start.min(self.tty_output.len());
        let mut len = self.tty_output.len();

        // Ensure the existing window does not exceed `max` (e.g. restored from an older snapshot).
        let mut window_len = len - start;
        if window_len > max {
            start = len - max;
            window_len = max;
        }

        let drop = window_len.saturating_add(bytes.len()).saturating_sub(max);
        if drop != 0 {
            // If the discarded prefix would grow too large, compact before appending so we don't
            // temporarily grow the underlying `Vec` to an oversized allocation (e.g. when callers
            // append in large chunks).
            if start.saturating_add(drop) >= max {
                self.tty_output.copy_within(start.., 0);
                self.tty_output.truncate(window_len);
                start = 0;
                len = window_len;
            }
            start = start.saturating_add(drop).min(len);
        }

        self.tty_output_start = start;
        self.tty_output.extend_from_slice(bytes);

        // Keep a safety net in case of inconsistent state (e.g. corrupted snapshots).
        if self.tty_output_start >= max {
            let start = self.tty_output_start.min(self.tty_output.len());
            self.tty_output.copy_within(start.., 0);
            self.tty_output.truncate(self.tty_output.len() - start);
            self.tty_output_start = 0;
        }
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
    use font8x8::{UnicodeFonts, BASIC_FONTS};

    fn boot_sector(pattern: u8) -> [u8; BIOS_SECTOR_SIZE] {
        let mut sector = [pattern; BIOS_SECTOR_SIZE];
        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    #[test]
    fn bios_config_vbe_lfb_base_overrides_default() {
        let default = Bios::new(BiosConfig::default());
        assert_eq!(
            default.video.vbe.lfb_base,
            crate::video::vbe::VbeDevice::LFB_BASE_DEFAULT
        );

        let base = 0xDEAD_BEEFu32;
        let bios = Bios::new(BiosConfig {
            vbe_lfb_base: Some(base),
            ..BiosConfig::default()
        });
        assert_eq!(bios.video.vbe.lfb_base, base);
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

        // Diskette Parameter Table (vector 0x1E).
        let dpt = DISKETTE_PARAM_TABLE_OFFSET as usize;
        assert_eq!(
            &rom_image[dpt..dpt + 11],
            &[0xAF, 0x02, 0x25, 0x02, 0x12, 0x1B, 0xFF, 0x6C, 0xF6, 0x0F, 0x08]
        );

        // Fixed Disk Parameter Table (vectors 0x41/0x46).
        let hdpt = FIXED_DISK_PARAM_TABLE_OFFSET as usize;
        assert_eq!(
            &rom_image[hdpt..hdpt + 16],
            &[
                0x00, 0x04, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00
            ]
        );

        // Video Parameter Table (vector 0x1D).
        let vpt = VIDEO_PARAM_TABLE_OFFSET as usize;
        assert_eq!(
            &rom_image[vpt..vpt + 16],
            &[
                0x5F, 0x4F, 0x50, 0x82, 0x55, 0x81, 0xBF, 0x1F, 0x00, 0x4F, 0x0D, 0x0E, 0x00, 0x00,
                0x00, 0x00
            ]
        );

        // Built-in 8x16 font table (INT 10h AH=11h AL=30h).
        //
        // Verify the bitmap for 'A' (0x41) is present and uses the expected 8x8-basic-derived
        // glyph scaled to 8x16 (each row duplicated).
        let a_glyph = VGA_FONT_8X16_OFFSET as usize + (0x41usize * 16);
        let glyph8 = BASIC_FONTS.get('A').unwrap_or([0u8; 8]);
        let mut expected = [0u8; 16];
        for (row, bits) in glyph8.iter().copied().enumerate() {
            expected[row * 2] = bits;
            expected[row * 2 + 1] = bits;
        }
        assert_eq!(&rom_image[a_glyph..a_glyph + 16], &expected);

        // 8x8 font table (IVT vector 0x1F).
        let a_glyph_8x8 = VGA_FONT_8X8_OFFSET as usize + (0x41usize * 8);
        assert_eq!(&rom_image[a_glyph_8x8..a_glyph_8x8 + 8], &glyph8);
    }

    #[test]
    fn post_initializes_ivt_vectors() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk, None);

        let read_vec = |mem: &mut TestMemory, v: u8| -> (u16, u16) {
            let addr = (v as u64) * 4;
            (mem.read_u16(addr), mem.read_u16(addr + 2))
        };

        assert_eq!(read_vec(&mut mem, 0x10), (INT10_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mut mem, 0x13), (INT13_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mut mem, 0x15), (INT15_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mut mem, 0x16), (INT16_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(read_vec(&mut mem, 0x1A), (INT1A_STUB_OFFSET, BIOS_SEGMENT));
        assert_eq!(
            read_vec(&mut mem, 0x1D),
            (VIDEO_PARAM_TABLE_OFFSET, BIOS_SEGMENT)
        );
        assert_eq!(
            read_vec(&mut mem, 0x1E),
            (DISKETTE_PARAM_TABLE_OFFSET, BIOS_SEGMENT)
        );
        assert_eq!(
            read_vec(&mut mem, 0x1F),
            (VGA_FONT_8X8_OFFSET, BIOS_SEGMENT)
        );
        assert_eq!(
            read_vec(&mut mem, 0x41),
            (FIXED_DISK_PARAM_TABLE_OFFSET, BIOS_SEGMENT)
        );
        assert_eq!(
            read_vec(&mut mem, 0x46),
            (FIXED_DISK_PARAM_TABLE_OFFSET, BIOS_SEGMENT)
        );
    }

    #[test]
    fn post_initializes_bda_and_ebda() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk, None);

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

        bios.post(&mut cpu, &mut mem, &mut disk, None);

        let loaded = mem.read_bytes(0x7C00, BIOS_SECTOR_SIZE);
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

        bios.post(&mut cpu, &mut mem, &mut disk, None);

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
    fn push_key_is_bounded_to_bda_keyboard_buffer_capacity() {
        let mut bios = Bios::new(BiosConfig::default());

        let extra = 5usize;
        for i in 0..(KEYBOARD_QUEUE_CAPACITY + extra) {
            bios.push_key(i as u16);
        }

        assert_eq!(bios.keyboard_queue.len(), KEYBOARD_QUEUE_CAPACITY);
        let retained: Vec<u16> = bios.keyboard_queue.iter().copied().collect();
        let expected: Vec<u16> = (extra..(KEYBOARD_QUEUE_CAPACITY + extra))
            .map(|i| i as u16)
            .collect();
        assert_eq!(retained, expected);
    }

    #[test]
    fn post_builds_acpi_rsdp_in_ebda() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk, None);

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

    fn parse_smbios_type1_uuid(mem: &mut TestMemory, eps_addr: u32) -> [u8; 16] {
        // SMBIOS 2.x EPS length is 0x1F bytes.
        let eps = mem.read_bytes(eps_addr as u64, 0x1F);
        assert_eq!(&eps[0..4], b"_SM_");

        let table_len = u16::from_le_bytes([eps[0x16], eps[0x17]]) as usize;
        let table_addr = u32::from_le_bytes([eps[0x18], eps[0x19], eps[0x1A], eps[0x1B]]) as u64;

        let table = mem.read_bytes(table_addr, table_len);

        let mut i = 0usize;
        while i < table.len() {
            if i + 1 >= table.len() {
                break;
            }
            let ty = table[i];
            let len = table[i + 1] as usize;
            assert!(len >= 4, "invalid SMBIOS structure length {len}");
            assert!(i + len <= table.len(), "truncated SMBIOS structure");

            if ty == 1 {
                let start = i + 8;
                let end = start + 16;
                assert!(end <= table.len(), "truncated SMBIOS Type 1 UUID field");
                return table[start..end].try_into().unwrap();
            }

            // Skip formatted + string-set (terminated by double NUL).
            let mut j = i + len;
            while j + 1 < table.len() {
                if table[j] == 0 && table[j + 1] == 0 {
                    j += 2;
                    break;
                }
                j += 1;
            }
            i = j;

            if ty == 127 {
                break;
            }
        }

        panic!("SMBIOS Type 1 structure missing");
    }

    fn smbios_uuid_for_seed(uuid_seed: u64) -> [u8; 16] {
        let config = BiosConfig {
            smbios_uuid_seed: uuid_seed,
            ..BiosConfig::default()
        };
        let mut bios = Bios::new(config.clone());
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(config.memory_size_bytes);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk, None);

        let eps_addr = bios
            .smbios_eps_addr()
            .expect("SMBIOS EPS should be built during POST");
        parse_smbios_type1_uuid(&mut mem, eps_addr)
    }

    #[test]
    fn smbios_type1_uuid_default_seed_is_stable() {
        assert_eq!(
            smbios_uuid_for_seed(0),
            [
                0x5c, 0xca, 0x68, 0x4d, 0xc9, 0x6c, 0x2b, 0x4a, 0xa9, 0x9d, 0x58, 0x2e, 0x63, 0xb0,
                0xe0, 0x17
            ]
        );
    }

    #[test]
    fn smbios_type1_uuid_seed_is_configurable_and_deterministic() {
        let uuid0 = smbios_uuid_for_seed(0);

        let uuid1 = smbios_uuid_for_seed(1);
        assert_eq!(
            uuid1,
            [
                0xef, 0x9b, 0xef, 0x09, 0xc9, 0xd4, 0x26, 0x4a, 0xa8, 0x5c, 0x04, 0xad, 0xa0, 0x2c,
                0xec, 0x36
            ]
        );
        assert_ne!(uuid0, uuid1);

        // Deterministic: same seed yields the same bytes across runs/instances.
        assert_eq!(smbios_uuid_for_seed(1), uuid1);
    }
    #[test]
    fn post_reports_acpi_build_failure_to_tty_output() {
        // Force ACPI placement to be out-of-bounds by advertising too little guest RAM for the
        // default `AcpiPlacement` (tables start at 1MiB).
        let mut bios = Bios::new(BiosConfig {
            memory_size_bytes: 0x0010_0000, // 1MiB
            boot_drive: 0x80,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(aero_cpu_core::state::CpuMode::Real);
        let mut mem = TestMemory::new(0x0010_0000);
        let mut disk = InMemoryDisk::from_boot_sector(boot_sector(0));

        bios.post(&mut cpu, &mut mem, &mut disk, None);

        assert!(bios.rsdp_addr().is_none(), "ACPI should not be built");
        let tty = String::from_utf8_lossy(bios.tty_output());
        assert!(
            tty.contains("ACPI build failed"),
            "expected BIOS TTY output to contain an ACPI failure message, got: {tty:?}"
        );
    }
}
