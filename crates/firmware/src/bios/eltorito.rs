//! El Torito (bootable CD-ROM) support.
//!
//! This module implements enough of the El Torito and ISO9660 structures to locate and load the
//! initial/default no-emulation boot entry from a typical Windows install ISO.

use super::{BlockDevice, DiskError};

const ISO9660_STANDARD_IDENTIFIER: &[u8; 5] = b"CD001";
const ISO9660_VERSION: u8 = 1;

// The boot system ID in the ISO9660 boot record volume descriptor is space-padded.
const EL_TORITO_BOOT_SYSTEM_ID_SPACES: [u8; 32] = {
    let mut out = [b' '; 32];
    let src = *b"EL TORITO SPECIFICATION";
    let mut i = 0;
    while i < src.len() {
        out[i] = src[i];
        i += 1;
    }
    out
};

const ISO_BLOCK_BYTES: usize = 2048;
const BIOS_SECTOR_BYTES: usize = 512;
const BIOS_SECTORS_PER_ISO_BLOCK: u64 = (ISO_BLOCK_BYTES / BIOS_SECTOR_BYTES) as u64; // 4

/// Fields needed to load and jump to a no-emulation El Torito boot image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BootImageInfo {
    /// Real-mode segment where the boot image should be loaded/executed.
    pub(super) load_segment: u16,
    /// Number of 512-byte virtual sectors to load.
    pub(super) sector_count: u16,
    /// 2048-byte logical block address of the boot image in the ISO.
    pub(super) load_rba: u32,
}

pub(super) fn parse_boot_image(disk: &mut dyn BlockDevice) -> Result<BootImageInfo, &'static str> {
    let boot_catalog_lba = find_boot_catalog_lba(disk)?;
    parse_boot_catalog(disk, boot_catalog_lba)
}

fn find_boot_catalog_lba(disk: &mut dyn BlockDevice) -> Result<u32, &'static str> {
    // ISO9660 volume descriptor set begins at logical block 16.
    for iso_lba in 16u32.. {
        let mut block = [0u8; ISO_BLOCK_BYTES];
        read_iso_block(disk, iso_lba, &mut block).map_err(|_| "Disk read error")?;

        let typ = block[0];
        if typ == 0xFF {
            break;
        }

        if &block[1..6] != ISO9660_STANDARD_IDENTIFIER || block[6] != ISO9660_VERSION {
            // Not an ISO9660 volume descriptor set (or corrupt).
            break;
        }

        if typ != 0x00 {
            continue;
        }

        // Boot Record Volume Descriptor (ISO9660 type 0).
        let boot_system_id = &block[7..39];
        if boot_system_id != EL_TORITO_BOOT_SYSTEM_ID_SPACES {
            continue;
        }

        // Boot Catalog LBA (little-endian u32 at offset 0x47).
        let lba_bytes: [u8; 4] = block[0x47..0x4B].try_into().unwrap();
        return Ok(u32::from_le_bytes(lba_bytes));
    }

    Err("Missing El Torito boot record")
}

fn parse_boot_catalog(
    disk: &mut dyn BlockDevice,
    boot_catalog_lba: u32,
) -> Result<BootImageInfo, &'static str> {
    let mut block = [0u8; ISO_BLOCK_BYTES];
    read_iso_block(disk, boot_catalog_lba, &mut block).map_err(|_| "Disk read error")?;

    let validation_entry = &block[0..32];
    validate_catalog_validation_entry(validation_entry)?;

    let initial_entry = &block[32..64];
    parse_initial_entry(initial_entry)
}

fn validate_catalog_validation_entry(entry: &[u8]) -> Result<(), &'static str> {
    if entry.len() != 32 {
        return Err("Invalid El Torito boot catalog");
    }
    if entry[0] != 0x01 {
        return Err("Invalid El Torito boot catalog");
    }
    if entry[0x1E] != 0x55 || entry[0x1F] != 0xAA {
        return Err("Invalid El Torito boot catalog");
    }

    // Checksum over 16-bit words must sum to 0.
    let mut sum: u16 = 0;
    for chunk in entry.chunks_exact(2) {
        sum = sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    if sum != 0 {
        return Err("Invalid El Torito boot catalog checksum");
    }

    Ok(())
}

fn parse_initial_entry(entry: &[u8]) -> Result<BootImageInfo, &'static str> {
    if entry.len() != 32 {
        return Err("Invalid El Torito boot catalog");
    }

    let boot_indicator = entry[0];
    if boot_indicator != 0x88 {
        return Err("El Torito boot image is not bootable");
    }

    let media_type = entry[1];
    if media_type != 0 {
        return Err("Unsupported El Torito boot media type");
    }

    let load_segment = u16::from_le_bytes(entry[4..6].try_into().unwrap());
    let load_segment = if load_segment == 0 { 0x07C0 } else { load_segment };
    let sector_count = u16::from_le_bytes(entry[6..8].try_into().unwrap());
    let load_rba = u32::from_le_bytes(entry[8..12].try_into().unwrap());

    Ok(BootImageInfo {
        load_segment,
        sector_count,
        load_rba,
    })
}

