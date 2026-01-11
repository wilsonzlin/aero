use core::cmp::min;
use core::mem::size_of;
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables, PhysicalMemory as AcpiPhysicalMemory};
use aero_devices::pci::{PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};

use crate::types::{
    E820Entry, RealModeCpu, E820_TYPE_ACPI, E820_TYPE_NVS, E820_TYPE_RAM, E820_TYPE_RESERVED,
    FLAG_CF, FLAG_IF, FLAG_ZF,
};
use firmware_tables::smbios::{SmbiosConfig, SmbiosTables};

const BDA_BASE: u32 = 0x0400;
const BDA_VIDEO_MODE_ADDR: u32 = BDA_BASE + 0x49;
const BDA_TEXT_COLUMNS_ADDR: u32 = BDA_BASE + 0x4A;
const BDA_VIDEO_PAGE_SIZE_ADDR: u32 = BDA_BASE + 0x4C;
const BDA_VIDEO_PAGE_OFFSET_ADDR: u32 = BDA_BASE + 0x4E;
const BDA_CURSOR_POS_ADDR: u32 = BDA_BASE + 0x50;
const BDA_CURSOR_SHAPE_ADDR: u32 = BDA_BASE + 0x60;
const BDA_ACTIVE_PAGE_ADDR: u32 = BDA_BASE + 0x62;
const BDA_ROWS_MINUS_ONE_ADDR: u32 = BDA_BASE + 0x84;

const VGA_TEXT_BASE: u32 = 0x000B_8000;
const VGA_MODE13_BASE: u32 = 0x000A_0000;

const MODE13_WIDTH: u32 = 320;
const MODE13_HEIGHT: u32 = 200;
const MODE13_BYTES_PER_PAGE: u32 = MODE13_WIDTH * MODE13_HEIGHT;

const DEFAULT_TEXT_ATTR: u8 = 0x07;
const DEFAULT_CURSOR_START: u8 = 0x06;
const DEFAULT_CURSOR_END: u8 = 0x07;

/// A device the BIOS can boot from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootDevice {
    /// First hard disk (DL = 0x80).
    Hdd0,
}

