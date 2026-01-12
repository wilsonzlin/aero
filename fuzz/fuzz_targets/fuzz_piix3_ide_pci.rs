#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices::pci::PciDevice;
use aero_devices_storage::atapi::{AtapiCdrom, IsoBackend};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::pci_ide::Piix3IdePciDevice;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE, VirtualDisk};
use memory::MemoryBus;

const PRIMARY_BASE: u16 = 0x1F0;
const PRIMARY_CTRL: u16 = 0x3F6;
const SECONDARY_BASE: u16 = 0x170;
const SECONDARY_CTRL: u16 = 0x376;

#[derive(Clone)]
struct MemIso {
    sector_count: u32,
    data: Vec<u8>,
}

impl MemIso {
    fn new(sector_count: u32, init: &[u8]) -> Self {
        let bytes_len = sector_count as usize * AtapiCdrom::SECTOR_SIZE;
        let mut data = vec![0u8; bytes_len];
        if !data.is_empty() && !init.is_empty() {
            let mut off = 0usize;
            while off < data.len() {
                let take = (data.len() - off).min(init.len());
                data[off..off + take].copy_from_slice(&init[..take]);
                off += take;
            }
        }
        Self { sector_count, data }
    }
}

impl IsoBackend for MemIso {
    fn sector_count(&self) -> u32 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u32, buf: &mut [u8]) -> std::io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        if !buf.len().is_multiple_of(AtapiCdrom::SECTOR_SIZE) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unaligned ATAPI read length",
            ));
        }

        let start = (lba as u64)
            .checked_mul(AtapiCdrom::SECTOR_SIZE as u64)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow"))?;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "length overflow"))?;

        if end > self.data.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "read beyond end of ISO",
            ));
        }

        buf.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
}

/// Simple bounded guest-physical memory implementation for Bus Master IDE DMA fuzzing.
///
/// Reads outside the provided buffer return zeros; writes are dropped. This matches common
/// "unmapped memory" semantics and ensures the fuzzer cannot trigger host-side OOB accesses via
/// malicious PRD addresses.
#[derive(Clone)]
struct FuzzBus {
    data: Vec<u8>,
}

impl FuzzBus {
    fn new(size: usize, init: &[u8]) -> Self {
        let mut data = vec![0u8; size];
        let n = init.len().min(size);
        data[..n].copy_from_slice(&init[..n]);
        Self { data }
    }
}

impl MemoryBus for FuzzBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        buf.fill(0);
        if buf.is_empty() {
            return;
        }
        if paddr.checked_add(buf.len() as u64).is_none() {
            return;
        }
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        if start >= self.data.len() {
            return;
        }
        let avail = self.data.len() - start;
        let n = avail.min(buf.len());
        buf[..n].copy_from_slice(&self.data[start..start + n]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        if paddr.checked_add(buf.len() as u64).is_none() {
            return;
        }
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        if start >= self.data.len() {
            return;
        }
        let avail = self.data.len() - start;
        let n = avail.min(buf.len());
        self.data[start..start + n].copy_from_slice(&buf[..n]);
    }
}

fn place_region(seed: u64, mem_size: usize, align: usize, size: usize) -> u64 {
    if mem_size <= size {
        return 0;
    }
    let max_start = mem_size - size;
    let mut off = (seed as usize) % (max_start + 1);
    if align.is_power_of_two() {
        off &= !(align - 1);
    }
    off as u64
}

