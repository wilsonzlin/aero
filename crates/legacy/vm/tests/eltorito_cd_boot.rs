#![allow(deprecated)]

use aero_cpu_core::state::gpr;
use firmware::bios::{Bios, BiosConfig, InMemoryDisk};
use vm::{CpuExit, Vm};

const ISO_SECTOR_SIZE: usize = 2048;

const LOAD_SEGMENT: u16 = 0x2000;
const BOOT_CATALOG_LBA: u32 = 20;
const BOOT_IMAGE_LBA: u32 = 21;

const BOOT_IMAGE_CODE: &[u8] = &[
    0xFA, // cli
    0x31, 0xC0, // xor ax, ax
    0x8E, 0xD8, // mov ds, ax
    0x8E, 0xC0, // mov es, ax
    0x8E, 0xD0, // mov ss, ax
    0xBC, 0x00, 0x7C, // mov sp, 0x7c00
    0xFC, // cld
    // DAP at 0000:0500 (size=0x10, count=1, buf=0000:0600, lba=1)
    0xC6, 0x06, 0x00, 0x05, 0x10, // mov byte [0x0500], 0x10
    0xC6, 0x06, 0x01, 0x05, 0x00, // mov byte [0x0501], 0
    0xC7, 0x06, 0x02, 0x05, 0x01, 0x00, // mov word [0x0502], 1
    0xC7, 0x06, 0x04, 0x05, 0x00, 0x06, // mov word [0x0504], 0x0600
    0xC7, 0x06, 0x06, 0x05, 0x00, 0x00, // mov word [0x0506], 0
    0x66, 0xC7, 0x06, 0x08, 0x05, 0x01, 0x00, 0x00, 0x00, // mov dword [0x0508], 1
    0x66, 0xC7, 0x06, 0x0C, 0x05, 0x00, 0x00, 0x00, 0x00, // mov dword [0x050c], 0
    0xB8, 0x00, 0x42, // mov ax, 0x4200
    0xB2, 0xE0, // mov dl, 0xE0
    0xBE, 0x00, 0x05, // mov si, 0x0500
    0xCD, 0x13, // int 13h
    0x72, 0x17, // jc fail
    0x81, 0x3E, 0x00, 0x06, 0x4D, 0x41, // cmp word [0x0600], 0x414d ("MA")
    0x75, 0x0F, // jne fail
    0x81, 0x3E, 0x02, 0x06, 0x52, 0x4B, // cmp word [0x0602], 0x4b52 ("RK")
    0x75, 0x07, // jne fail
    // success: write 'S' to COM1 and halt
    0xBA, 0xF8, 0x03, // mov dx, 0x3f8
    0xB0, 0x53, // mov al, 'S'
    0xEE, // out dx, al
    0xF4, // hlt
    // fail: write 'F' to COM1 and halt
    0xBA, 0xF8, 0x03, // mov dx, 0x3f8
    0xB0, 0x46, // mov al, 'F'
    0xEE, // out dx, al
    0xF4, // hlt
];

fn write_iso_sector(image: &mut [u8], lba: u32, sector: &[u8]) {
    assert!(sector.len() <= ISO_SECTOR_SIZE);
    let start = (lba as usize) * ISO_SECTOR_SIZE;
    let end = start + sector.len();
    image[start..end].copy_from_slice(sector);
}

fn eltorito_validation_entry() -> [u8; 32] {
    let mut entry = [0u8; 32];
    entry[0] = 0x01; // header id
    entry[1] = 0x00; // platform id (x86)
    entry[30] = 0x55;
    entry[31] = 0xAA;
    let id = b"AERO ELTORITO TEST";
    entry[4..4 + id.len()].copy_from_slice(id);

    let mut sum: u32 = 0;
    for i in 0..16 {
        let word = u16::from_le_bytes([entry[i * 2], entry[i * 2 + 1]]);
        sum = sum.wrapping_add(word as u32);
    }
    let checksum = (0u16).wrapping_sub(sum as u16);
    entry[28..30].copy_from_slice(&checksum.to_le_bytes());

    // Sanity check: checksum should make the full sum zero.
    let mut verify: u32 = 0;
    for i in 0..16 {
        let word = u16::from_le_bytes([entry[i * 2], entry[i * 2 + 1]]);
        verify = verify.wrapping_add(word as u32);
    }
    assert_eq!(verify as u16, 0);

    entry
}

