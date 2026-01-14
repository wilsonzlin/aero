#![cfg(not(target_arch = "wasm32"))]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aero_machine::{Machine, MachineConfig};
use aero_storage::{DiskError, Result as DiskResult, VirtualDisk};

const DEFAULT_WIN7_ISO_PATH: &str = "/state/win7.iso";
const ISO_SECTOR_SIZE: u64 = 2048;

#[derive(Debug)]
struct ReadOnlyFileDisk {
    file: File,
    len: u64,
}

impl ReadOnlyFileDisk {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

impl VirtualDisk for ReadOnlyFileDisk {
    fn capacity_bytes(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.len {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.len,
            });
        }

        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| DiskError::Io(e.to_string()))?;
        self.file
            .read_exact(buf)
            .map_err(|e| DiskError::Io(e.to_string()))?;
        Ok(())
    }

    fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> DiskResult<()> {
        Err(DiskError::Unsupported("read-only file disk"))
    }

    fn flush(&mut self) -> DiskResult<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct ElToritoBootInfo {
    /// Boot image LBA in 2048-byte ISO sectors.
    boot_image_lba: u32,
    /// Number of 512-byte sectors to load (per El Torito initial entry).
    boot_image_sector_count_512: u16,
    /// Load segment. If 0, the El Torito default is 0x07C0.
    load_segment: u16,
}

fn read_exact_at(file: &mut File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buf)?;
    Ok(())
}

fn parse_el_torito_boot_info(iso_path: &Path) -> std::io::Result<ElToritoBootInfo> {
    let mut file = File::open(iso_path)?;

    // ISO9660 volume descriptors begin at LBA 16 (2048-byte sectors) and are terminated by a type
    // 255 descriptor. We scan for the El Torito boot record volume descriptor (type 0) with a boot
    // system identifier of "EL TORITO SPECIFICATION".
    let mut buf = [0u8; ISO_SECTOR_SIZE as usize];
    let mut lba = 16u64;
    let boot_catalog_lba = loop {
        read_exact_at(&mut file, lba * ISO_SECTOR_SIZE, &mut buf)?;

        // All standard ISO9660 volume descriptors use "CD001".
        if &buf[1..6] != b"CD001" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing ISO9660 magic (CD001) in volume descriptor",
            ));
        }

        let ty = buf[0];
        if ty == 0 {
            let sys_id = &buf[7..39];
            // ISO9660 uses space-padded ASCII fields.
            let sys_id = std::str::from_utf8(sys_id)
                .unwrap_or("")
                .trim_end_matches(' ')
                .trim_end_matches('\0');
            if sys_id == "EL TORITO SPECIFICATION" {
                let lba_bytes: [u8; 4] = buf[0x47..0x4B].try_into().unwrap();
                break u32::from_le_bytes(lba_bytes);
            }
        } else if ty == 255 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "no El Torito boot record volume descriptor found",
            ));
        }

        lba = lba
            .checked_add(1)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "LBA overflow"))?;
    };

    // Boot catalog is a single ISO sector (2048 bytes).
    read_exact_at(
        &mut file,
        u64::from(boot_catalog_lba) * ISO_SECTOR_SIZE,
        &mut buf,
    )?;

    // Validation Entry (first 32 bytes).
    if buf[0] != 0x01 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "El Torito validation entry header id mismatch",
        ));
    }
    if buf[30] != 0x55 || buf[31] != 0xAA {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "El Torito validation entry key bytes mismatch",
        ));
    }

    // Initial/Default Entry (next 32 bytes).
    let entry = &buf[0x20..0x40];
    if entry[0] != 0x88 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "El Torito initial entry is not marked bootable",
        ));
    }

    let load_segment = u16::from_le_bytes([entry[2], entry[3]]);
    let boot_image_sector_count_512 = u16::from_le_bytes([entry[6], entry[7]]);
    let boot_image_lba = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);

    Ok(ElToritoBootInfo {
        boot_image_lba,
        boot_image_sector_count_512,
        load_segment,
    })
}

