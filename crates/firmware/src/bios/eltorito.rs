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

/// Default no-emulation load segment per the El Torito spec when the catalog field is zero.
const DEFAULT_LOAD_SEGMENT: u16 = 0x07C0;

/// Default "boot load size" used when the El Torito sector count field is zero.
///
/// Some ISO authoring tools omit `-boot-load-size`, producing an initial/default entry with
/// `sector_count=0`. The El Torito spec defines a default of 4 512-byte sectors (2048 bytes) for
/// this case, and Windows install media commonly relies on this default for `etfsboot.com`.
const DEFAULT_SECTOR_COUNT: u16 = 4;

/// Fields needed to load and jump to a no-emulation El Torito boot image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BootImageInfo {
    /// 2048-byte logical block address of the boot catalog in the ISO.
    pub(super) boot_catalog_lba: u32,
    /// Real-mode segment where the boot image should be loaded/executed.
    pub(super) load_segment: u16,
    /// Number of 512-byte virtual sectors to load.
    pub(super) sector_count: u16,
    /// 2048-byte logical block address of the boot image in the ISO.
    pub(super) load_rba: u32,
}

/// Parsed/default El Torito boot image selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ParsedBootImage {
    /// 2048-byte logical block address of the El Torito boot catalog.
    pub(super) boot_catalog_lba: u32,
    /// Initial/default boot image entry fields (no-emulation).
    pub(super) image: BootImageInfo,
}

pub(super) fn parse_boot_image(disk: &mut dyn BlockDevice) -> Result<ParsedBootImage, &'static str> {
    let boot_catalog_lba = find_boot_catalog_lba(disk)?;
    let image = parse_boot_catalog(disk, boot_catalog_lba)?;

    // Validate that the boot image fits within the underlying disk. This is a hard safety check so
    // we do not attempt out-of-range reads on corrupt catalogs.
    let start_lba_512 = u64::from(image.load_rba)
        .checked_mul(BIOS_SECTORS_PER_ISO_BLOCK)
        .ok_or("El Torito boot image load past end-of-image")?;
    let end_lba_512 = start_lba_512
        .checked_add(u64::from(image.sector_count))
        .ok_or("El Torito boot image load past end-of-image")?;
    if end_lba_512 > disk.size_in_sectors() {
        return Err("El Torito boot image load past end-of-image");
    }

    Ok(ParsedBootImage {
        boot_catalog_lba,
        image,
    })
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
            // Some ISO authoring tools pad with NUL bytes rather than spaces; tolerate both while
            // still requiring the canonical identifier prefix.
            const EL_TORITO_ID: &[u8] = b"EL TORITO SPECIFICATION";
            if !boot_system_id.starts_with(EL_TORITO_ID)
                || boot_system_id[EL_TORITO_ID.len()..]
                    .iter()
                    .any(|&b| b != 0 && b != b' ')
            {
                continue;
            }
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
    // Read a bounded slice of the catalog so we can scan for a usable entry without assuming it
    // lives in the first 2048 bytes.
    const MAX_CATALOG_BLOCKS: u32 = 4;

    let total_blocks = disk.size_in_sectors() / BIOS_SECTORS_PER_ISO_BLOCK;
    let available_blocks = total_blocks.saturating_sub(u64::from(boot_catalog_lba));
    let blocks_to_read =
        u32::try_from(available_blocks.min(u64::from(MAX_CATALOG_BLOCKS)).max(1))
            .unwrap_or(1);

    let mut catalog = vec![0u8; (blocks_to_read as usize) * ISO_BLOCK_BYTES];
    for i in 0..blocks_to_read {
        let mut block = [0u8; ISO_BLOCK_BYTES];
        read_iso_block(disk, boot_catalog_lba + i, &mut block).map_err(|_| "Disk read error")?;
        let start = (i as usize) * ISO_BLOCK_BYTES;
        catalog[start..start + ISO_BLOCK_BYTES].copy_from_slice(&block);
    }

    let validation_entry = &catalog[0..32];
    validate_catalog_validation_entry(validation_entry)?;

    // The initial/default entry uses the platform id from the validation entry; section headers can
    // override this for subsequent entries.
    let mut current_platform_id = validation_entry[1];

    // Scan entries starting at #1. Keep the common case fast by checking entry #1 first, then
    // continuing.
    for entry in catalog.chunks_exact(32).skip(1) {
        match entry[0] {
            // Section header (header indicator).
            0x90 | 0x91 => {
                current_platform_id = entry[1];
                continue;
            }
            // Boot entry (boot indicator).
            0x00 | 0x88 => {
                // Only support BIOS/x86 platform.
                if current_platform_id != 0 {
                    continue;
                }

                // Bootable + no-emulation.
                if entry[0] != 0x88 || entry[1] != 0 {
                    continue;
                }

                return parse_boot_entry(entry, boot_catalog_lba);
            }
            // Extension/unknown entries: ignore for robustness.
            _ => continue,
        }
    }

    Err("El Torito boot catalog contains no bootable BIOS no-emulation entry")
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

