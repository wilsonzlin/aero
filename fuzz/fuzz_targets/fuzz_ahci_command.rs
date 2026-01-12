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

const MAX_PRDTL: u16 = 32;
const MAX_SECTORS: u16 = 8;

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

    // Fixed-size RAM to keep allocations bounded and deterministic.
    let mem_size = 256usize * 1024;

    // Control fields are consumed first so the remaining bytes can be used as "guest RAM" backing.
    let clb_seed: u64 = u.arbitrary().unwrap_or(0);
    let fb_seed: u64 = u.arbitrary().unwrap_or(0);
    let ctba_seed: u64 = u.arbitrary().unwrap_or(0);
    let slot_seed: u8 = u.arbitrary().unwrap_or(0);
    let header_flags_seed: u32 = u.arbitrary().unwrap_or(0);
    let cmd_seed: u8 = u.arbitrary().unwrap_or(0);
    let feature_low: u8 = u.arbitrary().unwrap_or(0);
    let lba_seed: u64 = u.arbitrary().unwrap_or(0);
    let sectors_seed: u16 = u.arbitrary().unwrap_or(1);
    let prdtl_seed: u16 = u.arbitrary().unwrap_or(0);
    let force_valid_fis: bool = u.arbitrary().unwrap_or(true);
    let mmio_ops: u8 = u.int_in_range(0u8..=8).unwrap_or(0);
    let mut ops: Vec<(u16, u32)> = Vec::with_capacity(mmio_ops as usize);
    for _ in 0..mmio_ops {
        let off: u16 = u.arbitrary().unwrap_or(0);
        let val: u32 = u.arbitrary().unwrap_or(0);
        ops.push((off, val));
    }

    let mut mem = Bus::new(mem_size);

    // Seed guest RAM from the remaining bytes so the fuzzer can directly influence descriptor
    // tables (command list/table + PRDT) and DMA buffers.
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
    let clb = place_region(clb_seed, mem_size, 1024, 1024);
    let fb = place_region(fb_seed, mem_size, 256, 256);

    // Keep transfers <= 8 sectors (4KiB) to keep fuzz runs fast.
    let sectors = (sectors_seed % MAX_SECTORS).max(1);

    // Choose a command value; bias toward known commands so we reach deeper DMA/PRDT paths, but
    // still allow unknown commands for negative testing.
    let cmd = if (cmd_seed & 0x80) == 0 {
        match cmd_seed % 6 {
            0 => ATA_CMD_IDENTIFY,
            1 => ATA_CMD_READ_DMA_EXT,
            2 => ATA_CMD_WRITE_DMA_EXT,
            3 => ATA_CMD_FLUSH_CACHE,
            4 => ATA_CMD_FLUSH_CACHE_EXT,
            _ => ATA_CMD_SET_FEATURES,
        }
    } else {
        cmd_seed
    };

    let expected_len = match cmd {
        ATA_CMD_IDENTIFY => 512usize,
        ATA_CMD_READ_DMA_EXT | ATA_CMD_WRITE_DMA_EXT => (sectors as usize) * SECTOR_SIZE,
        _ => 0usize,
    };

    let mut prdtl = (prdtl_seed % (MAX_PRDTL + 1)) as u16; // 0..=MAX_PRDTL
    // If the command expects a DMA buffer, bias towards a non-empty PRDT so we actually parse it.
    if expected_len != 0 && prdtl == 0 {
        prdtl = 1;
    }

    let slot: u32 = (slot_seed % 32) as u32;

    // Command table region large enough for CFIS + PRDT.
    let ctba_size = 0x80usize + (prdtl as usize) * 16 + 64;
    let ctba = place_region(ctba_seed, mem_size, 128, ctba_size);

    // Program registers (in-range) to reach deeper parsing logic.
    let port_base = PORT_BASE;

    // Apply a small number of fuzzer-driven MMIO writes first (to stress register decode paths),
    // then overwrite the critical registers below so we can reach command execution.
    for (off, val) in ops {
        dev.mmio_write(off as u64, 4, val as u64);
    }

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

    // Overlay *only* the critical fields in the command header so:
    // - CTBA stays in-range (avoids early-exit from unmapped CFIS reads),
    // - PRDTL stays bounded (avoids pathological loops),
    // while still leaving other bytes attacker-controlled via the seeded guest RAM.
    let header_addr = clb + (slot as u64) * 32;
    let flags_low = header_flags_seed & 0x0000_FFFF;
    let flags = flags_low | ((prdtl as u32) << 16);
    mem.write_u32(header_addr, flags);
    mem.write_u32(header_addr + 4, 0); // PRDBC
    mem.write_u32(header_addr + 8, ctba as u32);
    mem.write_u32(header_addr + 12, (ctba >> 32) as u32);

    // Patch the CFIS in-place to keep internal allocations bounded and to ensure we frequently
    // reach the deeper command parsing and PRDT/DMA paths.
    if force_valid_fis {
        mem.write_u8(ctba, 0x27);
        let b1 = mem.read_u8(ctba + 1);
        mem.write_u8(ctba + 1, b1 | 0x80);
    }
    mem.write_u8(ctba + 2, cmd);
    mem.write_u8(ctba + 3, feature_low);
    let dev_byte = mem.read_u8(ctba + 7);
    mem.write_u8(ctba + 7, dev_byte | 0x40);

    // Clamp LBA so successful DMA reads/writes occur frequently (helps coverage).
    let max_lba = capacity / SECTOR_SIZE as u64;
    let lba = if max_lba == 0 { 0 } else { lba_seed % max_lba };
    mem.write_u8(ctba + 4, (lba & 0xFF) as u8);
    mem.write_u8(ctba + 5, ((lba >> 8) & 0xFF) as u8);
    mem.write_u8(ctba + 6, ((lba >> 16) & 0xFF) as u8);
    mem.write_u8(ctba + 8, ((lba >> 24) & 0xFF) as u8);
    mem.write_u8(ctba + 9, ((lba >> 32) & 0xFF) as u8);
    mem.write_u8(ctba + 10, ((lba >> 40) & 0xFF) as u8);

    // Clamp sector count away from 0 to avoid the ATA semantics of 0 => 65536 sectors.
    mem.write_u8(ctba + 12, (sectors & 0xFF) as u8);
    mem.write_u8(ctba + 13, ((sectors >> 8) & 0xFF) as u8);

    // Issue the command.
    dev.mmio_write(port_base + PORT_REG_CI, 4, (1u32 << slot) as u64);
    dev.process(&mut mem);
});
