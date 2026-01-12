#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices::pci::capabilities::PCI_CONFIG_SPACE_SIZE;
use aero_devices::pci::PciDevice;
use aero_devices_storage::ata::{
    AtaDrive, ATA_CMD_FLUSH_CACHE, ATA_CMD_FLUSH_CACHE_EXT, ATA_CMD_IDENTIFY, ATA_CMD_READ_DMA_EXT,
    ATA_CMD_SET_FEATURES, ATA_CMD_WRITE_DMA_EXT,
};
use aero_devices_storage::AhciPciDevice;
use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE, VirtualDisk};
use memory::MemoryBus;

// AHCI register offsets/bits (mirrors `aero-devices-storage/src/ahci.rs`).
const HBA_REG_CAP: u64 = 0x00;
const HBA_REG_GHC: u64 = 0x04;
const HBA_REG_IS: u64 = 0x08;
const HBA_REG_PI: u64 = 0x0C;
const HBA_REG_VS: u64 = 0x10;
const HBA_REG_CAP2: u64 = 0x24;
const HBA_REG_BOHC: u64 = 0x28;

const PORT_BASE: u64 = 0x100;

const PORT_REG_CLB: u64 = 0x00;
const PORT_REG_CLBU: u64 = 0x04;
const PORT_REG_FB: u64 = 0x08;
const PORT_REG_FBU: u64 = 0x0C;
const PORT_REG_IS: u64 = 0x10;
const PORT_REG_IE: u64 = 0x14;
const PORT_REG_CMD: u64 = 0x18;
const PORT_REG_CI: u64 = 0x38;

const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;

const PORT_IS_DHRS: u32 = 1 << 0;

/// Bounded guest-physical memory for AHCI DMA fuzzing.
///
/// Reads outside the provided buffer return zeros; writes are dropped.
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

fn decode_size(bits: u8) -> usize {
    match bits % 4 {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    }
}