impl BootDevice {
    pub fn bios_drive_number(self) -> u8 {
        match self {
            BootDevice::Hdd0 => 0x80,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BiosConfig {
    /// Total guest RAM size in bytes.
    pub total_memory_bytes: u64,
    /// Default boot device.
    pub boot_device: BootDevice,
    /// Number of CPUs exposed via ACPI tables.
    pub cpu_count: u8,
    /// Whether to build and publish minimal ACPI tables in guest RAM.
    pub enable_acpi: bool,
    /// Base address for ACPI SDT blobs (DSDT/FADT/MADT/HPET/RSDT/XSDT).
    pub acpi_tables_base: u64,
    /// Base address for the ACPI NVS window (E820 type 4). Used for firmware
    /// state such as the FACS (Firmware ACPI Control Structure).
    pub acpi_nvs_base: u64,
    /// Size of the ACPI NVS window in bytes.
    pub acpi_nvs_size: u64,
    /// Physical address where the ACPI RSDP will be written (< 1MiB).
    pub acpi_rsdp_addr: u64,
}

impl Default for BiosConfig {
    fn default() -> Self {
        Self {
            total_memory_bytes: 64 * 1024 * 1024,
            boot_device: BootDevice::Hdd0,
            cpu_count: 1,
            enable_acpi: true,
            acpi_tables_base: 0x0010_0000,
            acpi_nvs_base: 0x0011_0000,
            acpi_nvs_size: aero_acpi::DEFAULT_ACPI_NVS_SIZE,
            // Keep this in the standard BIOS search window but outside the system BIOS ROM.
            acpi_rsdp_addr: 0x000E_0000,
        }
    }
}

pub trait Memory {
    fn read_u8(&self, paddr: u32) -> u8;
    fn read_u16(&self, paddr: u32) -> u16;
    fn read_u32(&self, paddr: u32) -> u32;
    fn write_u8(&mut self, paddr: u32, v: u8);
    fn write_u16(&mut self, paddr: u32, v: u16);
    fn write_u32(&mut self, paddr: u32, v: u32);
    fn write_bytes(&mut self, paddr: u32, bytes: &[u8]) {
        for (i, b) in bytes.iter().copied().enumerate() {
            self.write_u8(paddr + i as u32, b);
        }
    }
}

/// Chipset A20 gate controller.
///
/// Real x86 systems expose the A20 line through multiple mechanisms:
/// - i8042 controller output port
/// - "Fast A20" latch at I/O port `0x92`
/// - BIOS INT 15h services (`AX=2400h..2403h`)
///
/// Aero's BIOS INT 15h implementation uses this trait so the emulator can wire
/// the firmware-visible A20 services to the same underlying A20 line used by
/// the platform memory bus for address masking.
pub trait A20Gate {
    fn a20_enabled(&self) -> bool;
    fn set_a20_enabled(&mut self, enabled: bool);
}

struct LocalA20Gate {
    enabled: bool,
}

impl A20Gate for LocalA20Gate {
    fn a20_enabled(&self) -> bool {
        self.enabled
    }

    fn set_a20_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

pub trait BlockDevice {
    fn read_sector(&mut self, lba: u64, buf512: &mut [u8; 512]) -> Result<(), DiskError>;

    fn write_sector(&mut self, lba: u64, buf512: &[u8; 512]) -> Result<(), DiskError>;

    fn sector_count(&self) -> u64;

    fn is_read_only(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskError {
    OutOfRange,
    IoError,
    InvalidPacket,
    ReadOnly,
}

pub trait Keyboard {
    /// Pop the next key if available.
    ///
    /// Returned as `(scan_code << 8) | ascii`.
    fn pop_key(&mut self) -> Option<u16>;

    fn peek_key(&mut self) -> Option<u16>;
}

#[derive(Default)]
pub struct NullKeyboard;

impl Keyboard for NullKeyboard {
    fn pop_key(&mut self) -> Option<u16> {
        None
    }
    fn peek_key(&mut self) -> Option<u16> {
        None
    }
}

/// VESA BIOS Extensions (VBE) services hooked via INT 10h AX=4Fxx.
///
/// The BIOS core only provides dispatch glue; implementations are expected to follow the
/// VBE convention of returning `AL=0x4F` and `AH=status` with CF clear on success.
pub trait VbeServices {
    fn handle_int10(&mut self, cpu: &mut RealModeCpu, mem: &mut dyn Memory);
}

pub struct NoVbe;

impl VbeServices for NoVbe {
    fn handle_int10(&mut self, cpu: &mut RealModeCpu, _mem: &mut dyn Memory) {
        cpu.set_ax(0x024F);
        cpu.set_cf(true);
    }
}

pub trait PciConfigSpace {
    /// Read a 32-bit PCI config register.
    ///
    /// `offset` must be 4-byte aligned.
    fn read_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8) -> u32;

    /// Write a 32-bit PCI config register.
    ///
    /// `offset` must be 4-byte aligned.
    fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u32, // 24-bit: class/subclass/prog-if
    pub irq_line: u8,
}

/// A minimal legacy BIOS implementation.
pub struct Bios {
    cfg: BiosConfig,
    e820: Vec<E820Entry>,
    pci_devices: Vec<PciDevice>,
    acpi: Option<AcpiTables>,
    a20_gate: Box<dyn A20Gate>,
    last_disk_status: u8,

    // Legacy VGA BIOS-visible state.
    video_mode: u8,
    active_page: u8,
    text_cols: u16,
    text_rows: u8,
    cursor_pos: [(u8, u8); 8],
    cursor_start: u8,
    cursor_end: u8,
    text_attr: u8,
    vbe: Box<dyn VbeServices>,

    // Keyboard buffer (BIOS-side, independent of i8042 model for now).
    kb_buf: VecDeque<u16>,
}

impl Bios {
    pub fn new(cfg: BiosConfig) -> Self {
        let acpi = if cfg.enable_acpi {
            let mut acpi_cfg = AcpiConfig::default();
            acpi_cfg.cpu_count = cfg.cpu_count.max(1);
            let placement = AcpiPlacement {
                tables_base: cfg.acpi_tables_base,
                nvs_base: cfg.acpi_nvs_base,
                nvs_size: cfg.acpi_nvs_size,
                rsdp_addr: cfg.acpi_rsdp_addr,
                alignment: aero_acpi::DEFAULT_ACPI_ALIGNMENT,
            };

            let tables = AcpiTables::build(&acpi_cfg, placement);
            let (tables_base, tables_len) = acpi_reclaimable_region_from_tables(&tables);
            let tables_end = tables_base.saturating_add(tables_len);
            let rsdp_end = tables
                .addresses
                .rsdp
                .saturating_add(tables.rsdp.len() as u64);
            let nvs_end = cfg.acpi_nvs_base.saturating_add(cfg.acpi_nvs_size);

            if tables_end <= cfg.total_memory_bytes
                && rsdp_end <= cfg.total_memory_bytes
                && nvs_end <= cfg.total_memory_bytes
            {
                Some(tables)
            } else {
                None
            }
        } else {
            None
        };

        let acpi_reclaimable = acpi.as_ref().map(acpi_reclaimable_region_from_tables);
        let acpi_nvs = acpi
            .as_ref()
            .map(|_| (cfg.acpi_nvs_base, cfg.acpi_nvs_size));
        let e820 = build_e820_map(
            cfg.total_memory_bytes,
            acpi_reclaimable,
            acpi_nvs,
        );
        Self {
            cfg,
            e820,
            pci_devices: Vec::new(),
            acpi,
            a20_gate: Box::new(LocalA20Gate { enabled: true }),
            last_disk_status: 0,
            video_mode: 0x03,
            active_page: 0,
            text_cols: 80,
            text_rows: 25,
            cursor_pos: [(0, 0); 8],
            cursor_start: DEFAULT_CURSOR_START,
            cursor_end: DEFAULT_CURSOR_END,
            text_attr: DEFAULT_TEXT_ATTR,
            vbe: Box::new(NoVbe),
            kb_buf: VecDeque::new(),
        }
    }

    pub fn set_vbe_handler(&mut self, handler: Box<dyn VbeServices>) {
        self.vbe = handler;
    }

    /// Override the BIOS-visible A20 gate controller.
    ///
    /// When unset, the BIOS maintains its own internal A20 latch that only
    /// affects INT 15h query results. Emulators should provide a real
    /// implementation so INT 15h A20 services toggle the same A20 line that the
    /// physical address bus uses for masking.
    pub fn set_a20_gate(&mut self, gate: Box<dyn A20Gate>) {
        self.a20_gate = gate;
    }

    /// Perform a simplified POST and transfer control to the boot sector.
    pub fn post<M: Memory, D: BlockDevice>(
        &mut self,
        cpu: &mut RealModeCpu,
        mem: &mut M,
        disk: &mut D,
    ) {
        let mut null_kbd = NullKeyboard;
        self.post_with_devices(cpu, mem, disk, &mut null_kbd, None);
    }

    /// POST with optional devices attached (keyboard + PCI config space).
    pub fn post_with_devices<M: Memory, D: BlockDevice, K: Keyboard>(
        &mut self,
        cpu: &mut RealModeCpu,
        mem: &mut M,
        disk: &mut D,
        _kbd: &mut K,
        mut pci: Option<&mut dyn PciConfigSpace>,
    ) {
        // Disable interrupts.
        cpu.eflags &= !FLAG_IF;

        self.init_ivt(mem);
        self.init_bda(mem);
        self.init_smbios(mem);

        if let Some(pci) = pci.as_deref_mut() {
            self.enumerate_pci(pci);
        }

        self.write_acpi_tables(mem);

        self.init_video(mem);

        // Basic banner for debugging / integration tests.
        self.print_str(mem, "Aero BIOS\r\n");

        // Load and jump to the boot sector (INT 19h path).
        self.int19(cpu, mem, disk);

        // Enable interrupts for the bootloader.
        cpu.eflags |= FLAG_IF;
    }

    pub fn pci_devices(&self) -> &[PciDevice] {
        &self.pci_devices
    }

    pub fn acpi_tables(&self) -> Option<&AcpiTables> {
        self.acpi.as_ref()
    }

    /// Handle an x86 `INT n` instruction (emulator "VM exit").
    pub fn handle_interrupt<M: Memory, D: BlockDevice, K: Keyboard>(
        &mut self,
        int_no: u8,
        cpu: &mut RealModeCpu,
        mem: &mut M,
        disk: &mut D,
        kbd: &mut K,
    ) {
        match int_no {
            0x10 => self.int10(cpu, mem),
            0x11 => self.int11(cpu, mem),
            0x12 => self.int12(cpu, mem),
            0x13 => self.int13(cpu, mem, disk),
            0x15 => self.int15(cpu, mem),
            0x16 => self.int16(cpu, kbd),
            0x19 => self.int19(cpu, mem, disk),
            0x1A => self.int1a(cpu, mem),
            _ => {
                // Unhandled: set CF and AH=0x01 (invalid function) when plausible.
                cpu.eflags |= FLAG_CF;
                cpu.set_ah(0x01);
            }
        }
    }

    fn init_ivt<M: Memory>(&self, mem: &mut M) {
        // Initialize IVT with a default handler pointer (F000:0000).
        // In Aero, interrupts are typically trapped by the emulator; the IVT is
        // still initialized so software that *reads* the IVT sees consistent data.
        for int_no in 0u32..256 {
            let vec = int_no * 4;
            mem.write_u16(vec, 0x0000);
            mem.write_u16(vec + 2, 0xF000);
        }
    }

    fn write_acpi_tables<M: Memory>(&self, mem: &mut M) {
        let Some(tables) = &self.acpi else {
            return;
        };

        struct Writer<'a, M>(&'a mut M);

        impl<M: Memory> AcpiPhysicalMemory for Writer<'_, M> {
            fn write(&mut self, paddr: u64, bytes: &[u8]) {
                let paddr_u32: u32 = paddr
                    .try_into()
                    .expect("ACPI table placement must be below 4GiB");
                self.0.write_bytes(paddr_u32, bytes);
            }
        }

        tables.write_to(&mut Writer(mem));
    }

    fn enumerate_pci<P: PciConfigSpace + ?Sized>(&mut self, pci: &mut P) {
        self.pci_devices.clear();
        let router = PciIntxRouter::new(PciIntxRouterConfig::default());
        for bus in 0u8..=0xFF {
            for device in 0u8..32 {
                for function in 0u8..8 {
                    let id = pci.read_config_dword(bus, device, function, 0x00);
                    let vendor_id = (id & 0xFFFF) as u16;
                    if vendor_id == 0xFFFF {
                        if function == 0 {
                            break;
                        }
                        continue;
                    }
                    let device_id = (id >> 16) as u16;
                    let class_reg = pci.read_config_dword(bus, device, function, 0x08);
                    let class_code = (class_reg >> 8) & 0x00FF_FFFF;
                    let reg_3c = pci.read_config_dword(bus, device, function, 0x3C);
                    let interrupt_pin = ((reg_3c >> 8) & 0xFF) as u8; // 1=INTA#, 2=INTB#, ...
                    let irq_line = assign_pci_irq(
                        &router,
                        PciBdf::new(bus, device, function),
                        interrupt_pin,
                    );

                    // Program Interrupt Line register (0x3C, low byte).
                    let new_3c = (reg_3c & 0xFFFF_FF00) | irq_line as u32;
                    pci.write_config_dword(bus, device, function, 0x3C, new_3c);

                    self.pci_devices.push(PciDevice {
                        bus,
                        device,
                        function,
                        vendor_id,
                        device_id,
                        class_code,
                        irq_line,
                    });

                    if function == 0 {
                        let header = pci.read_config_dword(bus, device, function, 0x0C);
                        let header_type = ((header >> 16) & 0xFF) as u8;
                        let is_multifunction = (header_type & 0x80) != 0;
                        if !is_multifunction {
                            break;
                        }
                    }
                }
            }
        }
    }

    fn init_bda<M: Memory>(&self, mem: &mut M) {
        // BIOS Data Area starts at 0x0400.
        // We only populate fields that common DOS/boot code uses.
        let bda = 0x0400u32;

        // Equipment word at 0x0410: we claim VGA, no floppy, 1 disk.
        mem.write_u16(bda + 0x10, 0x0021);

        // Conventional memory size in KiB at 0x0413.
        mem.write_u16(bda + 0x13, 640);

        // EBDA segment at 0x040E. We place EBDA at 0x9FC00 (typical).
        mem.write_u16(bda + 0x0E, 0x9FC0);

        // Keyboard flags at 0x0417.
        mem.write_u8(bda + 0x17, 0);
        mem.write_u8(bda + 0x18, 0);

        // Keyboard buffer head/tail pointers at 0x041A/0x041C.
        mem.write_u16(bda + 0x1A, 0x001E);
        mem.write_u16(bda + 0x1C, 0x001E);

        // Video mode (0x0449) and columns (0x044A).
        mem.write_u8(bda + 0x49, 0x03);
        mem.write_u16(bda + 0x4A, 80);

        // BIOS tick counter (0x046C) and midnight flag (0x0470).
        let ticks = ticks_since_midnight();
        mem.write_u32(bda + 0x6C, ticks);
        mem.write_u8(bda + 0x70, 0);
    }

    fn init_smbios<M: Memory>(&self, mem: &mut M) {
        struct FirmwareBus<'a, M>(&'a mut M);

        impl<M: Memory> firmware_tables::memory::MemoryBus for FirmwareBus<'_, M> {
            fn read_u8(&self, addr: u64) -> u8 {
                let paddr = u32::try_from(addr).expect("SMBIOS address must fit in u32");
                self.0.read_u8(paddr)
            }

            fn write_u8(&mut self, addr: u64, value: u8) {
                let paddr = u32::try_from(addr).expect("SMBIOS address must fit in u32");
                self.0.write_u8(paddr, value);
            }
        }

        let config = SmbiosConfig {
            ram_bytes: self.cfg.total_memory_bytes,
            cpu_count: 1,
            uuid_seed: 0,
            eps_addr: None,
            table_addr: None,
        };

        let mut bus = FirmwareBus(mem);
        let _ = SmbiosTables::build_and_write(&config, &mut bus);
    }

    fn init_video<M: Memory>(&mut self, mem: &mut M) {
        self.set_video_mode(mem, 0x03, true);
    }

    fn text_page_size_bytes(&self) -> u16 {
        self.text_cols
            .saturating_mul(self.text_rows as u16)
            .saturating_mul(2)
    }

    fn video_page_size_bytes(&self) -> u16 {
        match self.video_mode {
            0x13 => MODE13_BYTES_PER_PAGE as u16,
            _ => self.text_page_size_bytes(),
        }
    }

    fn sync_bda_video_state<M: Memory>(&self, mem: &mut M) {
        mem.write_u8(BDA_VIDEO_MODE_ADDR, self.video_mode);
        mem.write_u16(BDA_TEXT_COLUMNS_ADDR, self.text_cols);
        mem.write_u16(BDA_VIDEO_PAGE_SIZE_ADDR, self.video_page_size_bytes());
        mem.write_u16(
            BDA_VIDEO_PAGE_OFFSET_ADDR,
            self.video_page_size_bytes()
                .wrapping_mul(self.active_page as u16),
        );
        mem.write_u8(BDA_ACTIVE_PAGE_ADDR, self.active_page);
        mem.write_u16(
            BDA_CURSOR_SHAPE_ADDR,
            ((self.cursor_start as u16) << 8) | self.cursor_end as u16,
        );
        mem.write_u8(BDA_ROWS_MINUS_ONE_ADDR, self.text_rows.saturating_sub(1));

        for page in 0u32..8 {
            let (row, col) = self.cursor_pos[page as usize];
            let word = ((row as u16) << 8) | col as u16;
            mem.write_u16(BDA_CURSOR_POS_ADDR + page * 2, word);
        }
    }

    fn sync_bda_cursor<M: Memory>(&self, mem: &mut M, page: u8) {
        let page = (page & 0x07) as u32;
        let (row, col) = self.cursor_pos[page as usize];
        mem.write_u16(
            BDA_CURSOR_POS_ADDR + page * 2,
            ((row as u16) << 8) | col as u16,
        );
    }

    fn set_video_mode<M: Memory>(&mut self, mem: &mut M, mode: u8, clear: bool) -> bool {
        match mode {
            0x03 => {
                self.video_mode = 0x03;
                self.active_page = 0;
                self.text_cols = 80;
                self.text_rows = 25;
                self.cursor_pos = [(0, 0); 8];
                self.cursor_start = DEFAULT_CURSOR_START;
                self.cursor_end = DEFAULT_CURSOR_END;
                self.text_attr = DEFAULT_TEXT_ATTR;

                self.sync_bda_video_state(mem);
                if clear {
                    self.clear_text_page(mem, 0, self.text_attr);
                }
                true
            }
            0x13 => {
                self.video_mode = 0x13;
                self.active_page = 0;
                // Conventional BIOS values for mode 13h: 40 columns, 25 rows.
                self.text_cols = 40;
                self.text_rows = 25;
                self.cursor_pos = [(0, 0); 8];
                self.cursor_start = DEFAULT_CURSOR_START;
                self.cursor_end = DEFAULT_CURSOR_END;
                self.text_attr = DEFAULT_TEXT_ATTR;

                self.sync_bda_video_state(mem);
                if clear {
                    self.clear_mode13h(mem);
                }
                true
            }
            _ => false,
        }
    }

    fn clear_text_page<M: Memory>(&self, mem: &mut M, page: u8, attr: u8) {
        let cols = self.text_cols as u32;
        let rows = self.text_rows as u32;
        if cols == 0 || rows == 0 {
            return;
        }

        let base = self.text_page_base(page);
        let mut cell = base;
        for _ in 0..(cols * rows) {
            mem.write_u8(cell, b' ');
            mem.write_u8(cell + 1, attr);
            cell += 2;
        }
    }

    fn clear_mode13h<M: Memory>(&self, mem: &mut M) {
        for off in 0..MODE13_BYTES_PER_PAGE {
            mem.write_u8(VGA_MODE13_BASE + off, 0);
        }
    }

    fn text_page_base(&self, page: u8) -> u32 {
        VGA_TEXT_BASE + (page as u32 & 0x07) * (self.text_page_size_bytes() as u32)
    }

    fn write_text_cell<M: Memory>(
        &self,
        mem: &mut M,
        page: u8,
        row: u8,
        col: u8,
        ch: u8,
        attr: u8,
    ) {
        if row >= self.text_rows {
            return;
        }
        let cols = self.text_cols as u8;
        if cols == 0 || col >= cols {
            return;
        }
        let idx = row as u32 * self.text_cols as u32 + col as u32;
        let addr = self.text_page_base(page) + idx * 2;
        mem.write_u8(addr, ch);
        mem.write_u8(addr + 1, attr);
    }

    fn read_text_cell_attr<M: Memory>(&self, mem: &M, page: u8, row: u8, col: u8) -> u8 {
        if row >= self.text_rows {
            return 0;
        }
        let cols = self.text_cols as u8;
        if cols == 0 || col >= cols {
            return 0;
        }
        let idx = row as u32 * self.text_cols as u32 + col as u32;
        let addr = self.text_page_base(page) + idx * 2;
        mem.read_u8(addr + 1)
    }

    fn scroll_text_window_up<M: Memory>(
        &self,
        mem: &mut M,
        page: u8,
        top: u8,
        left: u8,
        bottom: u8,
        right: u8,
        lines: u8,
        blank_attr: u8,
    ) {
        let cols = self.text_cols as u8;
        let rows = self.text_rows;
        if cols == 0 || rows == 0 {
            return;
        }

        let top = top.min(rows - 1);
        let bottom = bottom.min(rows - 1);
        let left = left.min(cols - 1);
        let right = right.min(cols - 1);
        if bottom < top || right < left {
            return;
        }

        let height = bottom - top + 1;
        let lines = if lines == 0 {
            height
        } else {
            lines.min(height)
        };

        for row in top..=bottom {
            for col in left..=right {
                let src_row = row.saturating_add(lines);
                if src_row <= bottom {
                    let idx_src = src_row as u32 * self.text_cols as u32 + col as u32;
                    let addr_src = self.text_page_base(page) + idx_src * 2;
                    let ch = mem.read_u8(addr_src);
                    let attr = mem.read_u8(addr_src + 1);
                    self.write_text_cell(mem, page, row, col, ch, attr);
                } else {
                    self.write_text_cell(mem, page, row, col, b' ', blank_attr);
                }
            }
        }
    }

    fn tty_put_char<M: Memory>(&mut self, mem: &mut M, page: u8, ch: u8, attr: u8) {
        let page = page & 0x07;
        let cols = self.text_cols as u8;
        let rows = self.text_rows;
        if cols == 0 || rows == 0 {
            return;
        }

        let (mut row, mut col) = self.cursor_pos[page as usize];

        match ch {
            0x07 => {}
            0x08 => {
                if col > 0 {
                    col -= 1;
                } else if row > 0 {
                    row -= 1;
                    col = cols.saturating_sub(1);
                }
            }
            b'\r' => col = 0,
            b'\n' => row = row.saturating_add(1),
            ch => {
                self.write_text_cell(mem, page, row, col, ch, attr);
                col = col.saturating_add(1);
                if col >= cols {
                    col = 0;
                    row = row.saturating_add(1);
                }
            }
        }

        if row >= rows {
            self.scroll_text_window_up(mem, page, 0, 0, rows - 1, cols - 1, 1, attr);
            row = rows - 1;
        }

        self.cursor_pos[page as usize] = (row, col);
        self.sync_bda_cursor(mem, page);
    }

    fn print_str<M: Memory>(&mut self, mem: &mut M, s: &str) {
        for b in s.as_bytes().iter().copied() {
            self.tty_put_char(mem, self.active_page, b, self.text_attr);
        }
    }

    fn int10<M: Memory>(&mut self, cpu: &mut RealModeCpu, mem: &mut M) {
        if cpu.ah() == 0x4F {
            self.vbe.handle_int10(cpu, mem);
            return;
        }

        match cpu.ah() {
            0x00 => {
                // Set video mode (AL). Bit 7: don't clear screen.
                let al = cpu.al();
                let mode = al & 0x7F;
                let clear = (al & 0x80) == 0;
                if self.set_video_mode(mem, mode, clear) {
                    cpu.set_cf(false);
                } else {
                    cpu.set_cf(true);
                    cpu.set_ah(0x01);
                }
            }
            0x01 => {
                // Set cursor shape.
                self.cursor_start = cpu.ch();
                self.cursor_end = cpu.cl();
                mem.write_u16(
                    BDA_CURSOR_SHAPE_ADDR,
                    ((self.cursor_start as u16) << 8) | self.cursor_end as u16,
                );
                cpu.set_cf(false);
            }
            0x02 => {
                // Set cursor position: BH=page, DH=row, DL=col.
                let page = cpu.bh() & 0x07;
                let row = cpu.dh().min(self.text_rows.saturating_sub(1));
                let col = cpu.dl().min((self.text_cols.saturating_sub(1)) as u8);
                self.cursor_pos[page as usize] = (row, col);
                self.sync_bda_cursor(mem, page);
                cpu.set_cf(false);
            }
            0x03 => {
                // Get cursor position and shape: BH=page, DH/DL=row/col, CH/CL=shape.
                let page = cpu.bh() & 0x07;
                let (row, col) = self.cursor_pos[page as usize];
                cpu.set_dh(row);
                cpu.set_dl(col);
                cpu.set_ch(self.cursor_start);
                cpu.set_cl(self.cursor_end);
                cpu.set_cf(false);
            }
            0x06 => {
                // Scroll up window.
                let lines = cpu.al();
                let blank_attr = cpu.bh();
                let top = cpu.ch();
                let left = cpu.cl();
                let bottom = cpu.dh();
                let right = cpu.dl();

                let page = self.active_page;
                self.scroll_text_window_up(mem, page, top, left, bottom, right, lines, blank_attr);
                cpu.set_cf(false);
            }
            0x09 => {
                // Write character and attribute at cursor (repeat).
                let page = cpu.bh() & 0x07;
                let ch = cpu.al();
                let attr = cpu.bl();
                let count = cpu.cx();
                if count == 0 {
                    cpu.set_cf(false);
                    return;
                }

                let cols = self.text_cols as u32;
                let rows = self.text_rows as u32;
                let (row0, col0) = self.cursor_pos[page as usize];
                let mut linear = row0 as u32 * cols + col0 as u32;
                let max = rows * cols;

                for _ in 0..count {
                    if linear >= max {
                        break;
                    }
                    let row = (linear / cols) as u8;
                    let col = (linear % cols) as u8;
                    self.write_text_cell(mem, page, row, col, ch, attr);
                    linear += 1;
                }
                cpu.set_cf(false);
            }
            0x0A => {
                // Write character only at cursor (repeat), preserving attribute.
                let page = cpu.bh() & 0x07;
                let ch = cpu.al();
                let count = cpu.cx();
                if count == 0 {
                    cpu.set_cf(false);
                    return;
                }

                let cols = self.text_cols as u32;
                let rows = self.text_rows as u32;
                let (row0, col0) = self.cursor_pos[page as usize];
                let mut linear = row0 as u32 * cols + col0 as u32;
                let max = rows * cols;

                for _ in 0..count {
                    if linear >= max {
                        break;
                    }
                    let row = (linear / cols) as u8;
                    let col = (linear % cols) as u8;
                    let attr = self.read_text_cell_attr(mem, page, row, col);
                    self.write_text_cell(mem, page, row, col, ch, attr);
                    linear += 1;
                }
                cpu.set_cf(false);
            }
            0x0E => {
                // Teletype output: AL=char, BH=page, BL=attribute (non-zero).
                let page = cpu.bh() & 0x07;
                let ch = cpu.al();
                let attr = match cpu.bl() {
                    0 => self.text_attr,
                    v => v,
                };
                self.tty_put_char(mem, page, ch, attr);
                cpu.set_cf(false);
            }
            0x0F => {
                // Get current video mode. Return: AH=columns, AL=mode, BH=active page.
                cpu.set_al(self.video_mode);
                cpu.set_ah(self.text_cols as u8);
                cpu.set_bh(self.active_page);
                cpu.set_cf(false);
            }
            0x13 => {
                // Write string.
                // AL=write mode, BH=page, BL=attr, CX=len, DH=row, DL=col, ES:BP=ptr
                let write_mode = cpu.al();
                let page = cpu.bh() & 0x07;
                let attr = cpu.bl();
                let len = cpu.cx() as usize;
                let mut row = cpu.dh();
                let mut col = cpu.dl();

                let cols = self.text_cols as u8;
                let rows = self.text_rows;
                let mut addr = cpu.es_base() + cpu.bp() as u32;

                for _ in 0..len {
                    if row >= rows {
                        break;
                    }
                    let ch = mem.read_u8(addr);
                    addr = addr.wrapping_add(1);

                    let cell_attr = if (write_mode & 0x02) != 0 {
                        let a = mem.read_u8(addr);
                        addr = addr.wrapping_add(1);
                        a
                    } else {
                        attr
                    };

                    self.write_text_cell(mem, page, row, col, ch, cell_attr);
                    col = col.saturating_add(1);
                    if col >= cols {
                        col = 0;
                        row = row.saturating_add(1);
                    }
                }

                if (write_mode & 0x01) != 0 {
                    self.cursor_pos[page as usize] = (row, col);
                    self.sync_bda_cursor(mem, page);
                }

                cpu.set_cf(false);
            }
            _ => {
                cpu.set_cf(true);
                cpu.set_ah(0x01);
            }
        }
    }

    fn int11<M: Memory>(&mut self, cpu: &mut RealModeCpu, mem: &mut M) {
        // Get equipment list (returns AX).
        cpu.set_ax(mem.read_u16(0x0410));
        cpu.set_cf(false);
    }

    fn int12<M: Memory>(&mut self, cpu: &mut RealModeCpu, mem: &mut M) {
        // Get conventional memory size in KiB (returns AX).
        cpu.set_ax(mem.read_u16(0x0413));
        cpu.set_cf(false);
    }

    fn int13<M: Memory, D: BlockDevice>(
        &mut self,
        cpu: &mut RealModeCpu,
        mem: &mut M,
        disk: &mut D,
    ) {
        let ah = cpu.ah();
        match ah {
            0x00 => {
                // Reset.
                self.last_disk_status = 0;
                cpu.set_cf(false);
                cpu.set_ah(0);
            }
            0x01 => {
                // Get status of last operation.
                cpu.set_ah(self.last_disk_status);
                cpu.set_cf(self.last_disk_status != 0);
            }
            0x02 => {
                // Read sectors (CHS).
                let count = cpu.al() as u16;
                let cx = cpu.cx();
                let cyl = ((cx >> 8) as u16) | (((cx & 0x00C0) as u16) << 2);
                let sector = (cx & 0x003F) as u16;
                let head = cpu.dh() as u16;

                let buffer = (cpu.es_base() + cpu.bx() as u32) as u32;
                match chs_read(disk, cyl, head, sector, count, mem, buffer) {
                    Ok(()) => {
                        self.last_disk_status = 0;
                        cpu.set_cf(false);
                        cpu.set_ah(0);
                    }
                    Err(status) => {
                        self.last_disk_status = status;
                        cpu.set_cf(true);
                        cpu.set_ah(status);
                    }
                }
            }
            0x03 => {
                // Write sectors (CHS).
                let count = cpu.al() as u16;
                let cx = cpu.cx();
                let cyl = ((cx >> 8) as u16) | (((cx & 0x00C0) as u16) << 2);
                let sector = (cx & 0x003F) as u16;
                let head = cpu.dh() as u16;

                let buffer = (cpu.es_base() + cpu.bx() as u32) as u32;
                match chs_write(disk, cyl, head, sector, count, mem, buffer) {
                    Ok(()) => {
                        self.last_disk_status = 0;
                        cpu.set_cf(false);
                        cpu.set_ah(0);
                    }
                    Err(status) => {
                        self.last_disk_status = status;
                        cpu.set_cf(true);
                        cpu.set_ah(status);
                    }
                }
            }
            0x08 => {
                // Get drive parameters (very rough geometry for compatibility).
                // We claim 16 heads, 63 sectors, 1024 cylinders (max for int13).
                let cylinders = 1024u16;
                let heads = 16u16;
                let spt = 63u16;

                self.last_disk_status = 0;
                cpu.set_cf(false);
                cpu.set_ah(0);

                // CH = low 8 bits of max cylinder, CL = sector count + high cyl bits in bits 6-7.
                let max_cyl = cylinders - 1;
                let ch = (max_cyl & 0xFF) as u8;
                let cl = (spt & 0x3F) as u8 | (((max_cyl >> 2) & 0xC0) as u8);
                cpu.set_cx(((ch as u16) << 8) | cl as u16);

                // DH = max head, DL = drive count (we claim 1).
                cpu.set_dh((heads - 1) as u8);
                cpu.set_dl(1);
            }
            0x15 => {
                // Get disk type.
                if cpu.dl() < 0x80 {
                    cpu.eax = 0; // no floppy
                } else {
                    cpu.eax = 0x0300; // hard disk
                }
                self.last_disk_status = 0;
                cpu.set_cf(false);
            }
            0x41 => {
                // Check extensions present (EDD).
                if cpu.bx() != 0x55AA {
                    self.last_disk_status = 0x01;
                    cpu.set_cf(true);
                    cpu.set_ah(0x01);
                    return;
                }
                self.last_disk_status = 0;
                cpu.set_cf(false);
                cpu.set_bx(0xAA55);
                // Report EDD 3.0 (AH=0x30).
                cpu.set_ah(0x30);
                cpu.set_al(0x00);
                // Support 42h (extended read) and 48h (get drive parameters).
                cpu.set_cx(0x0001 | 0x0004);
            }
            0x42 => {
                // Extended read.
                let pkt_addr = cpu.ds_base() + (cpu.esi as u32);
                match read_dap(mem, pkt_addr) {
                    Ok(dap) => {
                        let mut lba = dap.lba;
                        let mut buf = dap.buffer;
                        for _ in 0..dap.sectors {
                            let mut sector = [0u8; 512];
                            if disk.read_sector(lba, &mut sector).is_err() {
                                self.last_disk_status = 0x01;
                                cpu.set_cf(true);
                                cpu.set_ah(0x01);
                                return;
                            }
                            mem.write_bytes(buf, &sector);
                            lba += 1;
                            buf = buf.wrapping_add(512);
                        }
                        self.last_disk_status = 0;
                        cpu.set_cf(false);
                        cpu.set_ah(0);
                    }
                    Err(_) => {
                        self.last_disk_status = 0x01;
                        cpu.set_cf(true);
                        cpu.set_ah(0x01);
                    }
                }
            }
            0x43 => {
                // Extended write.
                if disk.is_read_only() {
                    self.last_disk_status = 0x03;
                    cpu.set_cf(true);
                    cpu.set_ah(0x03);
                    return;
                }

                let pkt_addr = cpu.ds_base() + (cpu.esi as u32);
                match read_dap(mem, pkt_addr) {
                    Ok(dap) => {
                        let mut lba = dap.lba;
                        let mut buf = dap.buffer;
                        for _ in 0..dap.sectors {
                            let mut sector = [0u8; 512];
                            for i in 0..512u32 {
                                sector[i as usize] = mem.read_u8(buf.wrapping_add(i));
                            }
                            if disk.write_sector(lba, &sector).is_err() {
                                self.last_disk_status = 0x03;
                                cpu.set_cf(true);
                                cpu.set_ah(0x03);
                                return;
                            }
                            lba += 1;
                            buf = buf.wrapping_add(512);
                        }
                        self.last_disk_status = 0;
                        cpu.set_cf(false);
                        cpu.set_ah(0);
                    }
                    Err(_) => {
                        self.last_disk_status = 0x01;
                        cpu.set_cf(true);
                        cpu.set_ah(0x01);
                    }
                }
            }
            0x48 => {
                // Get drive parameters (EDD).
                // Output a minimal EDD 3.0 parameter table.
                let table_addr = cpu.ds_base() + (cpu.esi as u32);
                write_drive_params(mem, table_addr, disk.sector_count());
                self.last_disk_status = 0;
                cpu.set_cf(false);
                cpu.set_ah(0);
            }
            _ => {
                self.last_disk_status = 0x01;
                cpu.set_cf(true);
                cpu.set_ah(0x01);
            }
        }
    }

    fn int15<M: Memory>(&mut self, cpu: &mut RealModeCpu, mem: &mut M) {
        // E820: EAX=0xE820, EDX='SMAP' (0x534D4150)
        if cpu.eax == 0xE820 && cpu.edx == 0x534D_4150 {
            let index = cpu.ebx as usize;
            if index >= self.e820.len() {
                cpu.set_cf(true);
                return;
            }

            let buf = cpu.es_base() + (cpu.edi as u32);
            write_e820_entry(mem, buf, self.e820[index]);

            cpu.eax = 0x534D_4150;
            cpu.ecx = size_of::<E820Entry>() as u32;
            cpu.ebx = if index + 1 >= self.e820.len() {
                0
            } else {
                (index + 1) as u32
            };
            cpu.set_cf(false);
            return;
        }

        match cpu.ax() {
            0x2400 => {
                // Disable A20 line.
                self.a20_gate.set_a20_enabled(false);
                cpu.set_cf(false);
                cpu.set_ah(0);
                return;
            }
            0x2401 => {
                // Enable A20 line.
                self.a20_gate.set_a20_enabled(true);
                cpu.set_cf(false);
                cpu.set_ah(0);
                return;
            }
            0x2402 => {
                // Query A20 state: AL=0 disabled, 1 enabled.
                cpu.set_al(if self.a20_gate.a20_enabled() { 1 } else { 0 });
                cpu.set_ah(0);
                cpu.set_cf(false);
                return;
            }
            0x2403 => {
                // Query A20 support. We claim keyboard-controller and fast-A20 support.
                cpu.set_bx(0x0003);
                cpu.set_cf(false);
                cpu.set_ah(0);
                return;
            }
            0xE801 => {
                // Get memory size for >64MiB systems (legacy).
                //
                // Return:
                // - AX = KB between 1MiB and 16MiB
                // - BX = number of 64KiB blocks above 16MiB
                // - CX, DX may mirror AX/BX (many BIOSes do)
                //
                // This function is not a replacement for E820 and is only expected to
                // describe memory below 4GiB. We still count ACPI/NVS regions since they
                // are backed by physical RAM, even if they are marked reserved in E820.
                const SIXTEEN_MIB: u64 = 16 * 1024 * 1024;
                const ONE_MIB: u64 = 1024 * 1024;
                const FOUR_GIB: u64 = 0x1_0000_0000;

                fn sum_e820_bytes(entries: &[E820Entry], start: u64, end: u64) -> u64 {
                    let mut total = 0u64;
                    for entry in entries {
                        if entry.length == 0
                            || !matches!(
                                entry.region_type,
                                E820_TYPE_RAM | E820_TYPE_ACPI | E820_TYPE_NVS
                            )
                        {
                            continue;
                        }

                        let entry_start = entry.base;
                        let entry_end = entry.base.saturating_add(entry.length);
                        let overlap_start = entry_start.max(start);
                        let overlap_end = entry_end.min(end);
                        if overlap_end > overlap_start {
                            total = total.saturating_add(overlap_end - overlap_start);
                        }
                    }
                    total
                }

                let bytes_1m_to_16m = sum_e820_bytes(&self.e820, ONE_MIB, SIXTEEN_MIB);
                let bytes_16m_to_4g = sum_e820_bytes(&self.e820, SIXTEEN_MIB, FOUR_GIB);

                // 1MiB..16MiB = 15MiB max = 15360KiB (0x3C00).
                let ax_kb = (bytes_1m_to_16m / 1024).min(0x3C00) as u16;
                let bx_blocks = (bytes_16m_to_4g / 65_536).min(0xFFFF) as u16;

                cpu.set_ax(ax_kb);
                cpu.set_bx(bx_blocks);
                cpu.set_cx(ax_kb);
                cpu.set_dx(bx_blocks);
                cpu.set_cf(false);
                return;
            }
            _ => {}
        }

        match cpu.ah() {
            0x88 => {
                // Get extended memory size in KB (up to 64MB-1MB).
                let kb = ((self.cfg.total_memory_bytes / 1024).saturating_sub(1024)) as u32;
                cpu.eax = min(kb, 0xFFFF) as u32;
                cpu.set_cf(false);
            }
            _ => {
                cpu.set_cf(true);
                cpu.set_ah(0x86); // function not supported
            }
        }
    }

    fn int16<K: Keyboard>(&mut self, cpu: &mut RealModeCpu, kbd: &mut K) {
        // Merge external keyboard model into BIOS buffer.
        while let Some(k) = kbd.pop_key() {
            self.kb_buf.push_back(k);
        }

        match cpu.ah() {
            0x00 => {
                // Read key (wait).
                if let Some(key) = self.kb_buf.pop_front() {
                    cpu.set_ax(key);
                    cpu.eflags &= !FLAG_ZF;
                    cpu.set_cf(false);
                } else {
                    // For now, "no key" is reported as ZF=1 and AX=0.
                    cpu.set_ax(0);
                    cpu.eflags |= FLAG_ZF;
                    cpu.set_cf(false);
                }
            }
            0x01 => {
                // Check key (non-blocking).
                if let Some(&key) = self.kb_buf.front() {
                    cpu.set_ax(key);
                    cpu.eflags &= !FLAG_ZF;
                } else {
                    cpu.eflags |= FLAG_ZF;
                }
                cpu.set_cf(false);
            }
            _ => {
                cpu.set_cf(true);
                cpu.set_ah(0x01);
            }
        }
    }

    fn int19<M: Memory, D: BlockDevice>(
        &mut self,
        cpu: &mut RealModeCpu,
        mem: &mut M,
        disk: &mut D,
    ) {
        // Load LBA0 (MBR / boot sector) to 0x0000:0x7C00.
        let mut sector = [0u8; 512];
        let res = disk.read_sector(0, &mut sector);
        if res.is_err() || sector[510] != 0x55 || sector[511] != 0xAA {
            self.print_str(mem, "No bootable device\r\n");
            // Halt.
            cpu.cs = 0xF000;
            cpu.set_ip(0xE000);
            return;
        }

        mem.write_bytes(0x0000_7C00, &sector);

        // Register state expected by many boot sectors.
        cpu.eax = 0;
        cpu.ebx = 0;
        cpu.ecx = 0;
        cpu.edx = self.cfg.boot_device.bios_drive_number() as u32;
        cpu.esi = 0;
        cpu.edi = 0;
        cpu.ebp = 0;
        cpu.esp = 0x7C00;

        cpu.cs = 0x0000;
        cpu.ds = 0x0000;
        cpu.es = 0x0000;
        cpu.ss = 0x0000;
        cpu.set_ip(0x7C00);
    }

    fn int1a<M: Memory>(&mut self, cpu: &mut RealModeCpu, mem: &mut M) {
        match cpu.ah() {
            0x00 => {
                // Get system time: CX:DX = ticks since midnight, AL = midnight flag.
                let ticks = ticks_since_midnight();
                cpu.set_cx((ticks >> 16) as u16);
                cpu.set_dx((ticks & 0xFFFF) as u16);
                cpu.set_al(0);

                // Mirror into BDA locations used by some software.
                let bda = 0x0400u32;
                mem.write_u32(bda + 0x6C, ticks);
                mem.write_u8(bda + 0x70, 0);

                cpu.set_cf(false);
            }
            0x02 => {
                // Read real-time clock time (we use host UTC time; returned values are BCD).
                let (h, m, s) = utc_hms();
                let ch = to_bcd(h);
                let cl = to_bcd(m);
                let dh = to_bcd(s);
                cpu.set_cx(((ch as u16) << 8) | cl as u16);
                cpu.set_dx(((dh as u16) << 8) | 0u16);
                cpu.set_cf(false);
            }
            0x04 => {
                // Read real-time clock date (host UTC date; returned values are BCD).
                let (year, month, day) = utc_ymd();
                let century = (year / 100) as u8;
                let year2 = (year % 100) as u8;
                let ch = to_bcd(century);
                let cl = to_bcd(year2);
                let dh = to_bcd(month);
                let dl = to_bcd(day);
                cpu.set_cx(((ch as u16) << 8) | cl as u16);
                cpu.set_dx(((dh as u16) << 8) | dl as u16);
                cpu.set_cf(false);
            }
            _ => {
                cpu.set_cf(true);
                cpu.set_ah(0x86);
            }
        }
    }
}

const TICKS_PER_DAY: u64 = 1_573_040;

fn ticks_since_midnight() -> u32 {
    let secs = unix_seconds();
    let secs_of_day = secs % 86_400;
    // Scale to BIOS ticks (18.2065Hz) using integer math.
    let ticks = (secs_of_day * TICKS_PER_DAY) / 86_400;
    ticks as u32
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn utc_hms() -> (u8, u8, u8) {
    let secs = unix_seconds();
    let secs_of_day = secs % 86_400;
    let h = (secs_of_day / 3600) as u8;
    let m = ((secs_of_day / 60) % 60) as u8;
    let s = (secs_of_day % 60) as u8;
    (h, m, s)
}

fn utc_ymd() -> (i32, u8, u8) {
    let days = (unix_seconds() / 86_400) as i64;
    civil_from_days(days)
}

fn to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

// Gregorian date conversion based on Howard Hinnant's "civil_from_days" algorithm.
// `days` is the number of days since 1970-01-01 (Unix epoch), in UTC.
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = mp + if mp < 10 { 3 } else { -9 }; // [1, 12]
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m as u8, d as u8)
}

