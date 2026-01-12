#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::pci_ide::IdeController;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE, VirtualDisk};
use memory::MemoryBus;

const PRIMARY_BASE: u16 = 0x1F0;
const PRIMARY_CTRL: u16 = 0x3F6;
const SECONDARY_BASE: u16 = 0x170;
const SECONDARY_CTRL: u16 = 0x376;

// Bus Master IDE register block size is 16 bytes (two channels).
const DEFAULT_BUS_MASTER_BASE: u16 = 0xC000;

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
    for p in bus_master_base..bus_master_base.saturating_add(16) {
        ports.push(p);
    }
    ports
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // 64KiB..=1MiB guest RAM backing for Bus Master DMA.
    let pow: u8 = u.int_in_range(0u8..=4).unwrap_or(0);
    let mem_size: usize = (64usize * 1024) << pow;

    // Optionally vary the Bus Master base; align to 16 bytes.
    let bm_base_seed: u16 = u.arbitrary().unwrap_or(DEFAULT_BUS_MASTER_BASE);
    let bus_master_base: u16 = bm_base_seed & 0xFFF0;

    // Keep the disk small so fuzz runs stay fast.
    let capacity = 64 * SECTOR_SIZE as u64;
    let disk = match RawDisk::create(MemBackend::new(), capacity) {
        Ok(d) => d,
        Err(_) => return,
    };
    let drive = match AtaDrive::new(Box::new(disk) as Box<dyn VirtualDisk>) {
        Ok(d) => d,
        Err(_) => return,
    };

    // Randomize whether we attach the disk to the primary or secondary channel so we cover both
    // port decode and bus master channel selection logic.
    let attach_secondary: bool = u.arbitrary().unwrap_or(false);

    // Remaining bytes initialize guest RAM.
    let rest_len = u.len();
    let init = u.bytes(rest_len).unwrap_or(&[]);
    let mut mem = FuzzBus::new(mem_size, init);

    let mut ctl = IdeController::new(bus_master_base);
    if attach_secondary {
        ctl.attach_secondary_master_ata(drive);
    } else {
        ctl.attach_primary_master_ata(drive);
    }

    // Bias towards at least occasionally exercising the Bus Master DMA code paths with a
    // minimally well-formed PRD table and a DMA command.
    let synthesize_dma: bool = u.arbitrary().unwrap_or(true);
    if synthesize_dma {
        let cmd_base = if attach_secondary {
            SECONDARY_BASE
        } else {
            PRIMARY_BASE
        };
        let bm_chan_off: u16 = if attach_secondary { 8 } else { 0 };

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

        // Program Bus Master PRD base for the selected channel (reg 4).
        ctl.io_write(bus_master_base + bm_chan_off + 4, 4, prd_addr);

        // Program ATA registers for a 28-bit DMA command (master, LBA mode).
        let lba_max = (capacity / SECTOR_SIZE as u64).saturating_sub(sectors as u64);
        let lba: u32 = if lba_max == 0 {
            0
        } else {
            u.arbitrary::<u32>().unwrap_or(0) % (lba_max as u32 + 1)
        };

        // Drive/head: master + LBA bit set.
        let dev = 0xE0u8 | ((lba >> 24) as u8 & 0x0F);
        ctl.io_write(cmd_base + 6, 1, dev as u32);
        ctl.io_write(cmd_base + 2, 1, sectors as u32);
        ctl.io_write(cmd_base + 3, 1, (lba & 0xFF) as u32);
        ctl.io_write(cmd_base + 4, 1, ((lba >> 8) & 0xFF) as u32);
        ctl.io_write(cmd_base + 5, 1, ((lba >> 16) & 0xFF) as u32);

        // Issue ATA DMA command: READ DMA (0xC8) or WRITE DMA (0xCA).
        let cmd = if dma_write { 0xCAu8 } else { 0xC8u8 };
        ctl.io_write(cmd_base + 7, 1, cmd as u32);

        // Start Bus Master engine (selected channel). Direction bit:
        // - 1 => device -> memory (read)
        // - 0 => memory -> device (write)
        let bm_cmd = if dma_write { 0x01u32 } else { 0x09u32 };
        ctl.io_write(bus_master_base + bm_chan_off + 0, 1, bm_cmd);

        // Complete DMA synchronously.
        ctl.tick(&mut mem);
    }

    let known_ports = build_known_ports(bus_master_base);

    let ops_len: usize = u.int_in_range(0usize..=1024).unwrap_or(0);
    for _ in 0..ops_len {
        let kind: u8 = u.arbitrary().unwrap_or(0);
        match kind % 3 {
            0 => {
                // Read
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
                let _ = ctl.io_read(port, size);
            }
            1 => {
                // Write
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
                ctl.io_write(port, size, val);
            }
            _ => {
                // Tick DMA engine.
                ctl.tick(&mut mem);
            }
        }
    }

    // Ensure we run at least one tick so a synthesized DMA command can't be optimized away.
    ctl.tick(&mut mem);
});
