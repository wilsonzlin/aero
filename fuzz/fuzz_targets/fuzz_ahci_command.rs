#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices::pci::PciDevice;
use aero_devices_storage::ata::{
    AtaDrive, ATA_CMD_FLUSH_CACHE, ATA_CMD_FLUSH_CACHE_EXT, ATA_CMD_IDENTIFY, ATA_CMD_READ_DMA_EXT,
    ATA_CMD_SET_FEATURES, ATA_CMD_WRITE_DMA_EXT,
};
use aero_devices_storage::AhciPciDevice;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE, VirtualDisk};
use memory::{Bus, MemoryBus};

// AHCI register offsets/bits (mirrors `crates/aero-devices-storage/src/ahci.rs`).
const HBA_REG_GHC: u64 = 0x04;

const PORT_BASE: u64 = 0x100;

const PORT_REG_CLB: u64 = 0x00;
const PORT_REG_CLBU: u64 = 0x04;
const PORT_REG_FB: u64 = 0x08;
const PORT_REG_FBU: u64 = 0x0C;
const PORT_REG_IE: u64 = 0x14;
const PORT_REG_CMD: u64 = 0x18;
const PORT_REG_CI: u64 = 0x38;

const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;

const PORT_IS_DHRS: u32 = 1 << 0;

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

fn write_cmd_header(
    mem: &mut dyn MemoryBus,
    clb: u64,
    slot: usize,
    ctba: u64,
    prdtl: u16,
    write: bool,
) {
    // CFL=5 dwords (20 bytes) is enough for our fixed 64-byte CFIS buffer.
    let cfl = 5u32;
    let w = if write { 1u32 << 6 } else { 0 };
    let flags = cfl | w | ((prdtl as u32) << 16);
    let addr = clb + (slot as u64) * 32;
    mem.write_u32(addr, flags);
    mem.write_u32(addr + 4, 0); // PRDBC
    mem.write_u32(addr + 8, ctba as u32);
    mem.write_u32(addr + 12, (ctba >> 32) as u32);
}

fn write_prdt(mem: &mut dyn MemoryBus, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    mem.write_u32(addr, dba as u32);
    mem.write_u32(addr + 4, (dba >> 32) as u32);
    mem.write_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    mem.write_u32(addr + 12, dbc.saturating_sub(1) & 0x003F_FFFF);
}