fn read_iso_block(
    disk: &mut dyn BlockDevice,
    iso_lba: u32,
    out: &mut [u8; ISO_BLOCK_BYTES],
) -> Result<(), DiskError> {
    let base = u64::from(iso_lba)
        .checked_mul(BIOS_SECTORS_PER_ISO_BLOCK)
        .ok_or(DiskError::OutOfRange)?;

    for i in 0..BIOS_SECTORS_PER_ISO_BLOCK {
        let mut sector = [0u8; BIOS_SECTOR_BYTES];
        disk.read_sector(base + i, &mut sector)?;
        let off = (i as usize) * BIOS_SECTOR_BYTES;
        out[off..off + BIOS_SECTOR_BYTES].copy_from_slice(&sector);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bios::{Bios, BiosConfig, InMemoryDisk, TestMemory};
    use aero_cpu_core::state::{gpr, CpuMode, CpuState};

    fn write_iso_block(img: &mut [u8], iso_lba: usize, block: &[u8; ISO_BLOCK_BYTES]) {
        let off = iso_lba * ISO_BLOCK_BYTES;
        img[off..off + ISO_BLOCK_BYTES].copy_from_slice(block);
    }

    fn build_minimal_iso_no_emulation(
        boot_catalog_lba: u32,
        boot_image_lba: u32,
        boot_image_bytes: &[u8; ISO_BLOCK_BYTES],
        load_segment: u16,
        sector_count: u16,
    ) -> Vec<u8> {
        // Allocate enough blocks for the volume descriptors + boot catalog + boot image.
        let total_blocks = (boot_image_lba as usize).saturating_add(1).max(32);
        let mut img = vec![0u8; total_blocks * ISO_BLOCK_BYTES];

        // Primary Volume Descriptor at LBA16 (type 1).
        let mut pvd = [0u8; ISO_BLOCK_BYTES];
        pvd[0] = 0x01;
        pvd[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        pvd[6] = ISO9660_VERSION;
        write_iso_block(&mut img, 16, &pvd);

        // Boot Record Volume Descriptor at LBA17 (type 0).
        let mut brvd = [0u8; ISO_BLOCK_BYTES];
        brvd[0] = 0x00;
        brvd[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        brvd[6] = ISO9660_VERSION;
        brvd[7..39].copy_from_slice(&EL_TORITO_BOOT_SYSTEM_ID_SPACES);
        brvd[0x47..0x4B].copy_from_slice(&boot_catalog_lba.to_le_bytes());
        write_iso_block(&mut img, 17, &brvd);

        // Volume Descriptor Set Terminator at LBA18 (type 255).
        let mut term = [0u8; ISO_BLOCK_BYTES];
        term[0] = 0xFF;
        term[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        term[6] = ISO9660_VERSION;
        write_iso_block(&mut img, 18, &term);

        // Boot Catalog at `boot_catalog_lba`.
        let mut catalog = [0u8; ISO_BLOCK_BYTES];
        let mut validation = [0u8; 32];
        validation[0] = 0x01; // header id
        validation[0x1E] = 0x55;
        validation[0x1F] = 0xAA;
        let mut sum: u16 = 0;
        for chunk in validation.chunks_exact(2) {
            sum = sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        let checksum = (0u16).wrapping_sub(sum);
        validation[0x1C..0x1E].copy_from_slice(&checksum.to_le_bytes());
        // Sanity check: the checksum should make the sum come out to 0.
        let mut final_sum: u16 = 0;
        for chunk in validation.chunks_exact(2) {
            final_sum = final_sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        assert_eq!(final_sum, 0);
        catalog[0..32].copy_from_slice(&validation);

        let mut initial = [0u8; 32];
        initial[0] = 0x88; // bootable
        initial[1] = 0x00; // no emulation
        initial[4..6].copy_from_slice(&load_segment.to_le_bytes());
        initial[6..8].copy_from_slice(&sector_count.to_le_bytes());
        initial[8..12].copy_from_slice(&boot_image_lba.to_le_bytes());
        catalog[32..64].copy_from_slice(&initial);

        write_iso_block(&mut img, boot_catalog_lba as usize, &catalog);

        write_iso_block(&mut img, boot_image_lba as usize, boot_image_bytes);

        img
    }

    #[test]
    fn parse_boot_image_finds_default_no_emulation_entry() {
        let boot_catalog_lba = 20;
        let boot_image_lba = 21;
        let boot_image = [0xAB; ISO_BLOCK_BYTES];
        let img = build_minimal_iso_no_emulation(
            boot_catalog_lba,
            boot_image_lba,
            &boot_image,
            0,
            4,
        );
        let mut disk = InMemoryDisk::new(img);

        let info = parse_boot_image(&mut disk).expect("parser should succeed");
        assert_eq!(
            info,
            BootImageInfo {
                load_segment: 0x07C0,
                sector_count: 4,
                load_rba: boot_image_lba,
            }
        );
    }

    #[test]
    fn bios_post_loads_eltorito_boot_image_and_sets_entry_state() {
        let boot_catalog_lba = 20;
        let boot_image_lba = 21;
        let boot_image = [0x5A; ISO_BLOCK_BYTES];
        let img = build_minimal_iso_no_emulation(
            boot_catalog_lba,
            boot_image_lba,
            &boot_image,
            0,
            4,
        );

        let mut bios = Bios::new(BiosConfig {
            memory_size_bytes: 16 * 1024 * 1024,
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::new(img);

        bios.post(&mut cpu, &mut mem, &mut disk);

        let load_addr = (0x07C0u64) << 4;
        let loaded = mem.read_bytes(load_addr, ISO_BLOCK_BYTES);
        assert_eq!(loaded, vec![0x5A; ISO_BLOCK_BYTES]);

        assert_eq!(cpu.segments.cs.selector, 0x07C0);
        assert_eq!(cpu.rip(), 0);
        assert_eq!(cpu.gpr[gpr::RSP] as u16, 0x7C00);
        assert_eq!(cpu.gpr[gpr::RDX] as u8, 0xE0);
        assert_eq!(cpu.segments.ds.selector, 0);
        assert_eq!(cpu.segments.es.selector, 0);
        assert_eq!(cpu.segments.ss.selector, 0);
    }
}