fn assign_pci_irq(router: &PciIntxRouter, bdf: PciBdf, interrupt_pin: u8) -> u8 {
    // BIOS assigns config-space interrupt lines for the guest OS. Keep the policy in
    // lock-step with the device model (`aero_devices::pci::PciIntxRouter`) and ACPI
    // `_PRT` so Windows sees a consistent PCI INTx routing picture.
    let Some(pin) = PciInterruptPin::from_config_u8(interrupt_pin) else {
        return 0xFF;
    };
    let gsi = router.gsi_for_intx(bdf, pin);
    u8::try_from(gsi).unwrap_or(0xFF)
}

fn acpi_reclaimable_region_from_tables(tables: &AcpiTables) -> (u64, u64) {
    let addrs = &tables.addresses;
    let mut start = addrs.dsdt;
    start = start.min(addrs.fadt);
    start = start.min(addrs.madt);
    start = start.min(addrs.hpet);
    start = start.min(addrs.rsdt);
    start = start.min(addrs.xsdt);

    let mut end = start;
    end = end.max(addrs.dsdt.saturating_add(tables.dsdt.len() as u64));
    end = end.max(addrs.fadt.saturating_add(tables.fadt.len() as u64));
    end = end.max(addrs.madt.saturating_add(tables.madt.len() as u64));
    end = end.max(addrs.hpet.saturating_add(tables.hpet.len() as u64));
    end = end.max(addrs.rsdt.saturating_add(tables.rsdt.len() as u64));
    end = end.max(addrs.xsdt.saturating_add(tables.xsdt.len() as u64));

    (start, end.saturating_sub(start))
}

