use std::collections::VecDeque;

use crate::acpi::{tables::build_acpi_table_set_with_hpet, BuiltAcpiTables};
use crate::bus::Bus;
use crate::realmode::RealModeCpu;
use crate::e820::{build_default_e820, E820Entry};

#[derive(Debug, Clone)]
pub struct BiosConfig {
    pub ram_size: u64,
    pub acpi_base: u32,
    pub hpet_base: u64,
}

#[derive(Debug, Clone)]
pub struct Disk {
    data: Vec<u8>,
    pub sectors_per_track: u16,
    pub heads: u16,
}

impl Disk {
    pub const SECTOR_SIZE: usize = 512;

    pub fn empty() -> Self {
        Self {
            data: Vec::new(),
            sectors_per_track: 63,
            heads: 16,
        }
    }

    pub fn from_bytes(data: Vec<u8>) -> Self {
        assert!(
            data.len() % Self::SECTOR_SIZE == 0,
            "disk size must be a multiple of 512"
        );
        Self {
            data,
            sectors_per_track: 63,
            heads: 16,
        }
    }

    pub fn sector_count(&self) -> u64 {
        (self.data.len() / Self::SECTOR_SIZE) as u64
    }

    fn read_sector(&self, lba: u64, dst: &mut [u8; Self::SECTOR_SIZE]) -> bool {
        let start = (lba as usize) * Self::SECTOR_SIZE;
        let end = start + Self::SECTOR_SIZE;
        if end > self.data.len() {
            return false;
        }
        dst.copy_from_slice(&self.data[start..end]);
        true
    }
}

#[derive(Debug, Clone)]
pub struct Keyboard {
    queue: VecDeque<(u8, u8)>, // (ascii, scan)
}

impl Keyboard {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    pub fn push_key(&mut self, ascii: u8, scan: u8) {
        self.queue.push_back((ascii, scan));
    }

    fn pop_key(&mut self) -> Option<(u8, u8)> {
        self.queue.pop_front()
    }
}

#[derive(Debug, Clone)]
pub struct LegacyBios {
    pub config: BiosConfig,
    pub acpi: BuiltAcpiTables,
    pub e820: Vec<E820Entry>,
    pub disk0: Disk,
    pub keyboard: Keyboard,
}

impl LegacyBios {
    pub fn new(config: BiosConfig) -> Self {
        let acpi = build_acpi_table_set_with_hpet(config.acpi_base as u64, config.hpet_base);
        let e820 = build_default_e820(config.ram_size);
        Self {
            config,
            acpi,
            e820,
            disk0: Disk::empty(),
            keyboard: Keyboard::new(),
        }
    }

    pub fn post<B: Bus>(&self, bus: &mut B) {
        // ACPI tables are prebuilt on the host and laid out contiguously.
        bus.write(self.acpi.dsdt_address as u32, &self.acpi.dsdt);
        bus.write(self.acpi.fadt_address as u32, &self.acpi.fadt);
        bus.write(self.acpi.madt_address as u32, &self.acpi.madt);
        bus.write(self.acpi.hpet_address as u32, &self.acpi.hpet);
        bus.write(self.acpi.rsdt_address as u32, &self.acpi.rsdt);
        bus.write(self.acpi.xsdt_address as u32, &self.acpi.xsdt);
        bus.write(self.acpi.rsdp_address as u32, &self.acpi.rsdp);
    }

    pub fn handle_interrupt<B: Bus>(&mut self, int: u8, bus: &mut B, cpu: &mut RealModeCpu) {
        match int {
            0x10 => self.handle_int10(bus, cpu),
            0x13 => self.handle_int13(bus, cpu),
            0x15 => self.handle_int15(bus, cpu),
            0x16 => self.handle_int16(bus, cpu),
            _ => {
                cpu.set_carry(true);
            }
        }
    }

    pub fn handle_int10<B: Bus>(&mut self, bus: &mut B, cpu: &mut RealModeCpu) {
        match cpu.ah() {
            0x0E => {
                bus.serial_write(cpu.al());
                cpu.set_carry(false);
            }
            _ => {
                cpu.set_carry(true);
            }
        }
    }

    pub fn handle_int16<B: Bus>(&mut self, _bus: &mut B, cpu: &mut RealModeCpu) {
        match cpu.ah() {
            0x00 => {
                if let Some((ascii, scan)) = self.keyboard.pop_key() {
                    cpu.set_ax(((scan as u16) << 8) | (ascii as u16));
                    cpu.set_carry(false);
                } else {
                    cpu.set_ax(0);
                    cpu.set_carry(true);
                }
            }
            _ => {
                cpu.set_carry(true);
            }
        }
    }