fn resolve_win7_iso_path() -> Option<PathBuf> {
    let path = std::env::var_os("AERO_WIN7_ISO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WIN7_ISO_PATH));
    path.exists().then_some(path)
}

#[test]
#[ignore = "requires a local Windows 7 ISO; see AGENTS.md (/state/win7.iso)"]
fn win7_iso_el_torito_boot_smoke() {
    let Some(iso_path) = resolve_win7_iso_path() else {
        eprintln!(
            "skipping: Windows 7 ISO not found (set AERO_WIN7_ISO or place it at {DEFAULT_WIN7_ISO_PATH})"
        );
        return;
    };

    let boot_info = parse_el_torito_boot_info(&iso_path)
        .unwrap_or_else(|e| panic!("failed to parse El Torito boot catalog: {e}"));

    // Read the first 512 bytes of the El Torito boot image so we can confirm the BIOS loaded the
    // correct bytes into guest memory.
    let expected_sector0 = {
        let mut file = File::open(&iso_path).expect("failed to open ISO for boot image read");
        let mut buf = [0u8; 512];
        read_exact_at(
            &mut file,
            u64::from(boot_info.boot_image_lba) * ISO_SECTOR_SIZE,
            &mut buf,
        )
        .expect("failed to read El Torito boot image sector 0");
        buf
    };

    // Canonical Windows 7 storage topology machine (AHCI HDD + IDE CD-ROM).
    let mut m = Machine::new(MachineConfig::win7_storage(64 * 1024 * 1024)).unwrap();

    // Attach a recognizable placeholder HDD (disk_id=0) so BIOS has an HDD fallback and so we can
    // detect if we accidentally booted from the HDD instead of the CD.
    let mut hdd_boot = [0u8; 512];
    hdd_boot[..8].copy_from_slice(b"FAKEHDD!");
    hdd_boot[510] = 0x55;
    hdd_boot[511] = 0xAA;
    m.set_disk_image(hdd_boot.to_vec()).unwrap();

    // Attach the Windows 7 ISO to the canonical IDE secondary master CD-ROM slot.
    let iso_disk = ReadOnlyFileDisk::open(&iso_path).expect("failed to open ISO as a virtual disk");
    m.attach_ide_secondary_master_iso(Box::new(iso_disk))
        .expect("failed to attach ISO to IDE secondary master");

    // Re-run firmware POST now that the install media is attached.
    //
    // The El Torito boot path (if enabled) should parse the ISO boot catalog and transfer control
    // to the boot image instead of halting with "Disk read error" / "Invalid boot signature".
    m.reset();

    assert!(
        !m.cpu().halted,
        "CPU is halted immediately after POST (likely BIOS boot failure)"
    );

    let entry_paddr = m.cpu().segments.cs.base.wrapping_add(m.cpu().rip());
    let expected_load_seg = if boot_info.load_segment == 0 {
        0x07C0u16
    } else {
        boot_info.load_segment
    };
    let expected_entry_paddr = u64::from(expected_load_seg) * 16;

    assert_eq!(
        entry_paddr, expected_entry_paddr,
        "unexpected boot entrypoint physical address (CS={:04x} IP={:04x})",
        m.cpu().segments.cs.selector,
        m.cpu().rip()
    );

    let loaded = m.read_physical_bytes(entry_paddr, 512);
    assert_eq!(
        loaded.as_slice(),
        expected_sector0.as_slice(),
        "guest memory at boot entrypoint does not match El Torito boot image sector 0; did we boot from the wrong device?"
    );

    // El Torito no-emulation boot images still carry the standard boot signature at offset 0x1FE.
    assert_eq!(loaded[510], 0x55);
    assert_eq!(loaded[511], 0xAA);

    // Keep runtime bounded: run a small instruction slice to ensure we can execute a bit of the
    // boot image without immediately halting at the BIOS reset vector.
    //
    // (We intentionally don't attempt to fully boot Windows here; this is only a smoke test.)
    let _ = m.run_slice(50_000);

    // This check isn't strict about the exit reason (the bootloader may execute HLT or request BIOS
    // assists), but the CPU should not be stuck halted at the reset vector.
    let post_slice_paddr = m.cpu().segments.cs.base.wrapping_add(m.cpu().rip());
    assert_ne!(
        post_slice_paddr,
        firmware::bios::RESET_VECTOR_PHYS,
        "CPU appears to still be at the BIOS reset vector after executing boot slice"
    );

    // Sanity: sector count from the boot catalog should be non-zero for Win7 media.
    assert_ne!(boot_info.boot_image_sector_count_512, 0);
}

