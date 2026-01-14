use std::sync::Arc;

use aero_cpu_core::state::{gpr, CpuMode, CpuState};
use firmware::bios::{
    A20Gate, Bios, BiosBus, BiosConfig, FirmwareMemory, InMemoryCdrom, InMemoryDisk, BDA_BASE,
    BIOS_SECTOR_SIZE,
};
use memory::{DenseMemory, MapError, MemoryBus, PhysicalMemoryBus};

const ISO_SECTOR_SIZE: usize = 2048;

/// Deterministic in-memory El Torito ISO image used by BIOS unit tests.
///
/// This is intentionally minimal: it only includes what the BIOS needs to find the boot catalog
/// and load a no-emulation boot image.
struct TestIso {
    bytes: Vec<u8>,
    boot_catalog_lba: u32,
    boot_image_lba: u32,
    boot_image: [u8; ISO_SECTOR_SIZE],
}

impl TestIso {
    fn build() -> Self {
        // Keep the image tiny but large enough to include the ISO9660 volume descriptor set
        // (starting at LBA 16) and a couple of El Torito structures.
        let total_sectors = 32usize;
        let mut bytes = vec![0u8; total_sectors * ISO_SECTOR_SIZE];

        // Layout.
        let boot_catalog_lba: u32 = 20;
        let boot_image_lba: u32 = 21;

        // Primary Volume Descriptor (LBA 16).
        write_volume_descriptor_header(&mut bytes, 16, 0x01);

        // Boot Record Volume Descriptor (LBA 17).
        write_volume_descriptor_header(&mut bytes, 17, 0x00);
        write_space_padded_ascii(
            &mut bytes,
            lba_offset(17) + 7,
            32,
            "EL TORITO SPECIFICATION",
        );
        // Boot catalog pointer (little-endian LBA) at offset 0x47 (71).
        write_u32_le(&mut bytes, lba_offset(17) + 0x47, boot_catalog_lba);

        // Volume Descriptor Set Terminator (LBA 18).
        write_volume_descriptor_header(&mut bytes, 18, 0xFF);

        // Boot catalog (LBA 20).
        write_el_torito_boot_catalog(&mut bytes, boot_catalog_lba, boot_image_lba);

        // Boot image (LBA 21): 2048 bytes, recognizable pattern.
        let mut boot_image = [0u8; ISO_SECTOR_SIZE];
        boot_image[..8].copy_from_slice(b"AEROISO!");
        // Tiny program: `hlt; jmp $` at offset 0x10 for easy debugging.
        boot_image[0x10..0x13].copy_from_slice(&[0xF4, 0xEB, 0xFE]);
        // Fill the remainder with a deterministic non-zero byte so reads are obvious.
        for b in &mut boot_image[0x20..] {
            *b = 0xCC;
        }
        // Put a classic 0x55AA signature in the first 512 bytes so accidental MBR checks pass.
        boot_image[510] = 0x55;
        boot_image[511] = 0xAA;
        bytes[lba_offset(boot_image_lba)..lba_offset(boot_image_lba) + ISO_SECTOR_SIZE]
            .copy_from_slice(&boot_image);

        Self {
            bytes,
            boot_catalog_lba,
            boot_image_lba,
            boot_image,
        }
    }
}

fn lba_offset(lba: u32) -> usize {
    (lba as usize) * ISO_SECTOR_SIZE
}

fn write_volume_descriptor_header(image: &mut [u8], lba: u32, ty: u8) {
    let base = lba_offset(lba);
    image[base] = ty;
    image[base + 1..base + 6].copy_from_slice(b"CD001");
    image[base + 6] = 0x01; // version
}

