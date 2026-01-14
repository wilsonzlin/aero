#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_core::state::gpr;
use aero_machine::{Machine, RunExit};
use aero_storage::{MemBackend, RawDisk};

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("machine did not halt within slice budget");
}

fn build_minimal_eltorito_iso(boot_char: u8) -> Vec<u8> {
    const ISO_SECTOR: usize = 2048;
    const TOTAL_SECTORS: usize = 32; // enough for system area + descriptors + catalog + boot image

    // This ISO builder mirrors the firmware crate's El Torito unit test image layout:
    // `crates/firmware/tests/el_torito_boot.rs`.
    fn lba_offset(lba: u32) -> usize {
        (lba as usize) * ISO_SECTOR
    }
    fn write_volume_descriptor_header(image: &mut [u8], lba: u32, ty: u8) {
        let base = lba_offset(lba);
        image[base] = ty;
        image[base + 1..base + 6].copy_from_slice(b"CD001");
        image[base + 6] = 0x01; // version
    }
    fn write_space_padded_ascii(image: &mut [u8], offset: usize, len: usize, s: &str) {
        let bytes = s.as_bytes();
        let copy_len = bytes.len().min(len);
        image[offset..offset + len].fill(b' ');
        image[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
    }
    fn write_padded_ascii(image: &mut [u8], offset: usize, len: usize, s: &str) {
        let bytes = s.as_bytes();
        let copy_len = bytes.len().min(len);
        image[offset..offset + len].fill(0);
        image[offset..offset + copy_len].copy_from_slice(&bytes[..copy_len]);
    }
    fn write_u16_le(image: &mut [u8], offset: usize, v: u16) {
        image[offset..offset + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn write_u32_le(image: &mut [u8], offset: usize, v: u32) {
        image[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn el_torito_validation_checksum(entry: &[u8; 32]) -> u16 {
        // Checksum is the two's complement of the 16-bit sum of the whole entry, treating the
        // checksum field itself as zero.
        let mut sum: u32 = 0;
        for i in (0..32).step_by(2) {
            let word = if i == 28 {
                0u16
            } else {
                u16::from_le_bytes([entry[i], entry[i + 1]])
            };
            sum = sum.wrapping_add(u32::from(word));
        }
        (sum as u16).wrapping_neg()
    }
    fn write_el_torito_boot_catalog(image: &mut [u8], boot_catalog_lba: u32, boot_image_lba: u32) {
        let base = lba_offset(boot_catalog_lba);
        let catalog = &mut image[base..base + ISO_SECTOR];
        catalog.fill(0);

        // Validation Entry (32 bytes).
        let mut validation = [0u8; 32];
        validation[0] = 0x01; // header id
        validation[1] = 0x00; // platform id (x86)
        write_padded_ascii(&mut validation, 4, 24, "AERO ELTORITO TEST");
        validation[30] = 0x55;
        validation[31] = 0xAA;
        let checksum = el_torito_validation_checksum(&validation);
        validation[28..30].copy_from_slice(&checksum.to_le_bytes());
        catalog[0..32].copy_from_slice(&validation);

        // Default initial entry (32 bytes): bootable + no-emulation, load segment 0 => 0x07C0.
        let entry_off = 32;
        catalog[entry_off] = 0x88; // bootable
        catalog[entry_off + 1] = 0x00; // no emulation
        write_u16_le(catalog, entry_off + 2, 0x0000); // load segment
        write_u16_le(catalog, entry_off + 6, 4); // sector count (4 * 512 = 2048 bytes)
        write_u32_le(catalog, entry_off + 8, boot_image_lba);
    }

    let mut iso = vec![0u8; TOTAL_SECTORS * ISO_SECTOR];
    let boot_catalog_lba: u32 = 20;
    let boot_image_lba: u32 = 21;

    // Primary Volume Descriptor (LBA 16).
    write_volume_descriptor_header(&mut iso, 16, 0x01);

    // El Torito Boot Record Volume Descriptor (LBA 17).
    write_volume_descriptor_header(&mut iso, 17, 0x00);
    write_space_padded_ascii(&mut iso, lba_offset(17) + 7, 32, "EL TORITO SPECIFICATION");
    // Boot catalog pointer (little-endian LBA) at offset 0x47 (71).
    write_u32_le(&mut iso, lba_offset(17) + 0x47, boot_catalog_lba);

    // Volume Descriptor Set Terminator (LBA 18).
    write_volume_descriptor_header(&mut iso, 18, 0xFF);

    // Boot catalog (LBA 20).
    write_el_torito_boot_catalog(&mut iso, boot_catalog_lba, boot_image_lba);

    // Boot image at LBA 21 (one 2048-byte sector).
    {
        let mut boot_sector = [0u8; 512];
        let mut i = 0usize;

        // mov dx, 0x3f8
        boot_sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;
        // mov al, imm8
        boot_sector[i..i + 2].copy_from_slice(&[0xB0, boot_char]);
        i += 2;
        // out dx, al
        boot_sector[i] = 0xEE;
        i += 1;
        // hlt; jmp $-3 (stay halted even if interrupts wake us)
        boot_sector[i..i + 3].copy_from_slice(&[0xF4, 0xEB, 0xFD]);

        boot_sector[510] = 0x55;
        boot_sector[511] = 0xAA;
        let base = lba_offset(boot_image_lba);
        iso[base..base + 512].copy_from_slice(&boot_sector);
    }

    iso
}

#[test]
fn machine_win7_install_helper_sets_boot_drive_e0_and_boots_eltorito_iso() {
    let iso_bytes = build_minimal_eltorito_iso(b'I');
    let iso = RawDisk::open(MemBackend::from_vec(iso_bytes)).unwrap();

    let mut m = Machine::new_with_win7_install(2 * 1024 * 1024, Box::new(iso)).unwrap();

    // Firmware should enter the ISO boot image with DL=0xE0 (first CD-ROM drive number).
    assert_eq!(m.cpu().gpr[gpr::RDX] as u8, 0xE0);

    run_until_halt(&mut m);
    assert_eq!(m.take_serial_output(), vec![b'I']);
}
