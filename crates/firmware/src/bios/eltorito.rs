//! El Torito (bootable CD-ROM) support.
//!
//! This module implements enough of the El Torito and ISO9660 structures to locate and load the
//! initial/default no-emulation boot entry from a typical Windows install ISO.

use super::{BlockDevice, DiskError, BIOS_SECTOR_SIZE};

// Error strings are intentionally stable: they are surfaced via `Bios::tty_output()` when POST
// fails, and unit tests (and humans) rely on them for debugging.
const ERR_NO_BOOT_RECORD: &str = "Missing El Torito boot record";
const ERR_INVALID_CATALOG: &str = "invalid boot catalog validation entry";
const ERR_NO_BOOTABLE_ENTRY: &str = "no bootable initial/default entry";
const ERR_READ: &str = "boot image read error";

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
const BIOS_SECTORS_PER_ISO_BLOCK: u64 = (ISO_BLOCK_BYTES / BIOS_SECTOR_SIZE) as u64; // 4

/// Default no-emulation load segment per the El Torito spec when the catalog field is zero.
const DEFAULT_LOAD_SEGMENT: u16 = 0x07C0;

/// Upper bound for scanning the ISO9660 volume descriptor set when locating the El Torito boot
/// record.
///
/// Real-world install media (e.g. Windows) places the boot record early in the descriptor set. Keep
/// this bounded so malformed or adversarial images cannot force POST to walk a huge image.
const MAX_VOLUME_DESCRIPTOR_SCAN: u32 = 128;

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

pub(super) fn parse_boot_image(
    disk: &mut dyn BlockDevice,
) -> Result<ParsedBootImage, &'static str> {
    let boot_catalog_lba = find_boot_catalog_lba(disk)?;
    let image = parse_boot_catalog(disk, boot_catalog_lba)?;

    // Validate that the boot image fits within the underlying disk. This is a hard safety check so
    // we do not attempt out-of-range reads on corrupt catalogs.
    let start_lba_512 = u64::from(image.load_rba)
        .checked_mul(BIOS_SECTORS_PER_ISO_BLOCK)
        .ok_or(ERR_READ)?;
    let end_lba_512 = start_lba_512
        .checked_add(u64::from(image.sector_count))
        .ok_or(ERR_READ)?;
    if end_lba_512 > disk.size_in_sectors() {
        return Err(ERR_READ);
    }

    Ok(ParsedBootImage {
        boot_catalog_lba,
        image,
    })
}