    pub fn handle_int13<B: Bus>(&mut self, bus: &mut B, cpu: &mut RealModeCpu) {
        let ah = cpu.ah();
        match ah {
            0x00 => {
                // reset disk
                cpu.set_ah(0);
                cpu.set_carry(false);
            }
            0x02 => {
                // read sectors (CHS)
                let count = cpu.al() as u64;
                let ch = ((cpu.ecx as u16) >> 8) as u8;
                let cl = cpu.ecx as u8;
                let dh = ((cpu.edx as u16) >> 8) as u8;
                let dl = cpu.edx as u8;

                if dl != 0x80 || count == 0 {
                    cpu.set_ah(0x01);
                    cpu.set_carry(true);
                    return;
                }

                let sector = (cl & 0x3F) as u64;
                if sector == 0 {
                    cpu.set_ah(0x04);
                    cpu.set_carry(true);
                    return;
                }

                let cylinder = ch as u64;
                let head = dh as u64;
                let lba = (cylinder * (self.disk0.heads as u64) + head)
                    * (self.disk0.sectors_per_track as u64)
                    + (sector - 1);

                let total = self.disk0.sector_count();
                if lba.checked_add(count).map_or(true, |end| end > total) {
                    cpu.set_ah(0x04);
                    cpu.set_carry(true);
                    return;
                }

                let mut sector_buf = [0u8; Disk::SECTOR_SIZE];
                let buf_phys = RealModeCpu::seg_off(cpu.es, cpu.bx());
                for i in 0..count {
                    if !self.disk0.read_sector(lba + i, &mut sector_buf) {
                        cpu.set_ah(0x04);
                        cpu.set_carry(true);
                        return;
                    }
                    bus.write(buf_phys + (i as u32) * (Disk::SECTOR_SIZE as u32), &sector_buf);
                }

                cpu.set_ah(0);
                cpu.set_al(count as u8);
                cpu.set_carry(false);
            }
            _ => {
                cpu.set_ah(0x01);
                cpu.set_carry(true);
            }
        }
    }

    pub fn handle_int15<B: Bus>(&mut self, bus: &mut B, cpu: &mut RealModeCpu) {
        match cpu.eax as u16 {
            0x2400 => {
                // Disable A20.
                bus.set_a20_enabled(false);
                cpu.set_ah(0);
                cpu.set_carry(false);
            }
            0x2401 => {
                // Enable A20.
                bus.set_a20_enabled(true);
                cpu.set_ah(0);
                cpu.set_carry(false);
            }
            0x2402 => {
                // Query A20: AL=0 disabled / AL=1 enabled.
                cpu.set_al(if bus.a20_enabled() { 1 } else { 0 });
                cpu.set_ah(0);
                cpu.set_carry(false);
            }
            0x2403 => {
                // A20 support bitmask (keyboard controller + port 92 + INT15).
                cpu.set_bx(0x0007);
                cpu.set_ah(0);
                cpu.set_carry(false);
            }
            0xE801 => {
                let (ax_kb, bx_blocks) = e801_from_e820(&self.e820);
                cpu.set_ax(ax_kb);
                cpu.set_bx(bx_blocks);
                cpu.set_cx(ax_kb);
                cpu.set_dx(bx_blocks);
                cpu.set_carry(false);
            }
            0xE820 => {
                if cpu.edx != 0x534D_4150 {
                    // 'SMAP'
                    cpu.set_carry(true);
                    cpu.eax = 0;
                    return;
                }

                let idx = cpu.ebx as usize;
                if idx >= self.e820.len() {
                    cpu.set_carry(true);
                    return;
                }

                let entry = self.e820[idx];
                let next = if idx + 1 < self.e820.len() {
                    (idx + 1) as u32
                } else {
                    0
                };

                let buf_size = cpu.ecx as usize;
                let write_len = if buf_size >= 24 { 24 } else { 20.min(buf_size) };

                let mut raw = [0u8; 24];
                raw[0..8].copy_from_slice(&entry.base.to_le_bytes());
                raw[8..16].copy_from_slice(&entry.length.to_le_bytes());
                raw[16..20].copy_from_slice(&entry.typ.to_le_bytes());
                raw[20..24].copy_from_slice(&1u32.to_le_bytes()); // ACPI 3.0 extended attributes

                let buf_phys = RealModeCpu::seg_off(cpu.es, cpu.edi as u16);
                bus.write(buf_phys, &raw[..write_len]);

                cpu.eax = 0x534D_4150;
                cpu.ebx = next;
                cpu.ecx = write_len as u32;
                cpu.set_carry(false);
            }
            _ => {
                cpu.set_carry(true);
                cpu.set_ah(0x86);
            }
        }
    }
}

fn e801_from_e820(map: &[E820Entry]) -> (u16, u16) {
    const ONE_MIB: u64 = 0x0010_0000;
    const SIXTEEN_MIB: u64 = 0x0100_0000;
    const FOUR_GIB: u64 = 0x1_0000_0000;

    let bytes_1m_to_16m = sum_e820_ram(map, ONE_MIB, SIXTEEN_MIB);
    let bytes_16m_to_4g = sum_e820_ram(map, SIXTEEN_MIB, FOUR_GIB);

    let ax_kb = (bytes_1m_to_16m / 1024).min(0x3C00) as u16;
    let bx_blocks = (bytes_16m_to_4g / 65536).min(0xFFFF) as u16;
    (ax_kb, bx_blocks)
}

fn sum_e820_ram(map: &[E820Entry], start: u64, end: u64) -> u64 {
    let mut total = 0u64;
    for entry in map {
        if entry.typ != E820Entry::TYPE_RAM || entry.length == 0 {
            continue;
        }
        let entry_start = entry.base;
        let entry_end = entry.end();
        let overlap_start = entry_start.max(start);
        let overlap_end = entry_end.min(end);
        if overlap_end > overlap_start {
            total = total.saturating_add(overlap_end - overlap_start);
        }
    }
    total
}
