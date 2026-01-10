use core::cmp::min;
use core::mem::size_of;
use std::collections::VecDeque;

use crate::types::{
    E820Entry, RealModeCpu, E820_TYPE_RAM, E820_TYPE_RESERVED, FLAG_CF, FLAG_IF, FLAG_ZF,
};

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
}

impl Default for BiosConfig {
    fn default() -> Self {
        Self {
            total_memory_bytes: 64 * 1024 * 1024,
            boot_device: BootDevice::Hdd0,
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

pub trait BlockDevice {
    fn read_sector(&self, lba: u64, buf512: &mut [u8; 512]) -> Result<(), DiskError>;
    fn sector_count(&self) -> u64;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskError {
    OutOfRange,
    IoError,
    InvalidPacket,
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

    // VGA text-mode state.
    video_mode: u8,
    cursor_row: u8,
    cursor_col: u8,
    text_attr: u8,

    // Keyboard buffer (BIOS-side, independent of i8042 model for now).
    kb_buf: VecDeque<u16>,
}

impl Bios {
    pub fn new(cfg: BiosConfig) -> Self {
        let e820 = build_e820_map(cfg.total_memory_bytes);
        Self {
            cfg,
            e820,
            pci_devices: Vec::new(),
            video_mode: 0x03,
            cursor_row: 0,
            cursor_col: 0,
            text_attr: 0x07,
            kb_buf: VecDeque::new(),
        }
    }

    /// Perform a simplified POST and transfer control to the boot sector.
    pub fn post<M: Memory, D: BlockDevice>(
        &mut self,
        cpu: &mut RealModeCpu,
        mem: &mut M,
        disk: &D,
    ) {
        let mut null_kbd = NullKeyboard;
        self.post_with_devices(cpu, mem, disk, &mut null_kbd, None);
    }

    /// POST with optional devices attached (keyboard + PCI config space).
    pub fn post_with_devices<M: Memory, D: BlockDevice, K: Keyboard>(
        &mut self,
        cpu: &mut RealModeCpu,
        mem: &mut M,
        disk: &D,
        _kbd: &mut K,
        mut pci: Option<&mut dyn PciConfigSpace>,
    ) {
        // Disable interrupts.
        cpu.eflags &= !FLAG_IF;

        self.init_ivt(mem);
        self.init_bda(mem);

        if let Some(pci) = pci.as_deref_mut() {
            self.enumerate_pci(pci);
        }

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

    /// Handle an x86 `INT n` instruction (emulator "VM exit").
    pub fn handle_interrupt<M: Memory, D: BlockDevice, K: Keyboard>(
        &mut self,
        int_no: u8,
        cpu: &mut RealModeCpu,
        mem: &mut M,
        disk: &D,
        kbd: &mut K,
    ) {
        match int_no {
            0x10 => self.int10(cpu, mem),
            0x13 => self.int13(cpu, mem, disk),
            0x15 => self.int15(cpu, mem),
            0x16 => self.int16(cpu, kbd),
            0x19 => self.int19(cpu, mem, disk),
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

    fn enumerate_pci<P: PciConfigSpace + ?Sized>(&mut self, pci: &mut P) {
        self.pci_devices.clear();
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
                    let irq_line = assign_pci_irq(bus, device, function);

                    // Program Interrupt Line register (0x3C, low byte).
                    let reg_3c = pci.read_config_dword(bus, device, function, 0x3C);
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
    }

    fn init_video<M: Memory>(&mut self, mem: &mut M) {
        self.video_mode = 0x03;
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.text_attr = 0x07;
        self.clear_screen(mem);
        self.sync_bda_cursor(mem);
    }

    fn clear_screen<M: Memory>(&self, mem: &mut M) {
        // VGA text buffer at 0xB8000, 80x25, 2 bytes per cell.
        let base = 0x000B_8000u32;
        for i in 0..(80 * 25) {
            let cell = base + (i * 2) as u32;
            mem.write_u8(cell, b' ');
            mem.write_u8(cell + 1, self.text_attr);
        }
    }

    fn sync_bda_cursor<M: Memory>(&self, mem: &mut M) {
        let bda = 0x0400u32;
        // Cursor position for page 0 at 0x0450 (row) and 0x0451 (col) via word 0x0450.
        mem.write_u16(
            bda + 0x50,
            ((self.cursor_row as u16) << 8) | self.cursor_col as u16,
        );
    }

    fn put_char<M: Memory>(&mut self, mem: &mut M, ch: u8) {
        match ch {
            b'\r' => {
                self.cursor_col = 0;
            }
            b'\n' => {
                self.cursor_col = 0;
                self.cursor_row = self.cursor_row.saturating_add(1);
            }
            0x08 => {
                // Backspace.
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
                self.write_cell(mem, self.cursor_row, self.cursor_col, b' ', self.text_attr);
            }
            ch => {
                self.write_cell(mem, self.cursor_row, self.cursor_col, ch, self.text_attr);
                self.cursor_col = self.cursor_col.saturating_add(1);
                if self.cursor_col >= 80 {
                    self.cursor_col = 0;
                    self.cursor_row = self.cursor_row.saturating_add(1);
                }
            }
        }

        if self.cursor_row >= 25 {
            self.scroll_up(mem);
            self.cursor_row = 24;
        }
        self.sync_bda_cursor(mem);
    }

    fn write_cell<M: Memory>(&self, mem: &mut M, row: u8, col: u8, ch: u8, attr: u8) {
        let idx = row as u32 * 80 + col as u32;
        let base = 0x000B_8000u32 + idx * 2;
        mem.write_u8(base, ch);
        mem.write_u8(base + 1, attr);
    }

    fn scroll_up<M: Memory>(&self, mem: &mut M) {
        let base = 0x000B_8000u32;
        // Move rows 1..24 to 0..23.
        for row in 1..25u32 {
            for col in 0..80u32 {
                let src = base + (row * 80 + col) * 2;
                let dst = base + ((row - 1) * 80 + col) * 2;
                let ch = mem.read_u8(src);
                let attr = mem.read_u8(src + 1);
                mem.write_u8(dst, ch);
                mem.write_u8(dst + 1, attr);
            }
        }
        // Clear last row.
        for col in 0..80u32 {
            let dst = base + (24 * 80 + col) * 2;
            mem.write_u8(dst, b' ');
            mem.write_u8(dst + 1, self.text_attr);
        }
    }

    fn print_str<M: Memory>(&mut self, mem: &mut M, s: &str) {
        for b in s.as_bytes().iter().copied() {
            self.put_char(mem, b);
        }
    }

    fn int10<M: Memory>(&mut self, cpu: &mut RealModeCpu, mem: &mut M) {
        match cpu.ah() {
            0x00 => {
                // Set video mode (AL).
                let mode = cpu.al();
                // We only support 80x25 text.
                self.video_mode = mode;
                self.init_video(mem);
                cpu.set_cf(false);
            }
            0x02 => {
                // Set cursor position: DH=row, DL=col.
                self.cursor_row = cpu.dh();
                self.cursor_col = cpu.dl();
                self.sync_bda_cursor(mem);
                cpu.set_cf(false);
            }
            0x03 => {
                // Get cursor position: DH=row, DL=col.
                cpu.set_dh(self.cursor_row);
                cpu.set_dl(self.cursor_col);
                cpu.set_cf(false);
            }
            0x0E => {
                // Teletype output.
                let ch = cpu.al();
                self.put_char(mem, ch);
                cpu.set_cf(false);
            }
            0x0F => {
                // Get current video mode.
                // Return: AH=columns, AL=mode, BH=active page.
                cpu.set_al(self.video_mode);
                cpu.set_ah(80);
                cpu.ebx = (cpu.ebx & 0xFFFF_FFFF) & !0xFF00; // BH=0
                cpu.set_cf(false);
            }
            _ => {
                cpu.set_cf(true);
                cpu.set_ah(0x01);
            }
        }
    }

    fn int13<M: Memory, D: BlockDevice>(&mut self, cpu: &mut RealModeCpu, mem: &mut M, disk: &D) {
        let ah = cpu.ah();
        match ah {
            0x00 => {
                // Reset.
                cpu.set_cf(false);
                cpu.set_ah(0);
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
                        cpu.set_cf(false);
                        cpu.set_ah(0);
                    }
                    Err(status) => {
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
                cpu.set_cf(false);
            }
            0x41 => {
                // Check extensions present (EDD).
                if cpu.bx() != 0x55AA {
                    cpu.set_cf(true);
                    cpu.set_ah(0x01);
                    return;
                }
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
                                cpu.set_cf(true);
                                cpu.set_ah(0x01);
                                return;
                            }
                            mem.write_bytes(buf, &sector);
                            lba += 1;
                            buf = buf.wrapping_add(512);
                        }
                        cpu.set_cf(false);
                        cpu.set_ah(0);
                    }
                    Err(_) => {
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
                cpu.set_cf(false);
                cpu.set_ah(0);
            }
            _ => {
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

    fn int19<M: Memory, D: BlockDevice>(&mut self, cpu: &mut RealModeCpu, mem: &mut M, disk: &D) {
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
}

fn assign_pci_irq(_bus: u8, device: u8, function: u8) -> u8 {
    // Deterministic, simple routing: map to IRQ10/IRQ11 via PIRQ A-D.
    // This matches the design stub in docs/09-bios-firmware.md.
    let pirq = (device.wrapping_add(function)) & 0x03;
    match pirq {
        0 | 2 => 10,
        _ => 11,
    }
}

fn build_e820_map(total_memory_bytes: u64) -> Vec<E820Entry> {
    // Keep this conservative and OS-friendly:
    // - 0x00000000..0x0009F000 : usable
    // - 0x0009F000..0x00100000 : reserved (EBDA + video + BIOS ROM shadow)
    // - 0x00100000..total      : usable
    let mut entries = Vec::new();

    entries.push(E820Entry {
        base: 0x0000_0000,
        length: 0x0009_F000,
        region_type: E820_TYPE_RAM,
        extended_attributes: 1,
    });
    entries.push(E820Entry {
        base: 0x0009_F000,
        length: 0x0006_1000,
        region_type: E820_TYPE_RESERVED,
        extended_attributes: 1,
    });

    let usable_base = 0x0010_0000u64;
    if total_memory_bytes > usable_base {
        entries.push(E820Entry {
            base: usable_base,
            length: total_memory_bytes - usable_base,
            region_type: E820_TYPE_RAM,
            extended_attributes: 1,
        });
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
    disk: &D,
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
