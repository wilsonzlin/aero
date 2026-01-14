use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aero_cpu_core::state::{gpr, CpuMode, CpuState};
use firmware::bios::{
    A20Gate, Bios, BiosConfig, CdromDevice, DiskError, FirmwareMemory, InMemoryDisk,
    BIOS_SECTOR_SIZE, CDROM_SECTOR_SIZE,
};
use memory::{DenseMemory, MapError, MemoryBus, PhysicalMemoryBus};

struct FileCdrom {
    file: File,
    sector_count: u64,
}

impl FileCdrom {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len % (CDROM_SECTOR_SIZE as u64) != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ISO size {len} is not a multiple of {CDROM_SECTOR_SIZE}"),
            ));
        }
        Ok(Self {
            file,
            sector_count: len / (CDROM_SECTOR_SIZE as u64),
        })
    }
}

impl CdromDevice for FileCdrom {
    fn read_sector(
        &mut self,
        lba: u64,
        buf: &mut [u8; CDROM_SECTOR_SIZE],
    ) -> Result<(), DiskError> {
        if lba >= self.sector_count {
            return Err(DiskError::OutOfRange);
        }
        let off = lba
            .checked_mul(CDROM_SECTOR_SIZE as u64)
            .ok_or(DiskError::OutOfRange)?;
        self.file
            .seek(SeekFrom::Start(off))
            .map_err(|_| DiskError::OutOfRange)?;
        self.file
            .read_exact(buf)
            .map_err(|_| DiskError::OutOfRange)?;
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        self.sector_count
    }
}

/// Minimal BIOS bus for firmware integration tests.
struct TestBus {
    a20_enabled: bool,
    inner: PhysicalMemoryBus,
}

impl TestBus {
    fn new(size: u64) -> Self {
        let ram = DenseMemory::new(size).expect("guest RAM allocation failed");
        Self {
            a20_enabled: false,
            inner: PhysicalMemoryBus::new(Box::new(ram)),
        }
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }
}

impl A20Gate for TestBus {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }
}

impl FirmwareMemory for TestBus {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        match self.inner.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                let already_mapped = self
                    .inner
                    .rom_regions()
                    .iter()
                    .any(|r| r.start == base && r.data.len() == len);
                if !already_mapped {
                    panic!("unexpected ROM mapping overlap at 0x{base:016x}");
                }
            }
            Err(MapError::AddressOverflow) => {
                panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})");
            }
        }
    }
}

impl MemoryBus for TestBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if self.a20_enabled {
            self.inner.read_physical(paddr, buf);
            return;
        }

        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            *slot = self.inner.read_physical_u8(addr);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if self.a20_enabled {
            self.inner.write_physical(paddr, buf);
            return;
        }

        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            self.inner.write_physical_u8(addr, byte);
        }
    }
}

fn set_real_mode_seg(seg: &mut aero_cpu_core::state::Segment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

#[derive(Debug, Clone, Copy)]
struct ElToritoInfo {
    boot_image_lba: u64,
    load_segment: u16,
    load_bytes: usize,
}

fn parse_el_torito(cdrom: &mut dyn CdromDevice) -> ElToritoInfo {
    let mut boot_catalog_lba = None;
    for lba in 16u64..64 {
        let mut sec = [0u8; CDROM_SECTOR_SIZE];
        cdrom.read_sector(lba, &mut sec).unwrap();
        assert_eq!(
            &sec[1..6],
            b"CD001",
            "invalid ISO9660 signature at LBA {lba}"
        );
        match sec[0] {
            0 => {
                if sec[7..39].starts_with(b"EL TORITO SPECIFICATION") {
                    boot_catalog_lba =
                        Some(u32::from_le_bytes(sec[71..75].try_into().unwrap()) as u64);
                    break;
                }
            }
            255 => break,
            _ => {}
        }
    }
    let boot_catalog_lba = boot_catalog_lba.expect("missing El Torito boot record");

    let mut catalog = [0u8; CDROM_SECTOR_SIZE];
    cdrom.read_sector(boot_catalog_lba, &mut catalog).unwrap();
    assert_eq!(
        catalog[0], 0x01,
        "invalid El Torito validation entry header"
    );
    assert_eq!(
        &catalog[30..32],
        &[0x55, 0xAA],
        "invalid El Torito validation key"
    );

    let entry = &catalog[0x20..0x40];
    assert_eq!(entry[0], 0x88, "El Torito initial entry is not bootable");
    assert_eq!(entry[1], 0, "expected no-emulation El Torito boot entry");

    let mut load_segment = u16::from_le_bytes([entry[2], entry[3]]);
    if load_segment == 0 {
        load_segment = 0x07C0;
    }
    let load_sectors_512 = u16::from_le_bytes([entry[6], entry[7]]) as usize;
    assert!(load_sectors_512 > 0, "invalid El Torito load size");
    let load_bytes = load_sectors_512 * BIOS_SECTOR_SIZE;
    let boot_image_lba = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as u64;

    ElToritoInfo {
        boot_image_lba,
        load_segment,
        load_bytes,
    }
}

fn iso_path() -> PathBuf {
    std::env::var_os("AERO_WINDOWS7_ISO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/state/win7.iso"))
}