fn eltorito_initial_entry(load_segment: u16, sector_count: u16, image_lba: u32) -> [u8; 32] {
    let mut entry = [0u8; 32];
    entry[0] = 0x88; // bootable
    entry[1] = 0x00; // no emulation
    entry[2..4].copy_from_slice(&load_segment.to_le_bytes());
    entry[6..8].copy_from_slice(&sector_count.to_le_bytes());
    entry[8..12].copy_from_slice(&image_lba.to_le_bytes());
    entry
}

fn build_minimal_eltorito_iso() -> Vec<u8> {
    // Layout:
    // - LBA 1: marker bytes read by the boot image
    // - LBA 16: Primary Volume Descriptor
    // - LBA 17: Boot Record Volume Descriptor (El Torito)
    // - LBA 18: Volume Descriptor Set Terminator
    // - LBA 20: Boot Catalog
    // - LBA 21: No-emulation boot image
    let total_lbas = BOOT_IMAGE_LBA as usize + 1;
    let mut iso = vec![0u8; total_lbas * ISO_SECTOR_SIZE];

    iso[ISO_SECTOR_SIZE..ISO_SECTOR_SIZE + 4].copy_from_slice(b"MARK");

    let mut pvd = [0u8; ISO_SECTOR_SIZE];
    pvd[0] = 0x01; // type: primary volume descriptor
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 0x01; // version
    let volume_space_size = total_lbas as u32;
    pvd[80..84].copy_from_slice(&volume_space_size.to_le_bytes());
    pvd[84..88].copy_from_slice(&volume_space_size.to_be_bytes());
    write_iso_sector(&mut iso, 16, &pvd);

    let mut boot_record = [0u8; ISO_SECTOR_SIZE];
    boot_record[0] = 0x00; // type: boot record
    boot_record[1..6].copy_from_slice(b"CD001");
    boot_record[6] = 0x01; // version
    let sys_id = b"EL TORITO SPECIFICATION";
    boot_record[7..7 + sys_id.len()].copy_from_slice(sys_id);
    for b in &mut boot_record[7 + sys_id.len()..7 + 32] {
        *b = b' ';
    }
    for b in &mut boot_record[39..71] {
        *b = b' ';
    }
    boot_record[71..75].copy_from_slice(&BOOT_CATALOG_LBA.to_le_bytes());
    write_iso_sector(&mut iso, 17, &boot_record);

    let mut term = [0u8; ISO_SECTOR_SIZE];
    term[0] = 0xFF; // terminator
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 0x01;
    write_iso_sector(&mut iso, 18, &term);

    let mut catalog = [0u8; ISO_SECTOR_SIZE];
    catalog[0..32].copy_from_slice(&eltorito_validation_entry());
    catalog[32..64].copy_from_slice(&eltorito_initial_entry(LOAD_SEGMENT, 4, BOOT_IMAGE_LBA));
    write_iso_sector(&mut iso, BOOT_CATALOG_LBA, &catalog);

    let mut boot_image = [0u8; ISO_SECTOR_SIZE];
    boot_image[..BOOT_IMAGE_CODE.len()].copy_from_slice(BOOT_IMAGE_CODE);
    write_iso_sector(&mut iso, BOOT_IMAGE_LBA, &boot_image);

    iso
}

#[test]
fn eltorito_cd_boot_reads_iso_sectors_via_int13_ext() {
    let iso = build_minimal_eltorito_iso();
    assert!(iso.len().is_multiple_of(512));

    let disk = InMemoryDisk::new(iso);
    let bios = Bios::new(BiosConfig {
        memory_size_bytes: 16 * 1024 * 1024,
        boot_drive: 0xE0,
        ..BiosConfig::default()
    });

    let mut vm = Vm::new(16 * 1024 * 1024, bios, disk);
    vm.reset();

    assert_eq!(
        vm.cpu.segments.cs.selector, LOAD_SEGMENT,
        "BIOS should transfer control to El Torito boot image"
    );
    assert_eq!(
        vm.cpu.rip(),
        0,
        "boot image entrypoint should be load_segment:0"
    );
    assert_eq!(
        vm.cpu.gpr[gpr::RDX] as u8,
        0xE0,
        "DL should contain the CD boot drive number"
    );

    for _ in 0..50_000 {
        if vm.step() == CpuExit::Halt {
            break;
        }
    }

    assert!(vm.cpu.halted, "boot image should terminate with HLT");
    assert_eq!(vm.serial_output(), &[b'S']);
}