fn build_e820_map(
    total_memory_bytes: u64,
    acpi_region: Option<(u64, u64)>,
    nvs_region: Option<(u64, u64)>,
) -> Vec<E820Entry> {
    // Keep this conservative and OS-friendly:
    // - 0x00000000..0x0009F000 : usable conventional memory
    // - 0x0009F000..0x00100000 : reserved (EBDA + VGA + BIOS ROM)
    // - 0x00100000..(3GiB)     : usable, except optional ACPI table blob
    // - 0xC0000000..0x100000000: reserved PCI MMIO window ("PCI hole") if total RAM exceeds 3GiB
    // - 0x100000000..          : high memory, except optional ACPI table blob (rare)
    const ONE_MIB: u64 = 0x0010_0000;
    const PCI_HOLE_START: u64 = 0xC000_0000;
    const PCI_HOLE_END: u64 = 0x1_0000_0000;

    fn push_region(entries: &mut Vec<E820Entry>, base: u64, end: u64, region_type: u32) {
        if end <= base {
            return;
        }
        entries.push(E820Entry {
            base,
            length: end - base,
            region_type,
            extended_attributes: 1,
        });
    }

    fn push_ram_split_by_reserved(
        entries: &mut Vec<E820Entry>,
        base: u64,
        end: u64,
        reserved: &[(u64, u64, u32)],
    ) {
        if end <= base {
            return;
        }

        let mut cursor = base;
        for &(r_base, r_len, r_type) in reserved {
            let r_end = r_base.saturating_add(r_len);
            let a_start = r_base.clamp(base, end);
            let a_end = r_end.clamp(base, end);
            if a_end <= a_start {
                continue;
            }

            if a_start > cursor {
                push_region(entries, cursor, a_start, E820_TYPE_RAM);
            }
            push_region(entries, a_start, a_end, r_type);
            cursor = a_end;
        }

        if end > cursor {
            push_region(entries, cursor, end, E820_TYPE_RAM);
        }
    }

    let mut reserved = Vec::new();
    if let Some((base, len)) = acpi_region {
        reserved.push((base, len, E820_TYPE_ACPI));
    }
    if let Some((base, len)) = nvs_region {
        reserved.push((base, len, E820_TYPE_NVS));
    }
    reserved.sort_by_key(|(base, _, _)| *base);

    let mut entries = Vec::new();

    // Conventional memory.
    push_region(&mut entries, 0x0000_0000, 0x0009_F000, E820_TYPE_RAM);

    // EBDA/VGA/BIOS.
    push_region(&mut entries, 0x0009_F000, ONE_MIB, E820_TYPE_RESERVED);

    if total_memory_bytes <= ONE_MIB {
        return entries;
    }

    // Model a PCI hole once we exceed 3GiB of RAM. Remaining RAM is placed above 4GiB.
    let low_ram_end = total_memory_bytes.min(PCI_HOLE_START);
    push_ram_split_by_reserved(&mut entries, ONE_MIB, low_ram_end, &reserved);

    if total_memory_bytes > PCI_HOLE_START {
        push_region(
            &mut entries,
            PCI_HOLE_START,
            PCI_HOLE_END,
            E820_TYPE_RESERVED,
        );

        let high_ram_len = total_memory_bytes - PCI_HOLE_START;
        let high_ram_end = PCI_HOLE_END.saturating_add(high_ram_len);
        push_ram_split_by_reserved(&mut entries, PCI_HOLE_END, high_ram_end, &reserved);
    }

    entries
}

