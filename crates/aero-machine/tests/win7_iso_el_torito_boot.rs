#![cfg(not(target_arch = "wasm32"))]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aero_cpu_core::state::{gpr, RFLAGS_CF};
use aero_machine::{Machine, MachineConfig};
use aero_storage::{DiskError, Result as DiskResult, VirtualDisk, SECTOR_SIZE};

const DEFAULT_WIN7_ISO_PATH: &str = "/state/win7.iso";
const ISO_SECTOR_SIZE: u64 = 2048;

// El Torito conventions (match `crates/firmware/src/bios/eltorito.rs`).
const EL_TORITO_DEFAULT_LOAD_SEGMENT: u16 = 0x07C0;
const EL_TORITO_DEFAULT_SECTOR_COUNT_512: u16 = 4;
const EL_TORITO_CD_BOOT_DRIVE: u8 = 0xE0;

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
    /// Boot catalog LBA in 2048-byte ISO sectors.
    boot_catalog_lba: u32,
    /// Boot image LBA in 2048-byte ISO sectors.
    boot_image_lba: u32,
    /// Number of 512-byte sectors to load (after applying El Torito defaulting rules).
    boot_image_sector_count_512: u16,
    /// Load segment (after applying El Torito defaulting rules).
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

    // The El Torito boot catalog may span more than one ISO block (though typical images, including
    // Windows install media, keep the bootable entries in the first block). To match the firmware
    // parser's behavior, read a small bounded window and scan for the first bootable BIOS
    // no-emulation entry.
    const MAX_CATALOG_BLOCKS: u64 = 4;
    let meta_len = file.metadata()?.len();
    let total_blocks = meta_len / ISO_SECTOR_SIZE;
    let available_blocks = total_blocks.saturating_sub(u64::from(boot_catalog_lba));
    let blocks_to_read = available_blocks.clamp(1, MAX_CATALOG_BLOCKS);

    let mut catalog = vec![0u8; (blocks_to_read * ISO_SECTOR_SIZE) as usize];
    read_exact_at(
        &mut file,
        u64::from(boot_catalog_lba) * ISO_SECTOR_SIZE,
        &mut catalog,
    )?;

    fn validate_validation_entry(entry: &[u8]) -> std::io::Result<()> {
        if entry.len() < 32 || entry[0] != 0x01 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "El Torito validation entry header id mismatch",
            ));
        }
        if entry[30] != 0x55 || entry[31] != 0xAA {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "El Torito validation entry key bytes mismatch",
            ));
        }

        // Checksum over 16-bit words (little-endian) must sum to 0.
        let mut sum: u16 = 0;
        for chunk in entry[0..32].chunks_exact(2) {
            sum = sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        if sum != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "El Torito validation entry checksum mismatch",
            ));
        }
        Ok(())
    }

    if catalog.len() < 64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "El Torito boot catalog too small",
        ));
    }

    let validation = &catalog[0..32];
    validate_validation_entry(validation)?;

    // The initial/default entry uses the platform id from the validation entry; section headers can
    // override this for subsequent entries.
    let mut current_platform_id = validation[1];

    for entry in catalog.chunks_exact(32).skip(1) {
        match entry[0] {
            // Section header.
            0x90 | 0x91 => {
                current_platform_id = entry[1];
            }
            // Boot entry.
            0x00 | 0x88 => {
                // BIOS/x86 platform id is 0.
                if current_platform_id != 0 {
                    continue;
                }
                // Require bootable + no-emulation.
                if entry[0] != 0x88 || entry[1] != 0 {
                    continue;
                }

                let load_segment = u16::from_le_bytes([entry[2], entry[3]]);
                let load_segment = if load_segment == 0 {
                    EL_TORITO_DEFAULT_LOAD_SEGMENT
                } else {
                    load_segment
                };

                let boot_image_sector_count_512 = u16::from_le_bytes([entry[6], entry[7]]);
                let boot_image_sector_count_512 = if boot_image_sector_count_512 == 0 {
                    // Per El Torito, the default load size when the catalog field is zero is 4
                    // 512-byte virtual sectors (2048 bytes). Windows install media commonly relies
                    // on this default for `etfsboot.com`.
                    EL_TORITO_DEFAULT_SECTOR_COUNT_512
                } else {
                    boot_image_sector_count_512
                };
                let boot_image_lba = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);

                return Ok(ElToritoBootInfo {
                    boot_catalog_lba,
                    boot_image_lba,
                    boot_image_sector_count_512,
                    load_segment,
                });
            }
            _ => {}
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "no bootable BIOS no-emulation entry found in El Torito catalog",
    ))
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

    // Read the boot image bytes so we can confirm the BIOS loaded the correct bytes into guest
    // memory.
    let expected_boot_bytes = {
        let mut file = File::open(&iso_path).expect("failed to open ISO for boot image read");
        let want_len = usize::from(boot_info.boot_image_sector_count_512)
            .saturating_mul(SECTOR_SIZE)
            // Clamp to a small-ish window so a corrupt catalog cannot cause OOM. Windows install
            // media uses a small no-emulation boot image (typically 4 sectors / 2048 bytes).
            .min(64 * SECTOR_SIZE);
        let mut buf = vec![0u8; want_len];
        read_exact_at(
            &mut file,
            u64::from(boot_info.boot_image_lba) * ISO_SECTOR_SIZE,
            &mut buf,
        )
        .expect("failed to read El Torito boot image bytes");
        buf
    };

    // Canonical Windows 7 storage topology machine (AHCI HDD + IDE CD-ROM).
    let mut m = Machine::new(MachineConfig::win7_storage(64 * 1024 * 1024)).unwrap();

    // Attach a recognizable placeholder HDD to the machine's canonical shared disk so the BIOS has
    // an HDD fallback and we can detect if we accidentally booted from it instead of the CD.
    let mut hdd = vec![0u8; 64 * SECTOR_SIZE];
    hdd[0..8].copy_from_slice(b"FAKEHDD!");
    hdd[510] = 0x55;
    hdd[511] = 0xAA;
    m.set_disk_image(hdd).unwrap();

    // Attach the Windows 7 ISO to the canonical IDE secondary master CD-ROM slot.
    let iso_disk = ReadOnlyFileDisk::open(&iso_path).expect("failed to open ISO as a virtual disk");
    m.attach_ide_secondary_master_iso(Box::new(iso_disk))
        .expect("failed to attach ISO to IDE secondary master");

    // Explicitly request CD boot. `Machine` defaults to booting from the first HDD
    // (`boot_drive=0x80`), so this test must override it to the El Torito CD boot drive (`0xE0`)
    // after attaching the install media and before running POST again.
    m.set_boot_drive(0xE0);

    // Re-run firmware POST now that the install media is attached.
    //
    // The El Torito boot path (if enabled) should parse the ISO boot catalog and transfer control
    // to the boot image instead of halting with "Disk read error" / "Invalid boot signature".
    m.reset();

    assert!(
        !m.cpu().halted,
        "CPU is halted immediately after POST (likely BIOS boot failure)"
    );
    assert!(
        !m.bios_tty_output()
            .windows(b"Disk read error".len())
            .any(|w| w == b"Disk read error"),
        "BIOS reported a disk read error:\n{}",
        String::from_utf8_lossy(m.bios_tty_output())
    );
    assert!(
        !m.bios_tty_output()
            .windows(b"Invalid boot signature".len())
            .any(|w| w == b"Invalid boot signature"),
        "BIOS reported an invalid boot signature:\n{}",
        String::from_utf8_lossy(m.bios_tty_output())
    );

    let entry_paddr = m.cpu().segments.cs.base.wrapping_add(m.cpu().rip());
    let expected_entry_paddr = u64::from(boot_info.load_segment) * 16;

    assert_eq!(
        entry_paddr,
        expected_entry_paddr,
        "unexpected boot entrypoint physical address (CS={:04x} IP={:04x})",
        m.cpu().segments.cs.selector,
        m.cpu().rip()
    );
    assert_eq!(
        m.cpu().gpr[gpr::RDX] as u8,
        EL_TORITO_CD_BOOT_DRIVE,
        "expected BIOS to pass boot drive in DL"
    );

    let loaded = m.read_physical_bytes(entry_paddr, expected_boot_bytes.len());
    assert!(
        !loaded.as_slice().starts_with(b"FAKEHDD!"),
        "boot entrypoint matches the fake HDD marker; BIOS did not boot from the attached ISO (boot_drive must be 0xE0 for CD boot)"
    );
    assert_eq!(
        loaded.as_slice(),
        expected_boot_bytes.as_slice(),
        "guest memory at boot entrypoint does not match El Torito boot image bytes; did we boot from the wrong device?"
    );

    // El Torito no-emulation boot images still carry the standard boot signature at offset 0x1FE.
    assert_eq!(loaded[510], 0x55);
    assert_eq!(loaded[511], 0xAA);

    // Smoke-test INT 13h AH=4Bh ("El Torito disk emulation services"): some boot images query this
    // to locate the boot catalog and/or confirm the BIOS boot drive mapping. This also validates
    // that the BIOS cached boot metadata during POST (and uses the same defaulting rules we
    // applied above).
    {
        const PACKET_ADDR: u64 = 0x0500;
        const CODE_ADDR: u64 = 0x9000;

        let cpu_entry = m.cpu().clone();
        let saved_packet = m.read_physical_bytes(PACKET_ADDR, 0x20);

        // Caller-supplied status packet buffer (0x13 bytes). Use a larger clear window so stale
        // bytes can't mask partial writes.
        m.write_physical(PACKET_ADDR, &[0u8; 0x20]);

        // Tiny real-mode stub:
        //   mov es, 0
        //   mov di, PACKET_ADDR
        //   mov ax, 0x4B01
        //   mov dx, 0x00E0
        //   int 0x13
        //   cli
        //   hlt
        let code: [u8; 17] = [
            0x31,
            0xC0, // xor ax, ax
            0x8E,
            0xC0, // mov es, ax
            0xBF,
            0x00,
            0x05, // mov di, 0x0500
            0xB8,
            0x01,
            0x4B, // mov ax, 0x4B01
            0xBA,
            EL_TORITO_CD_BOOT_DRIVE,
            0x00, // mov dx, 0x00E0
            0xCD,
            0x13, // int 0x13
            0xFA, // cli
            0xF4, // hlt
        ];
        let saved_code = m.read_physical_bytes(CODE_ADDR, code.len());
        m.write_physical(CODE_ADDR, &code);

        // Jump to the stub.
        {
            let cpu = m.cpu_mut();
            cpu.halted = false;
            cpu.clear_pending_bios_int();
            cpu.segments.cs.selector = 0;
            cpu.segments.cs.base = 0;
            cpu.segments.cs.limit = 0xFFFF;
            cpu.segments.cs.access = 0;
            cpu.set_rip(CODE_ADDR);
        }

        let _ = m.run_slice(10_000);

        assert_eq!(
            m.cpu().rflags() & RFLAGS_CF,
            0,
            "INT 13h AH=4Bh AL=01h reported failure (CF=1)"
        );

        assert_eq!(m.read_physical_u8(PACKET_ADDR), 0x13);
        assert_eq!(m.read_physical_u8(PACKET_ADDR + 1), 0x00); // no-emulation
        assert_eq!(m.read_physical_u8(PACKET_ADDR + 2), EL_TORITO_CD_BOOT_DRIVE);
        assert_eq!(m.read_physical_u8(PACKET_ADDR + 3), 0); // controller index
        assert_eq!(
            m.read_physical_u32(PACKET_ADDR + 4),
            boot_info.boot_image_lba
        );
        assert_eq!(
            m.read_physical_u32(PACKET_ADDR + 8),
            boot_info.boot_catalog_lba
        );
        assert_eq!(
            m.read_physical_u16(PACKET_ADDR + 12),
            boot_info.load_segment
        );
        assert_eq!(
            m.read_physical_u16(PACKET_ADDR + 14),
            boot_info.boot_image_sector_count_512
        );

        // Restore memory we scribbled so the subsequent boot image slice runs in an environment as
        // close as possible to the real POST handoff.
        m.write_physical(PACKET_ADDR, &saved_packet);
        m.write_physical(CODE_ADDR, &saved_code);

        // Restore CPU state at the El Torito boot entrypoint so we can execute a bounded slice of
        // the real boot image below.
        *m.cpu_mut() = cpu_entry;
    }

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
}