fn build_known_offsets(port_base: u64) -> Vec<u64> {
    let mut offsets = vec![
        HBA_REG_CAP,
        HBA_REG_GHC,
        HBA_REG_IS,
        HBA_REG_PI,
        HBA_REG_VS,
        HBA_REG_CAP2,
        HBA_REG_BOHC,
    ];
    offsets.extend_from_slice(&[
        port_base + PORT_REG_CLB,
        port_base + PORT_REG_CLBU,
        port_base + PORT_REG_FB,
        port_base + PORT_REG_FBU,
        port_base + PORT_REG_IS,
        port_base + PORT_REG_IE,
        port_base + PORT_REG_CMD,
        port_base + PORT_REG_CI,
    ]);
    offsets
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

#[derive(Clone, Copy)]
enum MmioOp {
    Read { offset: u64, size: usize },
    Write { offset: u64, size: usize, value: u64 },
    ConfigRead { offset: u16, size: usize },
    ConfigWrite { offset: u16, size: usize, value: u32 },
    Process,
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // 64KiB..=1MiB guest memory backing.
    let pow: u8 = u.int_in_range(0u8..=4).unwrap_or(0);
    let mem_size: usize = (64usize * 1024) << pow;

    // 1..=4 ports to exercise multi-port decode logic without blowing up fuzz runtime.
    let num_ports: usize = u.int_in_range(1usize..=4).unwrap_or(1);
    let port_sel_seed: u8 = u.arbitrary().unwrap_or(0);
    let port_idx: usize = if num_ports == 0 {
        0
    } else {
        (port_sel_seed as usize) % num_ports
    };
    let port_base = PORT_BASE + (port_idx as u64) * 0x80;
    let known_offsets = build_known_offsets(port_base);

    // Seeds used to bias the location of command list/FIS/table/buffers.
    let clb_seed: u64 = u.arbitrary().unwrap_or(0);
    let fb_seed: u64 = u.arbitrary().unwrap_or(0);
    let ctba_seed: u64 = u.arbitrary().unwrap_or(0);
    let data_seed: u64 = u.arbitrary().unwrap_or(0);

    let ghc_seed: u32 = u.arbitrary().unwrap_or(0);
    let port_ie_seed: u32 = u.arbitrary().unwrap_or(0);
    let port_cmd_seed: u32 = u.arbitrary().unwrap_or(0);
    let port_ci_seed: u32 = u.arbitrary().unwrap_or(0);

    // Command synthesis controls.
    let synthesize: bool = u.arbitrary().unwrap_or(true);
    let cmd_sel: u8 = u.arbitrary().unwrap_or(0);
    let slot_seed: u8 = u.arbitrary().unwrap_or(0);
    let lba_seed2: u64 = u.arbitrary().unwrap_or(0);
    let sector_count_seed: u8 = u.arbitrary().unwrap_or(1);
    let prdtl_seed: u8 = u.arbitrary().unwrap_or(1);
    let set_features_subcmd: u8 = u.arbitrary().unwrap_or(0);
    let wild_after: bool = u.arbitrary().unwrap_or(false);

    let ops_len: usize = u.int_in_range(0usize..=64).unwrap_or(0);
    let mut ops = Vec::with_capacity(ops_len);
    for _ in 0..ops_len {
        let kind: u8 = u.arbitrary().unwrap_or(0);
        match kind % 5 {
            0 => {
                // Bias towards known register offsets to reach deeper state transitions.
                let use_known: bool = u.arbitrary().unwrap_or(true);
                let offset = if use_known && !known_offsets.is_empty() {
                    let idx = u
                        .int_in_range(0usize..=known_offsets.len().saturating_sub(1))
                        .unwrap_or(0);
                    known_offsets[idx]
                } else {
                    let offset: u16 = u.arbitrary().unwrap_or(0);
                    offset as u64
                };
                let size = decode_size(u.arbitrary().unwrap_or(0));
                ops.push(MmioOp::Read { offset, size });
            }
            1 => {
                let use_known: bool = u.arbitrary().unwrap_or(true);
                let offset = if use_known && !known_offsets.is_empty() {
                    let idx = u
                        .int_in_range(0usize..=known_offsets.len().saturating_sub(1))
                        .unwrap_or(0);
                    known_offsets[idx]
                } else {
                    let offset: u16 = u.arbitrary().unwrap_or(0);
                    offset as u64
                };
                let size = decode_size(u.arbitrary().unwrap_or(0));
                let value: u64 = u.arbitrary().unwrap_or(0);
                ops.push(MmioOp::Write { offset, size, value });
            }
            2 => {
                // PCI config read.
                let size = match u.arbitrary::<u8>().unwrap_or(0) % 3 {
                    0 => 1,
                    1 => 2,
                    _ => 4,
                };
                let max_off = (PCI_CONFIG_SPACE_SIZE - size) as u16;
                let seed: u16 = u.arbitrary().unwrap_or(0);
                let offset = if max_off == 0 { 0 } else { seed % (max_off + 1) };
                ops.push(MmioOp::ConfigRead { offset, size });
            }
            3 => {
                // PCI config write. Keep accesses valid to avoid asserting in the PCI config
                // framework (e.g. BAR writes require aligned dword accesses).
                let which: u8 = u.arbitrary().unwrap_or(0);
                if which % 2 == 0 {
                    // Command register (0x04..0x05). Allow 1/2/4-byte writes.
                    let size = match u.arbitrary::<u8>().unwrap_or(0) % 3 {
                        0 => 1,
                        1 => 2,
                        _ => 4,
                    };
                    let mut value: u32 = u.arbitrary().unwrap_or(0);
                    // Bias towards keeping MEM + BUSMASTER enabled so we still reach deeper DMA paths.
                    let force_enable: bool = u.arbitrary().unwrap_or(true);
                    if force_enable {
                        value |= (1 << 1) | (1 << 2);
                    }
                    ops.push(MmioOp::ConfigWrite {
                        offset: 0x04,
                        size,
                        value,
                    });
                } else {
                    // BAR5 (ABAR) dword write at 0x24 (0x10 + 5*4).
                    let value: u32 = u.arbitrary().unwrap_or(0);
                    ops.push(MmioOp::ConfigWrite {
                        offset: 0x10 + 5 * 4,
                        size: 4,
                        value,
                    });
                }
            }
            _ => ops.push(MmioOp::Process),
        }
    }

    // Remaining bytes initialize guest RAM.
    let rest_len = u.len();
    let init = u.bytes(rest_len).unwrap_or(&[]);
    let mut mem = FuzzBus::new(mem_size, init);

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

    let mut dev = AhciPciDevice::new(num_ports);
    dev.attach_drive(port_idx, drive);

    // Enable PCI bus mastering so `AhciPciDevice::process()` will run DMA.
    // Also set memory space decode (bit 1) for realism.
    dev.config_mut().set_command((1 << 1) | (1 << 2));

    // Program registers with fuzz-controlled values (kept within our guest memory blob) so we can
    // reach deeper parsing logic.
    let clb = place_region(clb_seed, mem_size, 1024, 1024);
    let fb = place_region(fb_seed, mem_size, 256, 256);

    dev.mmio_write(port_base + PORT_REG_CLB, 4, clb as u64);
    dev.mmio_write(port_base + PORT_REG_CLBU, 4, (clb >> 32) as u64);
    dev.mmio_write(port_base + PORT_REG_FB, 4, fb as u64);
    dev.mmio_write(port_base + PORT_REG_FBU, 4, (fb >> 32) as u64);

    // Keep AE set so the controller is in AHCI mode; fuzz the rest.
    dev.mmio_write(
        HBA_REG_GHC,
        4,
        ((ghc_seed & (GHC_IE | GHC_AE)) | GHC_AE) as u64,
    );
    dev.mmio_write(
        port_base + PORT_REG_IE,
        4,
        (port_ie_seed | PORT_IS_DHRS) as u64,
    );
    dev.mmio_write(
        port_base + PORT_REG_CMD,
        4,
        (port_cmd_seed | PORT_CMD_ST | PORT_CMD_FRE) as u64,
    );

    // Optionally overlay a minimally well-formed command list/table so we exercise deeper parsing.
    if synthesize {
        let prdtl = (prdtl_seed as u16 % 8).max(1);
        let slot: u32 = (slot_seed % 32) as u32;

        let cmd = match cmd_sel % 6 {
            0 => ATA_CMD_IDENTIFY,
            1 => ATA_CMD_READ_DMA_EXT,
            2 => ATA_CMD_WRITE_DMA_EXT,
            3 => ATA_CMD_FLUSH_CACHE,
            4 => ATA_CMD_FLUSH_CACHE_EXT,
            _ => ATA_CMD_SET_FEATURES,
        };

        // Keep transfers <= 8 sectors (4KiB) to keep fuzz runs fast.
        let sector_count = ((sector_count_seed % 8) as u32).max(1);
        let byte_len = sector_count as usize * SECTOR_SIZE;

        let ctba_size = 0x80usize + (prdtl as usize) * 16 + 64;
        let ctba = place_region(ctba_seed, mem_size, 128, ctba_size);
        let data_base = place_region(data_seed, mem_size, 4, byte_len);

        let is_write = cmd == ATA_CMD_WRITE_DMA_EXT;
        write_cmd_header(&mut mem, clb, slot as usize, ctba, prdtl, is_write);

        let mut cfis = [0u8; 64];
        cfis[0] = 0x27; // FIS_TYPE_REG_H2D
        cfis[1] = 0x80; // C bit (command)
        cfis[2] = cmd;
        cfis[7] = 0x40; // LBA mode

        // LBA48.
        let max_lba = capacity / SECTOR_SIZE as u64;
        let lba = if max_lba == 0 { 0 } else { lba_seed2 % max_lba };
        cfis[4] = (lba & 0xFF) as u8;
        cfis[5] = ((lba >> 8) & 0xFF) as u8;
        cfis[6] = ((lba >> 16) & 0xFF) as u8;
        cfis[8] = ((lba >> 24) & 0xFF) as u8;
        cfis[9] = ((lba >> 32) & 0xFF) as u8;
        cfis[10] = ((lba >> 40) & 0xFF) as u8;

        cfis[12] = (sector_count & 0xFF) as u8;
        cfis[13] = ((sector_count >> 8) & 0xFF) as u8;

        // SET FEATURES subcommand is in Features (low byte).
        cfis[3] = set_features_subcmd;

        write_cfis(&mut mem, ctba, &cfis);

        // PRDT entries cover the transfer buffer.
        let mut remaining = byte_len;
        let mut cur = data_base;
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

        // Ensure at least one command is issued.
        dev.mmio_write(port_base + PORT_REG_CI, 4, (1u32 << slot) as u64);
    } else {
        dev.mmio_write(port_base + PORT_REG_CI, 4, (port_ci_seed & 0xF) as u64);
    }

    // Execute a small mixed sequence of MMIO reads/writes and processing ticks.
    for op in ops {
        match op {
            MmioOp::Read { offset, size } => {
                let _ = dev.mmio_read(offset, size);
            }
            MmioOp::Write {
                offset,
                size,
                value,
            } => dev.mmio_write(offset, size, value),
            MmioOp::ConfigRead { offset, size } => {
                let _ = dev.config_mut().read(offset, size);
            }
            MmioOp::ConfigWrite {
                offset,
                size,
                value,
            } => {
                let _ = dev.config_mut().write_with_effects(offset, size, value);
            }
            MmioOp::Process => dev.process(&mut mem),
        }
    }

    // Optionally reprogram CLB/FB to arbitrary (potentially hostile) addresses and issue a command
    // in a random slot, to stress address arithmetic/overflow paths without sacrificing the deep
    // "in-range" command parsing coverage from the earlier synthesis.
    if wild_after {
        let wild_slot: u32 = (slot_seed.rotate_left(1) % 32) as u32;
        let wild_ci: u32 = port_ci_seed | (1u32 << wild_slot) | 1;

        dev.mmio_write(port_base + PORT_REG_CLB, 4, clb_seed);
        dev.mmio_write(port_base + PORT_REG_CLBU, 4, (clb_seed >> 32) as u64);
        dev.mmio_write(port_base + PORT_REG_FB, 4, fb_seed);
        dev.mmio_write(port_base + PORT_REG_FBU, 4, (fb_seed >> 32) as u64);

        // Ensure the command engine is enabled when we tick.
        dev.mmio_write(
            HBA_REG_GHC,
            4,
            ((ghc_seed & (GHC_IE | GHC_AE)) | GHC_AE) as u64,
        );
        dev.mmio_write(
            port_base + PORT_REG_CMD,
            4,
            (port_cmd_seed | PORT_CMD_ST | PORT_CMD_FRE) as u64,
        );
        dev.mmio_write(port_base + PORT_REG_CI, 4, wild_ci as u64);
    }

    // Always process at least once so a synthesized command can't be "optimized away".
    // Re-enable PCI bus mastering (and MMIO decode) so we still execute the command list even if
    // earlier fuzzed PCI config writes disabled DMA.
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev.process(&mut mem);
});