fn write_e820_entry<M: Memory>(mem: &mut M, paddr: u32, entry: E820Entry) {
    // Write as packed little-endian.
    let off = paddr;
    mem.write_u32(off, (entry.base & 0xFFFF_FFFF) as u32);
    mem.write_u32(off + 4, (entry.base >> 32) as u32);
    mem.write_u32(off + 8, (entry.length & 0xFFFF_FFFF) as u32);
    mem.write_u32(off + 12, (entry.length >> 32) as u32);
    mem.write_u32(off + 16, entry.region_type);
    mem.write_u32(off + 20, entry.extended_attributes);
}

fn chs_read<M: Memory, D: BlockDevice>(
    disk: &mut D,
    cylinder: u16,
    head: u16,
    sector1: u16,
    count: u16,
    mem: &mut M,
    mut buf: u32,
) -> Result<(), u8> {
    if sector1 == 0 {
        return Err(0x01);
    }

    // Conventional "translation" geometry.
    const HEADS: u64 = 16;
    const SPT: u64 = 63;

    let mut lba = ((cylinder as u64 * HEADS) + head as u64) * SPT + (sector1 as u64 - 1);
    for _ in 0..count {
        let mut sector = [0u8; 512];
        disk.read_sector(lba, &mut sector).map_err(|_| 0x01)?;
        mem.write_bytes(buf, &sector);
        lba += 1;
        buf = buf.wrapping_add(512);
    }
    Ok(())
}

