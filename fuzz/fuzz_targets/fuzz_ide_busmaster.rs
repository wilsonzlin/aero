#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::pci_ide::IdeController;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE, VirtualDisk};
use memory::{Bus, MemoryBus};

const PRIMARY_BASE: u16 = 0x1F0;

// Bus Master IDE register block size is 16 bytes (two channels).
const DEFAULT_BUS_MASTER_BASE: u16 = 0xC000;

const MAX_PRDS: u8 = 32;
const MAX_SECTORS: u8 = 8;

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

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Fixed RAM size to keep fuzz runs deterministic and allocations bounded.
    let mem_size = 256usize * 1024;

    // Parse control values first so the remaining bytes can be used as guest RAM contents.
    let bm_base_seed: u16 = u.arbitrary().unwrap_or(DEFAULT_BUS_MASTER_BASE);
    let dma_write: bool = u.arbitrary().unwrap_or(false);
    let sectors_seed: u8 = u.arbitrary().unwrap_or(1);
    let prd_count_seed: u8 = u.arbitrary().unwrap_or(1);
    let prd_table_seed: u64 = u.arbitrary().unwrap_or(0);
    let lba_seed: u32 = u.arbitrary().unwrap_or(0);
    let force_eot: bool = u.arbitrary().unwrap_or(true);
    let mismatch_dir: bool = u.arbitrary().unwrap_or(false);

    let prd_count = (prd_count_seed % MAX_PRDS).max(1) as usize;
    let mut prd_entries: Vec<(u32, u16, bool)> = Vec::with_capacity(prd_count);
    for i in 0..prd_count {
        let mut addr: u32 = u.arbitrary().unwrap_or(0);
        let byte_count: u16 = u.arbitrary().unwrap_or(0);
        let mut eot: bool = u.arbitrary().unwrap_or(false);
        if i == prd_count - 1 && force_eot {
            eot = true;
        }
        // Bias some PRD buffers into RAM so DMA actually touches our Vec-backed memory.
        if (addr & 1) == 0 {
            addr %= mem_size as u32;
        }
        prd_entries.push((addr, byte_count, eot));
    }

    let mut mem = Bus::new(mem_size);

    // Seed RAM from the input.
    {
        let init_len = u.len();
        let init = u.bytes(init_len).unwrap_or(&[]);
        let ram = mem.ram_mut();
        let n = init.len().min(ram.len());
        ram[..n].copy_from_slice(&init[..n]);
    }

    // Small disk backing so fuzz runs stay fast.
    let capacity = 64 * SECTOR_SIZE as u64;
    let disk = match RawDisk::create(MemBackend::new(), capacity) {
        Ok(d) => d,
        Err(_) => return,
    };
    let drive = match AtaDrive::new(Box::new(disk) as Box<dyn VirtualDisk>) {
        Ok(d) => d,
        Err(_) => return,
    };

    // Bus Master base (aligned to 16 bytes).
    let bus_master_base: u16 = bm_base_seed & 0xFFF0;

    let mut ctl = IdeController::new(bus_master_base);
    ctl.attach_primary_master_ata(drive);

    // Synthesize a minimally well-formed PRD table + DMA command to reach the Bus Master DMA
    // descriptor parsing and scatter/gather logic.
    let sectors: u8 = (sectors_seed % MAX_SECTORS).max(1);

    // Place a PRD table in RAM (8 bytes per entry).
    let prd_bytes = prd_entries.len().saturating_mul(8);
    let prd_addr = place_region(prd_table_seed, mem_size, 4, prd_bytes.max(8))
        .min(u32::MAX as u64) as u32;

    for (i, (addr, count, eot)) in prd_entries.into_iter().enumerate() {
        let entry_addr = (prd_addr as u64).wrapping_add((i as u64) * 8);
        mem.write_u32(entry_addr, addr);
        mem.write_u16(entry_addr + 4, count);
        let flags = if eot { 0x8000u16 } else { 0 };
        mem.write_u16(entry_addr + 6, flags);
    }

    // Program Bus Master PRD base (primary channel, reg 4).
    ctl.io_write(bus_master_base + 4, 4, prd_addr);

    // Program ATA registers for a 28-bit DMA command (primary master, LBA mode).
    let lba_max = (capacity / SECTOR_SIZE as u64).saturating_sub(sectors as u64);
    let lba: u32 = if lba_max == 0 { 0 } else { lba_seed % (lba_max as u32 + 1) };

    // Drive/head: master + LBA bit set.
    let dev = 0xE0u8 | ((lba >> 24) as u8 & 0x0F);
    ctl.io_write(PRIMARY_BASE + 6, 1, dev as u32);
    ctl.io_write(PRIMARY_BASE + 2, 1, sectors as u32);
    ctl.io_write(PRIMARY_BASE + 3, 1, (lba & 0xFF) as u32);
    ctl.io_write(PRIMARY_BASE + 4, 1, ((lba >> 8) & 0xFF) as u32);
    ctl.io_write(PRIMARY_BASE + 5, 1, ((lba >> 16) & 0xFF) as u32);

    // Issue ATA DMA command: READ DMA (0xC8) or WRITE DMA (0xCA).
    let cmd = if dma_write { 0xCAu8 } else { 0xC8u8 };
    ctl.io_write(PRIMARY_BASE + 7, 1, cmd as u32);

    // Start Bus Master engine (primary channel). Direction bit:
    // - 1 => device -> memory (read)
    // - 0 => memory -> device (write)
    let mut bm_cmd = 0x01u32;
    let dir_bit = if dma_write { 0 } else { 0x08 };
    let dir_bit = if mismatch_dir { dir_bit ^ 0x08 } else { dir_bit };
    bm_cmd |= dir_bit;
    ctl.io_write(bus_master_base + 0, 1, bm_cmd);

    // Complete DMA synchronously.
    ctl.tick(&mut mem);
});