fn parse_boot_entry(entry: &[u8], boot_catalog_lba: u32) -> Result<BootImageInfo, &'static str> {
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

    // Boot entry format (El Torito spec):
    // 0x00: boot indicator
    // 0x01: boot media type
    // 0x02..0x04: load segment (0 => DEFAULT_LOAD_SEGMENT)
    // 0x04: system type
    // 0x05: unused
    // 0x06..0x08: sector count (0 => DEFAULT_SECTOR_COUNT)
    // 0x08..0x0C: load RBA (2048-byte sectors)
    let load_segment = u16::from_le_bytes(entry[2..4].try_into().unwrap());
    let load_segment = if load_segment == 0 {
        DEFAULT_LOAD_SEGMENT
    } else {
        load_segment
    };
    let sector_count = u16::from_le_bytes(entry[6..8].try_into().unwrap());
    let sector_count = if sector_count == 0 {
        DEFAULT_SECTOR_COUNT
    } else {
        sector_count
    };
    let load_rba = u32::from_le_bytes(entry[8..12].try_into().unwrap());

    Ok(BootImageInfo {
        boot_catalog_lba,
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
        initial[2..4].copy_from_slice(&load_segment.to_le_bytes());
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

        let parsed = parse_boot_image(&mut disk).expect("parser should succeed");
        assert_eq!(parsed.boot_catalog_lba, boot_catalog_lba);
        assert_eq!(
            parsed.image,
            BootImageInfo {
                boot_catalog_lba,
                load_segment: 0x07C0,
                sector_count: 4,
                load_rba: boot_image_lba,
            }
        );
    }

    #[test]
    fn parse_boot_image_accepts_nul_padded_boot_system_id() {
        let boot_catalog_lba = 20;
        let boot_image_lba = 21;
        let boot_image = [0xAB; ISO_BLOCK_BYTES];
        let mut img = build_minimal_iso_no_emulation(
            boot_catalog_lba,
            boot_image_lba,
            &boot_image,
            0,
            4,
        );

        // Patch the Boot Record Volume Descriptor (LBA17) boot system id to be NUL-padded rather
        // than space-padded.
        const EL_TORITO_ID: &[u8] = b"EL TORITO SPECIFICATION";
        let mut nul_id = [0u8; 32];
        nul_id[..EL_TORITO_ID.len()].copy_from_slice(EL_TORITO_ID);
        let brvd_off = 17 * ISO_BLOCK_BYTES;
        img[brvd_off + 7..brvd_off + 39].copy_from_slice(&nul_id);

        let mut disk = InMemoryDisk::new(img);
        let parsed = parse_boot_image(&mut disk).expect("parser should succeed");
        assert_eq!(parsed.boot_catalog_lba, boot_catalog_lba);
        assert_eq!(
            parsed.image,
            BootImageInfo {
                boot_catalog_lba,
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

        let info = bios
            .el_torito_boot_info
            .expect("El Torito boot should populate cached boot metadata");
        assert_eq!(info.media_type, crate::bios::ElToritoBootMediaType::NoEmulation);
        assert_eq!(info.boot_drive, 0xE0);
        assert_eq!(info.controller_index, 0);
        assert_eq!(info.boot_catalog_lba, Some(boot_catalog_lba));
        assert_eq!(info.boot_image_lba, Some(boot_image_lba));
        assert_eq!(info.load_segment, Some(0x07C0));
        assert_eq!(info.sector_count, Some(4));
    }

    #[test]
    fn parse_boot_image_defaults_sector_count_zero_to_four() {
        let boot_catalog_lba: u32 = 20;
        let boot_image_lba: u32 = 21;
        let boot_image = [0xAB; ISO_BLOCK_BYTES];
        let img = build_minimal_iso_no_emulation(
            boot_catalog_lba,
            boot_image_lba,
            &boot_image,
            0,
            0, // sector_count=0 => default
        );
        let mut disk = InMemoryDisk::new(img);

        let parsed = parse_boot_image(&mut disk).expect("parser should succeed");
        assert_eq!(parsed.image.sector_count, DEFAULT_SECTOR_COUNT);
    }

    #[test]
    fn parse_boot_image_scans_catalog_for_later_bootable_entry() {
        let boot_catalog_lba: u32 = 20;
        let boot_image_lba: u32 = 21;
        let boot_image = [0xCC; ISO_BLOCK_BYTES];

        // Allocate enough blocks for the volume descriptors + boot catalog + boot image.
        let total_blocks = (boot_image_lba as usize).saturating_add(1).max(32);
        let mut img = vec![0u8; total_blocks * ISO_BLOCK_BYTES];

        // Volume descriptors (same as `build_minimal_iso_no_emulation`).
        let mut pvd = [0u8; ISO_BLOCK_BYTES];
        pvd[0] = 0x01;
        pvd[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        pvd[6] = ISO9660_VERSION;
        write_iso_block(&mut img, 16, &pvd);

        let mut brvd = [0u8; ISO_BLOCK_BYTES];
        brvd[0] = 0x00;
        brvd[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        brvd[6] = ISO9660_VERSION;
        brvd[7..39].copy_from_slice(&EL_TORITO_BOOT_SYSTEM_ID_SPACES);
        brvd[0x47..0x4B].copy_from_slice(&boot_catalog_lba.to_le_bytes());
        write_iso_block(&mut img, 17, &brvd);

        let mut term = [0u8; ISO_BLOCK_BYTES];
        term[0] = 0xFF;
        term[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        term[6] = ISO9660_VERSION;
        write_iso_block(&mut img, 18, &term);

        // Boot catalog with a non-bootable initial entry followed by a BIOS section entry.
        let mut catalog = [0u8; ISO_BLOCK_BYTES];

        // Validation entry: set platform id to EFI (0xEF) so entry #1 is not considered a BIOS
        // entry.
        let mut validation = [0u8; 32];
        validation[0] = 0x01;
        validation[1] = 0xEF;
        validation[0x1E] = 0x55;
        validation[0x1F] = 0xAA;
        let mut sum: u16 = 0;
        for chunk in validation.chunks_exact(2) {
            sum = sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        let checksum = (0u16).wrapping_sub(sum);
        validation[0x1C..0x1E].copy_from_slice(&checksum.to_le_bytes());
        catalog[0..32].copy_from_slice(&validation);

        // Initial entry (#1): not bootable.
        let mut initial = [0u8; 32];
        initial[0] = 0x00; // not bootable
        initial[1] = 0x00; // no emulation
        catalog[32..64].copy_from_slice(&initial);

        // Section header (#2): BIOS platform (0).
        let mut header = [0u8; 32];
        header[0] = 0x90;
        header[1] = 0x00; // BIOS
        header[2..4].copy_from_slice(&1u16.to_le_bytes());
        catalog[64..96].copy_from_slice(&header);

        // Section entry (#3): bootable no-emulation entry.
        let mut entry = [0u8; 32];
        entry[0] = 0x88;
        entry[1] = 0x00;
        entry[2..4].copy_from_slice(&0u16.to_le_bytes()); // default load segment
        entry[6..8].copy_from_slice(&4u16.to_le_bytes());
        entry[8..12].copy_from_slice(&boot_image_lba.to_le_bytes());
        catalog[96..128].copy_from_slice(&entry);

        write_iso_block(&mut img, boot_catalog_lba as usize, &catalog);
        write_iso_block(&mut img, boot_image_lba as usize, &boot_image);

        let mut disk = InMemoryDisk::new(img);
        let parsed = parse_boot_image(&mut disk).expect("parser should succeed");
        assert_eq!(
            parsed.image,
            BootImageInfo {
                boot_catalog_lba,
                load_segment: DEFAULT_LOAD_SEGMENT,
                sector_count: 4,
                load_rba: boot_image_lba,
            }
        );
    }
}
