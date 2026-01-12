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
    let mut mem = Bus::new(mem_size);

    // Seed RAM from the input.
    {
        let rest_len = u.len();
        let init = u.bytes(rest_len).unwrap_or(&[]);
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
    let bm_base_seed: u16 = u.arbitrary().unwrap_or(DEFAULT_BUS_MASTER_BASE);
    let bus_master_base: u16 = bm_base_seed & 0xFFF0;

    let mut ctl = IdeController::new(bus_master_base);
    ctl.attach_primary_master_ata(drive);

    // Synthesize a minimally well-formed PRD table + DMA command to reach the Bus Master DMA
    // descriptor parsing and scatter/gather logic.
    let dma_write: bool = u.arbitrary().unwrap_or(false);
    let sectors: u8 = (u.arbitrary::<u8>().unwrap_or(1) % 8).max(1);
    let byte_len = sectors as usize * SECTOR_SIZE;

    let prd_seed: u64 = u.arbitrary().unwrap_or(0);
    let buf_seed: u64 = u.arbitrary().unwrap_or(0);

    // Place a single-entry PRD table (8 bytes) and one contiguous data buffer.
    let prd_addr = place_region(prd_seed, mem_size, 4, 8).min(u32::MAX as u64) as u32;
    let buf_addr = place_region(buf_seed, mem_size, 2, byte_len)
        .min(u32::MAX as u64) as u32;

    // PRD entry: [addr: u32][byte_count: u16][flags: u16]. End-of-table bit is 0x8000.
    mem.write_u32(prd_addr as u64, buf_addr);
    mem.write_u16(prd_addr as u64 + 4, byte_len as u16);
    mem.write_u16(prd_addr as u64 + 6, 0x8000);

    // Program Bus Master PRD base (primary channel, reg 4).
    ctl.io_write(bus_master_base + 4, 4, prd_addr);

    // Program ATA registers for a 28-bit DMA command (primary master, LBA mode).
    let lba_max = (capacity / SECTOR_SIZE as u64).saturating_sub(sectors as u64);
    let lba: u32 = if lba_max == 0 {
        0
    } else {
        u.arbitrary::<u32>().unwrap_or(0) % (lba_max as u32 + 1)
    };

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
    let bm_cmd = if dma_write { 0x01u32 } else { 0x09u32 };
    ctl.io_write(bus_master_base + 0, 1, bm_cmd);

    // Complete DMA synchronously.
    ctl.tick(&mut mem);
});