fn write_padded_ascii(image: &mut [u8], offset: usize, len: usize, s: &str) {
    let bytes = s.as_bytes();
    let copy_len = bytes.len().min(len);
    image[offset..offset + len].fill(0);
    image[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
}

fn write_space_padded_ascii(image: &mut [u8], offset: usize, len: usize, s: &str) {
    let bytes = s.as_bytes();
    let copy_len = bytes.len().min(len);
    image[offset..offset + len].fill(b' ');
    image[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
}

fn write_u16_le(image: &mut [u8], offset: usize, v: u16) {
    image[offset..offset + 2].copy_from_slice(&v.to_le_bytes());
}

fn write_u32_le(image: &mut [u8], offset: usize, v: u32) {
    image[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
}

fn write_el_torito_boot_catalog(image: &mut [u8], boot_catalog_lba: u32, boot_image_lba: u32) {
    let base = lba_offset(boot_catalog_lba);
    let catalog = &mut image[base..base + ISO_SECTOR_SIZE];
    catalog.fill(0);

    // Validation Entry (32 bytes).
    //
    // Fields:
    // - Header ID = 0x01
    // - Platform ID = 0 (80x86)
    // - ID string (optional)
    // - Checksum (u16) such that the 16-bit sum of the whole 32-byte entry is 0.
    // - Key bytes 0x55AA.
    let mut validation = [0u8; 32];
    validation[0] = 0x01;
    validation[1] = 0x00;
    write_padded_ascii(&mut validation, 4, 24, "AERO ELTORITO TEST");
    validation[30] = 0x55;
    validation[31] = 0xAA;
    let checksum = el_torito_validation_checksum(&validation);
    validation[28..30].copy_from_slice(&checksum.to_le_bytes());
    catalog[0..32].copy_from_slice(&validation);

    // Default/Initial Entry (32 bytes).
    //
    // We use "no emulation" and load segment 0 => BIOS should treat it as 0x07C0.
    let entry_off = 32;
    catalog[entry_off] = 0x88; // bootable
    catalog[entry_off + 1] = 0x00; // no emulation
    write_u16_le(catalog, entry_off + 2, 0x0000); // load segment (0 => 0x07C0)
    catalog[entry_off + 4] = 0x00; // system type (ignored for no-emulation)
    catalog[entry_off + 5] = 0x00;
    write_u16_le(catalog, entry_off + 6, 4); // sector count (4 * 512 = 2048 bytes)
    write_u32_le(catalog, entry_off + 8, boot_image_lba);
}

fn el_torito_validation_checksum(entry: &[u8; 32]) -> u16 {
    // The checksum is defined over the 32-byte validation entry as a 16-bit sum of little-endian
    // words. The checksum field itself (bytes 28-29) participates in the sum.
    //
    // We compute the two's complement so the final sum is 0.
    let mut sum: u32 = 0;
    for i in (0..32).step_by(2) {
        // Treat the checksum bytes as zero while computing the required value.
        let word = if i == 28 {
            0u16
        } else {
            u16::from_le_bytes([entry[i], entry[i + 1]])
        };
        sum = sum.wrapping_add(u32::from(word));
    }
    let sum16 = sum as u16;
    sum16.wrapping_neg()
}

/// Minimal guest memory bus for BIOS integration tests.
///
/// This mirrors `firmware::bios::TestMemory` but lives in an integration test so we can exercise
/// BIOS public APIs without needing internal-only helpers.
#[allow(dead_code)]
struct TestBus {
    a20_enabled: bool,
    inner: PhysicalMemoryBus,
}

#[allow(dead_code)]
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

    fn read_bytes(&mut self, paddr: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.read_physical(paddr, &mut out);
        out
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

#[allow(dead_code)]
fn install_interrupt_frame(bus: &mut dyn BiosBus, cpu: &mut CpuState, ss: u16, sp: u16) {
    // `Bios::dispatch_interrupt` expects the CPU to have executed `INT` already, so SS:SP points to
    // an interrupt frame containing return IP, CS, FLAGS.
    cpu.mode = CpuMode::Real;
    cpu.segments.ss.selector = ss;
    cpu.segments.ss.base = (ss as u64) << 4;
    cpu.segments.ss.limit = 0xFFFF;
    cpu.segments.ss.access = 0;
    cpu.gpr[gpr::RSP] = sp as u64;

    let frame_base = cpu.apply_a20(cpu.segments.ss.base.wrapping_add(sp as u64));
    bus.write_u16(frame_base, 0); // return IP
    bus.write_u16(frame_base + 2, 0); // return CS
    bus.write_u16(frame_base + 4, 0x0202); // return FLAGS (IF=1 + bit1)
}

#[test]
fn test_iso_builder_produces_a_minimal_el_torito_layout() {
    let iso = TestIso::build();

    // Boot Record Volume Descriptor should live at LBA 17 and advertise the boot catalog pointer.
    let brvd = &iso.bytes[lba_offset(17)..lba_offset(17) + ISO_SECTOR_SIZE];
    assert_eq!(brvd[0], 0x00);
    assert_eq!(&brvd[1..6], b"CD001");
    assert_eq!(brvd[6], 0x01);
    assert_eq!(&brvd[7..7 + 23], b"EL TORITO SPECIFICATION");
    let catalog_lba = u32::from_le_bytes(brvd[0x47..0x47 + 4].try_into().unwrap());
    assert_eq!(catalog_lba, iso.boot_catalog_lba);

    // Boot catalog validation entry should contain the 0x55AA key bytes and a correct checksum.
    let catalog =
        &iso.bytes[lba_offset(iso.boot_catalog_lba)..lba_offset(iso.boot_catalog_lba) + 64];
    assert_eq!(catalog[0], 0x01);
    assert_eq!(catalog[30], 0x55);
    assert_eq!(catalog[31], 0xAA);
    let mut sum: u32 = 0;
    for i in (0..32).step_by(2) {
        let word = u16::from_le_bytes([catalog[i], catalog[i + 1]]);
        sum = sum.wrapping_add(u32::from(word));
    }
    assert_eq!(sum as u16, 0);

    // Default entry should be bootable, no-emulation, sector count 4, and point at the boot image.
    assert_eq!(catalog[32], 0x88);
    assert_eq!(catalog[33], 0x00);
    let sector_count = u16::from_le_bytes([catalog[38], catalog[39]]);
    assert_eq!(sector_count, 4);
    let image_lba = u32::from_le_bytes(catalog[40..44].try_into().unwrap());
    assert_eq!(image_lba, iso.boot_image_lba);

    // Boot image should contain the marker + tiny program and an MBR-style signature (for sanity).
    assert_eq!(&iso.boot_image[..8], b"AEROISO!");
    assert_eq!(&iso.boot_image[0x10..0x13], &[0xF4, 0xEB, 0xFE]);
    assert_eq!(&iso.boot_image[510..BIOS_SECTOR_SIZE], &[0x55, 0xAA]);
}

#[test]
fn bios_post_boots_from_cd_eltorito_no_emulation() {
    let iso = TestIso::build();

    // BIOS CD boot uses the same 512-byte `BlockDevice` interface; El Torito blocks are mapped as
    // 4 consecutive 512-byte sectors.
    let mut disk = InMemoryDisk::new(iso.bytes.clone());

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0xE0,
        // Keep this test focused on El Torito + INT13; ACPI is validated separately.
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = CpuState::new(CpuMode::Real);
    let mut bus = TestBus::new(16 * 1024 * 1024);

    bios.post(&mut cpu, &mut bus, &mut disk, None);

    // Load segment 0 in the boot catalog is specified to map to 0x07C0.
    let load_addr = (0x07C0u64) << 4;
    let loaded = bus.read_bytes(load_addr, ISO_SECTOR_SIZE);
    assert_eq!(loaded.as_slice(), &iso.boot_image[..]);

    // CPU entry state: CS:IP should point at the loaded image, and DL should match boot drive.
    assert_eq!(cpu.segments.cs.selector, 0x07C0);
    assert_eq!(cpu.rip(), 0);
    assert_eq!(cpu.gpr[gpr::RDX] as u8, 0xE0);
    assert!(bios.booted_from_cdrom());

    // CD boot drive numbers (`0xE0..=0xEF`) must not inflate the BIOS Data Area hard disk count
    // field (0x40:0x75). With only a CD/ISO backend, there are no fixed disks to advertise.
    assert_eq!(bus.read_u8(BDA_BASE + 0x75), 0);
}

#[test]
fn bios_post_cd_boot_with_cdrom_backend_still_advertises_hdd0_in_bda() {
    let iso = TestIso::build();
    let mut cdrom = InMemoryCdrom::new(iso.bytes.clone());

    // Provide an HDD alongside the CD-ROM, but boot explicitly from CD (`DL=0xE0`).
    let mut hdd_sector = [0u8; BIOS_SECTOR_SIZE];
    hdd_sector[510] = 0x55;
    hdd_sector[511] = 0xAA;
    let mut hdd = InMemoryDisk::from_boot_sector(hdd_sector);

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0xE0,
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = CpuState::new(CpuMode::Real);
    let mut bus = TestBus::new(16 * 1024 * 1024);

    bios.post_with_cdrom(&mut cpu, &mut bus, &mut hdd, &mut cdrom);

    // Booted from CD.
    assert_eq!(cpu.gpr[gpr::RDX] as u8, 0xE0);
    assert_eq!(cpu.segments.cs.selector, 0x07C0);
    assert_eq!(cpu.rip(), 0);
    assert!(bios.booted_from_cdrom());
    assert_eq!(bios.config().boot_drive, 0xE0);

    // But still advertise HDD0 presence (fixed-disk count) so guests can access the HDD during install.
    assert_eq!(bus.read_u8(BDA_BASE + 0x75), 1);
}

#[test]
fn bios_post_cd_first_policy_boots_cd_but_restores_hdd_boot_drive() {
    let iso = TestIso::build();
    let mut cdrom = InMemoryCdrom::new(iso.bytes.clone());

    // Provide a valid HDD MBR boot sector as the fallback (not used in this test because CD boot succeeds).
    let mut hdd_sector = [0u8; BIOS_SECTOR_SIZE];
    hdd_sector[510] = 0x55;
    hdd_sector[511] = 0xAA;
    let mut hdd = InMemoryDisk::from_boot_sector(hdd_sector);

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        // Keep HDD0 as the configured fallback boot drive.
        boot_drive: 0x80,
        // Enable CD-first policy (boot CD0 if present).
        boot_from_cd_if_present: true,
        cd_boot_drive: 0xE0,
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = CpuState::new(CpuMode::Real);
    let mut bus = TestBus::new(16 * 1024 * 1024);

    bios.post_with_cdrom(&mut cpu, &mut bus, &mut hdd, &mut cdrom);

    // Booted from CD.
    assert_eq!(cpu.gpr[gpr::RDX] as u8, 0xE0);
    assert_eq!(cpu.segments.cs.selector, 0x07C0);
    assert_eq!(cpu.rip(), 0);
    assert!(bios.booted_from_cdrom());

    // But keep the configured fallback boot drive intact.
    assert_eq!(bios.config().boot_drive, 0x80);
    assert!(bios.config().boot_from_cd_if_present);

    // HDD0 should still be advertised via the BDA fixed-disk count (0x40:0x75).
    assert_eq!(bus.read_u8(BDA_BASE + 0x75), 1);
}

#[test]
fn bios_post_cd_first_policy_falls_back_to_hdd_when_cd_is_unbootable() {
    // An unbootable ISO: no ISO9660/El Torito descriptors. CD boot should fail and fall back to HDD.
    let mut cdrom = InMemoryCdrom::new(vec![0u8; 32 * ISO_SECTOR_SIZE]);

    // Valid MBR boot sector.
    let mut hdd_sector = [0u8; BIOS_SECTOR_SIZE];
    hdd_sector[0..8].copy_from_slice(b"AEROHDD!");
    hdd_sector[510] = 0x55;
    hdd_sector[511] = 0xAA;
    let mut hdd = InMemoryDisk::from_boot_sector(hdd_sector);

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0x80,
        boot_from_cd_if_present: true,
        cd_boot_drive: 0xE0,
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = CpuState::new(CpuMode::Real);
    let mut bus = TestBus::new(16 * 1024 * 1024);

    bios.post_with_cdrom(&mut cpu, &mut bus, &mut hdd, &mut cdrom);

    // Fell back to HDD.
    assert_eq!(cpu.gpr[gpr::RDX] as u8, 0x80);
    assert_eq!(cpu.segments.cs.selector, 0x0000);
    assert_eq!(cpu.rip(), 0x7C00);
    assert!(!bios.booted_from_cdrom());

    // Boot sector should be loaded into 0000:7C00.
    let loaded = bus.read_bytes(0x7C00, BIOS_SECTOR_SIZE);
    assert_eq!(loaded.as_slice(), &hdd_sector[..]);

    // Configured fallback boot drive is still intact.
    assert_eq!(bios.config().boot_drive, 0x80);
    assert!(bios.config().boot_from_cd_if_present);
}

#[test]
fn int13_ext_read_cd_via_dispatch_interrupt_reads_2048_sector() {
    let iso = TestIso::build();
    let mut disk = InMemoryDisk::new(iso.bytes.clone());

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0xE0,
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = CpuState::new(CpuMode::Real);
    let mut bus = TestBus::new(16 * 1024 * 1024);

    bios.post(&mut cpu, &mut bus, &mut disk, None);

    // Build a 16-byte Disk Address Packet at DS:SI=0000:0500.
    let dap_addr = 0x0500u64;
    bus.write_u8(dap_addr, 0x10); // size
    bus.write_u8(dap_addr + 1, 0); // reserved
    bus.write_u16(dap_addr + 2, 1); // sector count (2048-byte sectors for CD)
    bus.write_u16(dap_addr + 4, 0x0000); // dst offset
    bus.write_u16(dap_addr + 6, 0x2000); // dst segment (0x20000)
    bus.write_u64(dap_addr + 8, iso.boot_image_lba as u64); // ISO LBA (2048-byte blocks)

    // Set up registers for INT 13h AH=42h.
    cpu.gpr[gpr::RAX] = 0x4200;
    cpu.gpr[gpr::RDX] = 0x00E0; // DL=CD0
    cpu.gpr[gpr::RSI] = 0x0500;

    // Provide an interrupt frame so the dispatcher can merge return flags into the IRET image.
    install_interrupt_frame(&mut bus, &mut cpu, 0x0000, 0x0100);

    bios.dispatch_interrupt(0x13, &mut cpu, &mut bus, &mut disk, None);

    let flags = bus.read_u16(0x0104);
    assert_eq!(flags & 0x0001, 0, "CF should be cleared on success");
    assert_ne!(
        flags & 0x0200,
        0,
        "IF should be preserved from the interrupt frame"
    );

    let dst = 0x20000u64;
    let buf = bus.read_bytes(dst, ISO_SECTOR_SIZE);
    assert_eq!(buf.as_slice(), &iso.boot_image[..]);
}

#[test]
fn int13_ext_get_drive_params_cd_reports_2048_bytes_per_sector_via_dispatch_interrupt() {
    let iso = TestIso::build();
    let mut disk = InMemoryDisk::new(iso.bytes.clone());

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0xE0,
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = CpuState::new(CpuMode::Real);
    let mut bus = TestBus::new(16 * 1024 * 1024);

    bios.post(&mut cpu, &mut bus, &mut disk, None);

    // Caller-supplied EDD parameter table at DS:SI=0000:0600.
    let table = 0x0600u64;
    bus.write_u16(table, 0x1E); // buffer size

    cpu.gpr[gpr::RAX] = 0x4800;
    cpu.gpr[gpr::RDX] = 0x00E0; // DL=CD0
    cpu.gpr[gpr::RSI] = 0x0600;

    install_interrupt_frame(&mut bus, &mut cpu, 0x0000, 0x0120);
    bios.dispatch_interrupt(0x13, &mut cpu, &mut bus, &mut disk, None);

    let flags = bus.read_u16(0x0124);
    assert_eq!(flags & 0x0001, 0, "CF should be cleared on success");

    let bytes_per_sector = bus.read_u16(table + 24);
    assert_eq!(bytes_per_sector, 2048);
}