fn write_cfis(mem: &mut dyn MemoryBus, ctba: u64, cfis: &[u8; 64]) {
    mem.write_physical(ctba, cfis);
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Fixed-size RAM to keep allocations bounded and deterministic.
    let mem_size = 256usize * 1024;
    let mut mem = Bus::new(mem_size);

    // Seed guest RAM from the input for baseline entropy.
    {
        let init_len = u.len();
        let init = u.bytes(init_len).unwrap_or(&[]);
        let ram = mem.ram_mut();
        let n = init.len().min(ram.len());
        ram[..n].copy_from_slice(&init[..n]);
    }

    // Small disk backing (bounded to keep fuzz runs fast).
    let capacity = 64 * SECTOR_SIZE as u64;
    let disk = match RawDisk::create(MemBackend::new(), capacity) {
        Ok(d) => d,
        Err(_) => return,
    };
    let drive = match AtaDrive::new(Box::new(disk) as Box<dyn VirtualDisk>) {
        Ok(d) => d,
        Err(_) => return,
    };

    let mut dev = AhciPciDevice::new(1);
    dev.attach_drive(0, drive);

    // Enable PCI bus mastering so `AhciPciDevice::process()` will run DMA.
    // Also set memory space decode (bit 1) for realism.
    dev.config_mut().set_command((1 << 1) | (1 << 2));

    // Bias the location of CLB/FB/CTBA to be inside guest RAM.
    let clb_seed: u64 = u.arbitrary().unwrap_or(0);
    let fb_seed: u64 = u.arbitrary().unwrap_or(0);
    let ctba_seed: u64 = u.arbitrary().unwrap_or(0);
    let buf_seed: u64 = u.arbitrary().unwrap_or(0);

    // Keep transfers <= 8 sectors (4KiB) to keep fuzz runs fast.
    let sector_count_seed: u8 = u.arbitrary().unwrap_or(1);
    let sector_count = (sector_count_seed % 8).max(1) as usize;
    let byte_len = sector_count * SECTOR_SIZE;

    let prdtl_seed: u8 = u.arbitrary().unwrap_or(1);
    let prdtl = (prdtl_seed as u16 % 8).max(1);

    let cmd_sel: u8 = u.arbitrary().unwrap_or(0);
    let cmd = match cmd_sel % 6 {
        0 => ATA_CMD_IDENTIFY,
        1 => ATA_CMD_READ_DMA_EXT,
        2 => ATA_CMD_WRITE_DMA_EXT,
        3 => ATA_CMD_FLUSH_CACHE,
        4 => ATA_CMD_FLUSH_CACHE_EXT,
        _ => ATA_CMD_SET_FEATURES,
    };

    let slot_seed: u8 = u.arbitrary().unwrap_or(0);
    let slot: u32 = (slot_seed % 32) as u32;

    // Regions.
    let clb = place_region(clb_seed, mem_size, 1024, 1024);
    let fb = place_region(fb_seed, mem_size, 256, 256);
    let ctba_size = 0x80usize + (prdtl as usize) * 16 + 64;
    let ctba = place_region(ctba_seed, mem_size, 128, ctba_size);
    let buf_base = place_region(buf_seed, mem_size, 4, byte_len);

    // Program registers (in-range) to reach deeper parsing logic.
    let port_base = PORT_BASE;
    dev.mmio_write(port_base + PORT_REG_CLB, 4, clb as u64);
    dev.mmio_write(port_base + PORT_REG_CLBU, 4, (clb >> 32) as u64);
    dev.mmio_write(port_base + PORT_REG_FB, 4, fb as u64);
    dev.mmio_write(port_base + PORT_REG_FBU, 4, (fb >> 32) as u64);

    // Keep AE set so the controller is in AHCI mode; enable IE, and ensure ST/FRE are set so the
    // command engine runs.
    dev.mmio_write(HBA_REG_GHC, 4, (GHC_IE | GHC_AE) as u64);
    dev.mmio_write(port_base + PORT_REG_IE, 4, PORT_IS_DHRS as u64);
    dev.mmio_write(
        port_base + PORT_REG_CMD,
        4,
        (PORT_CMD_ST | PORT_CMD_FRE) as u64,
    );

    // Overlay a minimally well-formed command header/table so we exercise command parsing + PRDT
    // DMA scatter/gather logic.
    let is_write = cmd == ATA_CMD_WRITE_DMA_EXT;
    write_cmd_header(&mut mem, clb, slot as usize, ctba, prdtl, is_write);

    let mut cfis = [0u8; 64];
    // Register Host-to-Device FIS.
    cfis[0] = 0x27; // FIS_TYPE_REG_H2D
    cfis[1] = 0x80; // C bit (command)
    cfis[2] = cmd;
    cfis[7] = 0x40; // LBA mode

    // LBA48 (bounded).
    let lba_seed: u64 = u.arbitrary().unwrap_or(0);
    let max_lba = capacity / SECTOR_SIZE as u64;
    let lba = if max_lba == 0 { 0 } else { lba_seed % max_lba };
    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;

    // Sector count (avoid 0 => 65536 semantics).
    cfis[12] = (sector_count & 0xFF) as u8;
    cfis[13] = ((sector_count >> 8) & 0xFF) as u8;

    // SET FEATURES subcommand is in Features (low byte).
    cfis[3] = u.arbitrary().unwrap_or(0);
    write_cfis(&mut mem, ctba, &cfis);

    // PRDT entries cover the transfer buffer.
    let mut remaining = byte_len;
    let mut cur = buf_base;
    for entry in 0..(prdtl as usize) {
        if remaining == 0 {
            write_prdt(&mut mem, ctba, entry, cur, 1);
            continue;
        }
        let chunk = remaining.min(4096);
        write_prdt(&mut mem, ctba, entry, cur, chunk as u32);
        cur = cur.wrapping_add(chunk as u64);
        remaining -= chunk;
    }

    // Issue the command.
    dev.mmio_write(port_base + PORT_REG_CI, 4, (1u32 << slot) as u64);
    dev.process(&mut mem);
});