#[test]
#[ignore]
fn win7_iso_el_torito_post_and_int13_cd_reads() {
    let path = iso_path();
    if !path.is_file() {
        eprintln!(
            "Skipping Windows 7 ISO El Torito test: ISO not found at {} (set AERO_WINDOWS7_ISO)",
            path.display()
        );
        return;
    }

    let mut cdrom = FileCdrom::open(&path).expect("failed to open ISO");
    let boot = parse_el_torito(&mut cdrom);

    // Dummy HDD present as 0x80, but boot from CD (0xE0).
    let mut hdd = InMemoryDisk::from_boot_sector([0u8; BIOS_SECTOR_SIZE]);

    let mut bios = Bios::new(BiosConfig {
        boot_drive: 0xE0,
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = CpuState::new(CpuMode::Real);
    let mut bus = TestBus::new(64 * 1024 * 1024);

    bios.post_with_cdrom(&mut cpu, &mut bus, &mut hdd, &mut cdrom);

    assert!(
        !cpu.halted,
        "BIOS POST should not halt (tty={:?})",
        std::str::from_utf8(bios.tty_output()).ok()
    );

    assert_eq!(cpu.gpr[gpr::RDX] as u8, 0xE0, "DL should be the boot drive");
    assert_eq!(cpu.segments.cs.selector, boot.load_segment);
    assert_eq!(cpu.rip(), 0);

    // Validate the boot image bytes were loaded where El Torito indicates.
    let load_phys = (boot.load_segment as u64) << 4;
    let mut expected = vec![0u8; boot.load_bytes];
    let cd_sectors = boot.load_bytes.div_ceil(CDROM_SECTOR_SIZE);
    for i in 0..cd_sectors {
        let mut sec = [0u8; CDROM_SECTOR_SIZE];
        cdrom
            .read_sector(boot.boot_image_lba + i as u64, &mut sec)
            .unwrap();
        let dst_off = i * CDROM_SECTOR_SIZE;
        let end = (dst_off + CDROM_SECTOR_SIZE).min(expected.len());
        expected[dst_off..end].copy_from_slice(&sec[..end - dst_off]);
    }

    let mut loaded = vec![0u8; boot.load_bytes];
    bus.read_physical(load_phys, &mut loaded);
    assert_eq!(loaded, expected);

    // Now issue INT 13h AH=42h (EDD read) against DL=0xE0 for LBA 16 and verify ISO9660 signature.
    //
    // The BIOS interrupt dispatch contract expects an IRET frame on the stack. Create a synthetic
    // one at 0000:8000.
    set_real_mode_seg(&mut cpu.segments.ss, 0x0000);
    cpu.gpr[gpr::RSP] = 0x8000;
    bus.write_u16(0x8000, 0x0000); // return IP
    bus.write_u16(0x8002, 0x0000); // return CS
    bus.write_u16(0x8004, 0x0202); // FLAGS

    set_real_mode_seg(&mut cpu.segments.ds, 0x0000);
    cpu.gpr[gpr::RSI] = 0x0500;
    cpu.gpr[gpr::RAX] = 0x4200; // AH=42h extended read
    cpu.gpr[gpr::RDX] = 0x00E0; // DL=0xE0

    // DAP (16 bytes) at 0000:0500.
    bus.write_u8(0x0500, 0x10); // size
    bus.write_u8(0x0501, 0x00); // reserved
    bus.write_u16(0x0502, 1); // count
    bus.write_u16(0x0504, 0x2000); // buffer offset
    bus.write_u16(0x0506, 0x0000); // buffer segment
    bus.write_u64(0x0508, 16); // LBA

    bios.dispatch_interrupt_with_cdrom(0x13, &mut cpu, &mut bus, &mut hdd, Some(&mut cdrom));

    let mut sector = vec![0u8; CDROM_SECTOR_SIZE];
    bus.read_physical(0x2000, &mut sector);
    assert_eq!(&sector[1..6], b"CD001");
}