fn find_boot_catalog_lba(disk: &mut dyn BlockDevice) -> Result<u32, &'static str> {
    // ISO9660 volume descriptor set begins at logical block 16.
    let total_iso_blocks = disk.size_in_sectors() / BIOS_SECTORS_PER_ISO_BLOCK;
    // Bound scanning by both the disk size and a fixed maximum descriptor count so pathological
    // images cannot trigger extremely long loops.
    let max_iso_lba_by_size = u32::try_from(total_iso_blocks).unwrap_or(u32::MAX);
    let start = 16u32;
    let end_exclusive = max_iso_lba_by_size.min(start.saturating_add(MAX_VOLUME_DESCRIPTOR_SCAN));
    for iso_lba in start..end_exclusive {
        let mut block = [0u8; ISO_BLOCK_BYTES];
        if let Err(err) = read_iso_block(disk, iso_lba, &mut block) {
            // If we hit the end of the image while scanning for volume descriptors, treat it as a
            // missing boot record rather than surfacing a disk read error.
            match err {
                DiskError::OutOfRange => break,
            }
        }

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

    Err(ERR_NO_BOOT_RECORD)
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
        u32::try_from(available_blocks.min(u64::from(MAX_CATALOG_BLOCKS)).max(1)).unwrap_or(1);

    let mut catalog = vec![0u8; (blocks_to_read as usize) * ISO_BLOCK_BYTES];
    for i in 0..blocks_to_read {
        let mut block = [0u8; ISO_BLOCK_BYTES];
        read_iso_block(disk, boot_catalog_lba + i, &mut block).map_err(|_| ERR_READ)?;
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

    Err(ERR_NO_BOOTABLE_ENTRY)
}

fn validate_catalog_validation_entry(entry: &[u8]) -> Result<(), &'static str> {
    if entry.len() != 32 {
        return Err(ERR_INVALID_CATALOG);
    }
    if entry[0] != 0x01 {
        return Err(ERR_INVALID_CATALOG);
    }
    if entry[0x1E] != 0x55 || entry[0x1F] != 0xAA {
        return Err(ERR_INVALID_CATALOG);
    }

    // Checksum over 16-bit words must sum to 0.
    let mut sum: u16 = 0;
    for chunk in entry.chunks_exact(2) {
        sum = sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    if sum != 0 {
        return Err(ERR_INVALID_CATALOG);
    }

    Ok(())
}

fn parse_boot_entry(entry: &[u8], boot_catalog_lba: u32) -> Result<BootImageInfo, &'static str> {
    if entry.len() != 32 {
        return Err(ERR_INVALID_CATALOG);
    }

    let boot_indicator = entry[0];
    if boot_indicator != 0x88 {
        return Err(ERR_NO_BOOTABLE_ENTRY);
    }

    let media_type = entry[1];
    if media_type != 0 {
        return Err(ERR_NO_BOOTABLE_ENTRY);
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
        let mut sector = [0u8; BIOS_SECTOR_SIZE];
        disk.read_sector(base + i, &mut sector)?;
        let off = (i as usize) * BIOS_SECTOR_SIZE;
        out[off..off + BIOS_SECTOR_SIZE].copy_from_slice(&sector);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bios::{Bios, BiosConfig, InMemoryDisk, TestMemory};
    use aero_cpu_core::state::{gpr, CpuMode, CpuState};

    #[derive(Debug)]
    struct CountingDisk {
        inner: InMemoryDisk,
        reads: usize,
        max_reads: usize,
    }

    impl CountingDisk {
        fn new(data: Vec<u8>, max_reads: usize) -> Self {
            Self {
                inner: InMemoryDisk::new(data),
                reads: 0,
                max_reads,
            }
        }
    }

    impl BlockDevice for CountingDisk {
        fn read_sector(
            &mut self,
            lba: u64,
            buf: &mut [u8; BIOS_SECTOR_SIZE],
        ) -> Result<(), DiskError> {
            self.reads = self.reads.saturating_add(1);
            assert!(
                self.reads <= self.max_reads,
                "El Torito scan performed too many disk reads ({} > {})",
                self.reads,
                self.max_reads
            );
            self.inner.read_sector(lba, buf)
        }

        fn size_in_sectors(&self) -> u64 {
            self.inner.size_in_sectors()
        }
    }

    fn write_iso_block(img: &mut [u8], iso_lba: usize, block: &[u8; ISO_BLOCK_BYTES]) {
        let off = iso_lba * ISO_BLOCK_BYTES;
        img[off..off + ISO_BLOCK_BYTES].copy_from_slice(block);
    }

    fn iso9660_volume_descriptor(typ: u8) -> [u8; ISO_BLOCK_BYTES] {
        let mut desc = [0u8; ISO_BLOCK_BYTES];
        desc[0] = typ;
        desc[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        desc[6] = ISO9660_VERSION;
        desc
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
        let img =
            build_minimal_iso_no_emulation(boot_catalog_lba, boot_image_lba, &boot_image, 0, 4);
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
        let mut img =
            build_minimal_iso_no_emulation(boot_catalog_lba, boot_image_lba, &boot_image, 0, 4);

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
        let mut boot_image = [0x5A; ISO_BLOCK_BYTES];
        // Include a traditional 0x55AA signature so tests can sanity-check the first 512 bytes.
        boot_image[510] = 0x55;
        boot_image[511] = 0xAA;
        let img =
            build_minimal_iso_no_emulation(boot_catalog_lba, boot_image_lba, &boot_image, 0, 4);

        let mut bios = Bios::new(BiosConfig {
            memory_size_bytes: 16 * 1024 * 1024,
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        let mut disk = InMemoryDisk::new(img);

        bios.post(&mut cpu, &mut mem, &mut disk, None);

        let load_addr = (0x07C0u64) << 4;
        let loaded = mem.read_bytes(load_addr, ISO_BLOCK_BYTES);
        assert_eq!(loaded, boot_image.to_vec());

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
        assert_eq!(
            info.media_type,
            crate::bios::ElToritoBootMediaType::NoEmulation
        );
        assert_eq!(info.boot_drive, 0xE0);
        assert_eq!(info.controller_index, 0);
        assert_eq!(info.boot_catalog_lba, Some(boot_catalog_lba));
        assert_eq!(info.boot_image_lba, Some(boot_image_lba));
        assert_eq!(info.load_segment, Some(0x07C0));
        assert_eq!(info.sector_count, Some(4));
    }

    #[test]
    fn bios_post_respects_eltorito_non_default_load_segment() {
        let boot_catalog_lba = 20;
        let boot_image_lba = 21;
        let mut boot_image = [0xC3; ISO_BLOCK_BYTES];
        boot_image[510] = 0x55;
        boot_image[511] = 0xAA;
        let img = build_minimal_iso_no_emulation(
            boot_catalog_lba,
            boot_image_lba,
            &boot_image,
            0x2000,
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

        bios.post(&mut cpu, &mut mem, &mut disk, None);

        let load_addr = (0x2000u64) << 4;
        let loaded = mem.read_bytes(load_addr, ISO_BLOCK_BYTES);
        assert_eq!(loaded, boot_image.to_vec());

        assert_eq!(cpu.segments.cs.selector, 0x2000);
        assert_eq!(cpu.rip(), 0);
        assert_eq!(cpu.gpr[gpr::RDX] as u8, 0xE0);

        let info = bios
            .el_torito_boot_info
            .expect("El Torito boot should populate cached boot metadata");
        assert_eq!(info.load_segment, Some(0x2000));
    }

    #[test]
    fn el_torito_boot_failures_report_specific_messages() {
        fn run(img: Vec<u8>) -> String {
            let mut bios = Bios::new(BiosConfig {
                boot_drive: 0xE0,
                ..BiosConfig::default()
            });
            let mut cpu = CpuState::new(CpuMode::Real);
            let mut mem = TestMemory::new(16 * 1024 * 1024);
            let mut disk = InMemoryDisk::new(img);

            bios.post(&mut cpu, &mut mem, &mut disk, None);
            assert!(cpu.halted, "boot should fail");
            String::from_utf8_lossy(bios.tty_output())
                .trim()
                .to_string()
        }

        // 1) Missing boot record volume descriptor.
        let mut img = vec![0u8; 32 * ISO_BLOCK_BYTES];
        let mut pvd = [0u8; ISO_BLOCK_BYTES];
        pvd[0] = 0x01;
        pvd[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        pvd[6] = ISO9660_VERSION;
        write_iso_block(&mut img, 16, &pvd);
        let mut term = [0u8; ISO_BLOCK_BYTES];
        term[0] = 0xFF;
        term[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        term[6] = ISO9660_VERSION;
        write_iso_block(&mut img, 17, &term);
        assert_eq!(run(img), ERR_NO_BOOT_RECORD);

        // 2) Invalid boot catalog validation entry.
        let boot_catalog_lba = 20;
        let boot_image_lba = 21;
        let boot_image = [0x11; ISO_BLOCK_BYTES];
        let mut img =
            build_minimal_iso_no_emulation(boot_catalog_lba, boot_image_lba, &boot_image, 0, 4);
        // Corrupt the header id so validation fails.
        let off = boot_catalog_lba as usize * ISO_BLOCK_BYTES;
        img[off] = 0x02;
        assert_eq!(run(img), ERR_INVALID_CATALOG);

        // 3) No bootable initial/default entry.
        let mut img =
            build_minimal_iso_no_emulation(boot_catalog_lba, boot_image_lba, &boot_image, 0, 4);
        let off = boot_catalog_lba as usize * ISO_BLOCK_BYTES;
        // Clear the initial entry (boot indicator 0).
        img[off + 32] = 0x00;
        assert_eq!(run(img), ERR_NO_BOOTABLE_ENTRY);

        // 4) Boot image read error (catalog points past end-of-image).
        let mut img = vec![0u8; 32 * ISO_BLOCK_BYTES];
        // Minimal volume descriptor set with boot record.
        write_iso_block(&mut img, 16, &pvd);
        let mut brvd = [0u8; ISO_BLOCK_BYTES];
        brvd[0] = 0x00;
        brvd[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        brvd[6] = ISO9660_VERSION;
        brvd[7..39].copy_from_slice(&EL_TORITO_BOOT_SYSTEM_ID_SPACES);
        brvd[0x47..0x4B].copy_from_slice(&boot_catalog_lba.to_le_bytes());
        write_iso_block(&mut img, 17, &brvd);
        write_iso_block(&mut img, 18, &term);

        let mut catalog = [0u8; ISO_BLOCK_BYTES];
        // Valid validation entry.
        let mut validation = [0u8; 32];
        validation[0] = 0x01;
        validation[0x1E] = 0x55;
        validation[0x1F] = 0xAA;
        let mut sum: u16 = 0;
        for chunk in validation.chunks_exact(2) {
            sum = sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        let checksum = (0u16).wrapping_sub(sum);
        validation[0x1C..0x1E].copy_from_slice(&checksum.to_le_bytes());
        catalog[0..32].copy_from_slice(&validation);

        // Bootable no-emulation entry pointing well past the end of the disk.
        let mut entry = [0u8; 32];
        entry[0] = 0x88;
        entry[1] = 0x00;
        entry[6..8].copy_from_slice(&4u16.to_le_bytes());
        entry[8..12].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        catalog[32..64].copy_from_slice(&entry);
        write_iso_block(&mut img, boot_catalog_lba as usize, &catalog);

        assert_eq!(run(img), ERR_READ);
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

    #[test]
    fn parse_boot_image_missing_boot_record_without_terminator_returns_stable_error() {
        // Build an image that looks like an ISO9660 descriptor set but never provides a terminator
        // and never includes an El Torito boot record descriptor.
        let extra_descriptors = 8u32;
        let total_blocks = 16u32
            .saturating_add(MAX_VOLUME_DESCRIPTOR_SCAN)
            .saturating_add(extra_descriptors);
        let mut img = vec![0u8; (total_blocks as usize) * ISO_BLOCK_BYTES];

        for iso_lba in 16..(total_blocks as usize) {
            let desc = iso9660_volume_descriptor(0x01);
            write_iso_block(&mut img, iso_lba, &desc);
        }

        let cap_reads =
            (MAX_VOLUME_DESCRIPTOR_SCAN as usize) * (BIOS_SECTORS_PER_ISO_BLOCK as usize);
        let mut disk =
            CountingDisk::new(img, cap_reads + (BIOS_SECTORS_PER_ISO_BLOCK as usize) * 2);

        let err = parse_boot_image(&mut disk).unwrap_err();
        assert_eq!(err, ERR_NO_BOOT_RECORD);
        assert_eq!(disk.reads, cap_reads);
    }

    #[test]
    fn parse_boot_image_terminator_after_scan_cap_fails_quickly() {
        // Place a well-formed terminator after the scan cap; the implementation should not walk
        // arbitrarily far just to find the terminator.
        let terminator_offset = 10u32;
        let total_blocks = 16u32
            .saturating_add(MAX_VOLUME_DESCRIPTOR_SCAN)
            .saturating_add(terminator_offset)
            .saturating_add(1);
        let mut img = vec![0u8; (total_blocks as usize) * ISO_BLOCK_BYTES];

        for iso_lba in 16..(total_blocks as usize) {
            let desc = iso9660_volume_descriptor(0x01);
            write_iso_block(&mut img, iso_lba, &desc);
        }

        let terminator_lba = 16u32
            .saturating_add(MAX_VOLUME_DESCRIPTOR_SCAN)
            .saturating_add(terminator_offset);
        let term = iso9660_volume_descriptor(0xFF);
        write_iso_block(&mut img, terminator_lba as usize, &term);

        let cap_reads =
            (MAX_VOLUME_DESCRIPTOR_SCAN as usize) * (BIOS_SECTORS_PER_ISO_BLOCK as usize);
        let mut disk =
            CountingDisk::new(img, cap_reads + (BIOS_SECTORS_PER_ISO_BLOCK as usize) * 2);

        let err = parse_boot_image(&mut disk).unwrap_err();
        assert_eq!(err, ERR_NO_BOOT_RECORD);
        assert_eq!(disk.reads, cap_reads);
    }
}