fn decode_size(bits: u8) -> u8 {
    match bits % 3 {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

fn build_known_ports(bus_master_base: u16) -> Vec<u16> {
    let mut ports = Vec::new();

    // Primary taskfile ports.
    for p in PRIMARY_BASE..=PRIMARY_BASE + 7 {
        ports.push(p);
    }
    // Primary control ports.
    ports.push(PRIMARY_CTRL);
    ports.push(PRIMARY_CTRL + 1);

    // Secondary taskfile ports.
    for p in SECONDARY_BASE..=SECONDARY_BASE + 7 {
        ports.push(p);
    }
    // Secondary control ports.
    ports.push(SECONDARY_CTRL);
    ports.push(SECONDARY_CTRL + 1);

    // Bus Master IDE ports (BAR4): 16 bytes.
    for p in bus_master_base..=bus_master_base.saturating_add(15) {
        ports.push(p);
    }

    ports
}

#[derive(Clone, Copy)]
enum Op {
    ConfigRead { offset: u8, size: usize },
    ConfigWrite { offset: u8, size: usize, value: u32 },
    IoRead { port: u16, size: u8 },
    IoWrite { port: u16, size: u8, value: u32 },
    Tick,
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // 64KiB..=1MiB guest RAM backing for Bus Master DMA.
    let pow: u8 = u.int_in_range(0u8..=4).unwrap_or(0);
    let mem_size: usize = (64usize * 1024) << pow;

    // Pick a Bus Master base aligned to 16 bytes so we cover edge cases like 0xFFF0.
    let bm_base_seed: u16 = u.arbitrary().unwrap_or(Piix3IdePciDevice::DEFAULT_BUS_MASTER_BASE);
    let bus_master_base: u16 = bm_base_seed & 0xFFF0;

    // Attach either an ATA disk or an ATAPI CDROM so we cover both ATA DMA and ATAPI PACKET+DMA
    // paths through the PCI wrapper.
    let attach_atapi: bool = u.arbitrary().unwrap_or(false);
    let iso_sectors: u32 = (u.arbitrary::<u8>().unwrap_or(32) as u32 % 128).max(1);

    // PCI command register seed; we selectively force IO decode / bus mastering for better
    // coverage.
    let cmd_seed: u16 = u.arbitrary().unwrap_or(0);
    let force_enable: bool = u.arbitrary().unwrap_or(true);

    // Remaining bytes initialize guest RAM.
    let rest_len = u.len();
    let init = u.bytes(rest_len).unwrap_or(&[]);
    let mut mem = FuzzBus::new(mem_size, init);

    let mut dev = Piix3IdePciDevice::new();
    let capacity = 64 * SECTOR_SIZE as u64;
    if attach_atapi {
        let backend = MemIso::new(iso_sectors, init);
        dev.controller
            .attach_primary_master_atapi(AtapiCdrom::new(Some(Box::new(backend))));
    } else {
        // Keep the disk small so fuzz runs stay fast.
        let disk = match RawDisk::create(MemBackend::new(), capacity) {
            Ok(d) => d,
            Err(_) => return,
        };
        let drive = match AtaDrive::new(Box::new(disk) as Box<dyn VirtualDisk>) {
            Ok(d) => d,
            Err(_) => return,
        };
        dev.controller.attach_primary_master_ata(drive);
    }

    // Program BAR4 via a realistic config write (this applies PCI masking semantics).
    let bar4_value: u32 = (bus_master_base as u32) | 0x01;
    let _ = dev.config_mut().write_with_effects(0x20, 4, bar4_value);

    // Decide whether to force IO + BM enabled; otherwise let fuzz input decide.
    let command = if force_enable {
        cmd_seed | 0x0005 // IO space + bus master
    } else {
        cmd_seed & !0x0005
    };
    dev.config_mut().set_command(command);

    let bm_base = dev.bus_master_base();
    let known_ports = build_known_ports(bm_base);

    // Optional "DMA seed" to reach deeper BMIDE parsing early.
    let seed_dma: bool = u.arbitrary().unwrap_or(true);
    if seed_dma {
        let prd_seed: u64 = u.arbitrary().unwrap_or(0);
        let buf_seed: u64 = u.arbitrary().unwrap_or(0);

        if attach_atapi {
            // ATAPI PACKET + DMA-in seed: issue READ(10) with DMA requested.
            let blocks: u16 = ((u.arbitrary::<u8>().unwrap_or(1) % 4).max(1)) as u16;
            let byte_len = blocks as usize * AtapiCdrom::SECTOR_SIZE;

            // Place a single-entry PRD table (8 bytes) and one contiguous data buffer.
            let prd_addr = place_region(prd_seed, mem_size, 4, 8).min(u32::MAX as u64) as u32;
            let buf_addr = place_region(buf_seed, mem_size, 2, byte_len)
                .min(u32::MAX as u64) as u32;

            // PRD entry: [addr: u32][byte_count: u16][flags: u16]. End-of-table bit is 0x8000.
            mem.write_u32(prd_addr as u64, buf_addr);
            mem.write_u16(prd_addr as u64 + 4, byte_len as u16);
            mem.write_u16(prd_addr as u64 + 6, 0x8000);

            // Program Bus Master PRD base (primary channel, reg 4).
            dev.io_write(bm_base + 4, 4, prd_addr);

            // Request DMA via Features bit0.
            dev.io_write(PRIMARY_BASE + 1, 1, 1);

            // Issue PACKET command.
            dev.io_write(PRIMARY_BASE + 7, 1, 0xA0);

            // READ(10) packet, with bounded blocks count.
            let max_lba = iso_sectors.saturating_sub(blocks as u32).saturating_sub(1);
            let lba: u32 = if max_lba == 0 {
                0
            } else {
                u.arbitrary::<u32>().unwrap_or(0) % max_lba.max(1)
            };
            let mut packet = [0u8; 12];
            packet[0] = 0x28; // READ(10)
            packet[2..6].copy_from_slice(&lba.to_be_bytes());
            packet[7..9].copy_from_slice(&blocks.to_be_bytes());

            // Write packet as 6 words to the data register (little-endian words).
            for chunk in packet.chunks_exact(2) {
                let word = u16::from_le_bytes([chunk[0], chunk[1]]);
                dev.io_write(PRIMARY_BASE + 0, 2, word as u32);
            }

            // Start Bus Master engine (device -> memory).
            dev.io_write(bm_base + 0, 1, 0x09);

            dev.tick(&mut mem);
        } else {
            let dma_write: bool = u.arbitrary().unwrap_or(false);
            let sectors: u8 = (u.arbitrary::<u8>().unwrap_or(1) % 8).max(1);
            let byte_len = sectors as usize * SECTOR_SIZE;

            // Place a single-entry PRD table (8 bytes) and one contiguous data buffer.
            let prd_addr = place_region(prd_seed, mem_size, 4, 8).min(u32::MAX as u64) as u32;
            let buf_addr = place_region(buf_seed, mem_size, 2, byte_len)
                .min(u32::MAX as u64) as u32;

            // PRD entry: [addr: u32][byte_count: u16][flags: u16]. End-of-table bit is 0x8000.
            mem.write_u32(prd_addr as u64, buf_addr);
            mem.write_u16(prd_addr as u64 + 4, byte_len as u16);
            mem.write_u16(prd_addr as u64 + 6, 0x8000);

            // Program Bus Master PRD base (primary channel, reg 4).
            dev.io_write(bm_base + 4, 4, prd_addr);

            // Program ATA registers for a 28-bit DMA command (primary master, LBA mode).
            let lba_max = (capacity / SECTOR_SIZE as u64).saturating_sub(sectors as u64);
            let lba: u32 = if lba_max == 0 {
                0
            } else {
                u.arbitrary::<u32>().unwrap_or(0) % (lba_max as u32 + 1)
            };

            // Drive/head: master + LBA bit set.
            let dev_sel = 0xE0u8 | ((lba >> 24) as u8 & 0x0F);
            dev.io_write(PRIMARY_BASE + 6, 1, dev_sel as u32);
            dev.io_write(PRIMARY_BASE + 2, 1, sectors as u32);
            dev.io_write(PRIMARY_BASE + 3, 1, (lba & 0xFF) as u32);
            dev.io_write(PRIMARY_BASE + 4, 1, ((lba >> 8) & 0xFF) as u32);
            dev.io_write(PRIMARY_BASE + 5, 1, ((lba >> 16) & 0xFF) as u32);

            // Issue ATA DMA command: READ DMA (0xC8) or WRITE DMA (0xCA).
            let cmd = if dma_write { 0xCAu8 } else { 0xC8u8 };
            dev.io_write(PRIMARY_BASE + 7, 1, cmd as u32);

            // Start Bus Master engine (primary channel). Direction bit:
            // - 1 => device -> memory (read)
            // - 0 => memory -> device (write)
            let bm_cmd = if dma_write { 0x01u32 } else { 0x09u32 };
            dev.io_write(bm_base + 0, 1, bm_cmd);

            // Complete DMA synchronously.
            dev.tick(&mut mem);
        }
    }

    let ops_len: usize = u.int_in_range(0usize..=1024).unwrap_or(0);
    let mut ops = Vec::with_capacity(ops_len);
    for _ in 0..ops_len {
        let kind: u8 = u.arbitrary().unwrap_or(0);
        match kind % 5 {
            0 => {
                let offset: u8 = u.arbitrary().unwrap_or(0);
                let size = match kind % 3 {
                    0 => 1,
                    1 => 2,
                    _ => 4,
                };
                ops.push(Op::ConfigRead { offset, size });
            }
            1 => {
                let mut offset: u8 = u.arbitrary().unwrap_or(0);
                let mut size = match kind % 3 {
                    0 => 1,
                    1 => 2,
                    _ => 4,
                };
                let value: u32 = u.arbitrary().unwrap_or(0);

                // BAR writes must be 32-bit aligned dword accesses; coerce to that form.
                if (0x10..=0x27).contains(&offset) {
                    offset &= !0x3;
                    size = 4;
                }
                ops.push(Op::ConfigWrite { offset, size, value });
            }
            2 => {
                let use_known: bool = u.arbitrary().unwrap_or(true);
                let port = if use_known && !known_ports.is_empty() {
                    let idx = u
                        .int_in_range(0usize..=known_ports.len().saturating_sub(1))
                        .unwrap_or(0);
                    known_ports[idx]
                } else {
                    u.arbitrary::<u16>().unwrap_or(0)
                };
                let size = decode_size(u.arbitrary().unwrap_or(0));
                ops.push(Op::IoRead { port, size });
            }
            3 => {
                let use_known: bool = u.arbitrary().unwrap_or(true);
                let port = if use_known && !known_ports.is_empty() {
                    let idx = u
                        .int_in_range(0usize..=known_ports.len().saturating_sub(1))
                        .unwrap_or(0);
                    known_ports[idx]
                } else {
                    u.arbitrary::<u16>().unwrap_or(0)
                };
                let size = decode_size(u.arbitrary().unwrap_or(0));
                let val: u32 = u.arbitrary().unwrap_or(0);
                ops.push(Op::IoWrite { port, size, value: val });
            }
            _ => ops.push(Op::Tick),
        }
    }

    for op in ops {
        match op {
            Op::ConfigRead { offset, size } => {
                // PciConfigSpace asserts on invalid sizes/offsets; keep reads in-bounds.
                let off = offset as u16;
                if off as usize + size <= 256 && matches!(size, 1 | 2 | 4) {
                    let _ = dev.config_mut().read(off, size);
                }
            }
            Op::ConfigWrite { offset, size, value } => {
                let off = offset as u16;
                if off as usize + size <= 256 && matches!(size, 1 | 2 | 4) {
                    let _ = dev.config_mut().write_with_effects(off, size, value);
                }
            }
            Op::IoRead { port, size } => {
                let _ = dev.io_read(port, size);
            }
            Op::IoWrite { port, size, value } => {
                dev.io_write(port, size, value);
            }
            Op::Tick => dev.tick(&mut mem),
        }
    }

    dev.tick(&mut mem);
});