fn chs_write<M: Memory, D: BlockDevice>(
    disk: &mut D,
    cylinder: u16,
    head: u16,
    sector1: u16,
    count: u16,
    mem: &M,
    mut buf: u32,
) -> Result<(), u8> {
    if sector1 == 0 {
        return Err(0x01);
    }

    if disk.is_read_only() {
        return Err(0x03);
    }

    // Conventional "translation" geometry.
    const HEADS: u64 = 16;
    const SPT: u64 = 63;

    let mut lba = ((cylinder as u64 * HEADS) + head as u64) * SPT + (sector1 as u64 - 1);
    for _ in 0..count {
        let mut sector = [0u8; 512];
        for i in 0..512u32 {
            sector[i as usize] = mem.read_u8(buf.wrapping_add(i));
        }
        disk.write_sector(lba, &sector).map_err(|_| 0x03)?;
        lba += 1;
        buf = buf.wrapping_add(512);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct DiskAddressPacket {
    sectors: u16,
    buffer: u32,
    lba: u64,
}

fn read_dap<M: Memory>(mem: &M, paddr: u32) -> Result<DiskAddressPacket, DiskError> {
    let size = mem.read_u8(paddr);
    if size < 0x10 {
        return Err(DiskError::InvalidPacket);
    }
    let sectors = mem.read_u16(paddr + 2);
    let buf_off = mem.read_u16(paddr + 4) as u32;
    let buf_seg = mem.read_u16(paddr + 6) as u32;
    let buffer = (buf_seg << 4) + buf_off;
    let lba_low = mem.read_u32(paddr + 8) as u64;
    let lba_high = mem.read_u32(paddr + 12) as u64;
    let lba = lba_low | (lba_high << 32);
    Ok(DiskAddressPacket {
        sectors,
        buffer,
        lba,
    })
}

fn write_drive_params<M: Memory>(mem: &mut M, paddr: u32, sector_count: u64) {
    // EDD v3.0 drive parameter table (subset).
    // See: BIOS Enhanced Disk Drive spec. We only fill the fields that common
    // bootloaders read (size + total sectors + bytes/sector).
    //
    // Offset  Size  Field
    // 0       2     size of table (0x1A)
    // 2       2     flags (0)
    // 4       4     cylinders (0x3FF)
    // 8       4     heads (0x10)
    // 12      4     sectors/track (0x3F)
    // 16      8     total sectors
    // 24      2     bytes/sector (512)
    mem.write_u16(paddr + 0, 0x1A);
    mem.write_u16(paddr + 2, 0);
    mem.write_u32(paddr + 4, 1024);
    mem.write_u32(paddr + 8, 16);
    mem.write_u32(paddr + 12, 63);
    mem.write_u32(paddr + 16, (sector_count & 0xFFFF_FFFF) as u32);
    mem.write_u32(paddr + 20, (sector_count >> 32) as u32);
    mem.write_u16(paddr + 24, 512);
}
