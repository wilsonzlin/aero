#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

const ISO9660_STANDARD_IDENTIFIER: &[u8; 5] = b"CD001";
const ISO9660_VERSION: u8 = 1;
const ISO_BLOCK_BYTES: usize = 2048;

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
fn boots_eltorito_install_media_iso() {
    let boot_catalog_lba = 20;
    let boot_image_lba = 21;
    let boot_image = [0x5A; ISO_BLOCK_BYTES];
    let iso = build_minimal_iso_no_emulation(
        boot_catalog_lba,
        boot_image_lba,
        &boot_image,
        /* load_segment */ 0x07C0,
        /* sector_count */ 4,
    );

    let mut m = Machine::new(MachineConfig::win7_storage(16 * 1024 * 1024)).unwrap();
    m.attach_install_media_iso_bytes(iso)
        .expect("failed to attach install media ISO");
    m.set_boot_drive(0xE0);
    m.reset();

    let cs = m.cpu().segments.cs.selector;
    let rip = m.cpu().rip();
    assert_eq!(cs, 0x07C0);
    assert_eq!(rip, 0);

    let load_addr = (0x07C0u64) << 4;
    let loaded = m.read_physical_bytes(load_addr, ISO_BLOCK_BYTES);
    assert_eq!(loaded, vec![0x5A; ISO_BLOCK_BYTES]);
}

