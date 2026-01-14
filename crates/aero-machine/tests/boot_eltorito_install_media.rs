#![cfg(not(target_arch = "wasm32"))]

use aero_machine::{Machine, MachineConfig, RunExit};
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

fn rel8(from_next: u16, to: u16) -> u8 {
    let diff = i32::from(to) - i32::from(from_next);
    assert!(
        (-128..=127).contains(&diff),
        "rel8 out of range: from_next=0x{from_next:04x} to=0x{to:04x} diff={diff}"
    );
    (diff as i8) as u8
}

#[test]
fn boots_eltorito_install_media_iso() {
    let boot_catalog_lba = 20;
    let boot_image_lba = 21;

    // Boot image: perform an INT 13h AH=42h extended read from the CD boot drive (DL=0xE0),
    // validate the marker bytes, then write a single byte to COM1 and halt.
    //
    // This validates both:
    // - El Torito POST load (boot image bytes copied into RAM), and
    // - BIOS INT 13h routing for CD-ROM reads from the boot image.
    const SUCCESS: u8 = b'S';
    const FAIL: u8 = b'F';
    const DATA_LBA_2048: u32 = 22;

    let boot_image = {
        let mut img = [0u8; ISO_BLOCK_BYTES];
        let mut i = 0usize;

        // xor ax, ax
        img[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
        i += 2;
        // mov ds, ax
        img[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
        i += 2;

        // mov si, imm16  (patched after DAP placement is known)
        img[i] = 0xBE;
        i += 1;
        let si_imm_pos = i;
        i += 2;

        // mov ah, 0x42
        img[i..i + 2].copy_from_slice(&[0xB4, 0x42]);
        i += 2;
        // int 0x13
        img[i..i + 2].copy_from_slice(&[0xCD, 0x13]);
        i += 2;

        // jc fail
        img[i] = 0x72;
        i += 1;
        let jc_off_pos = i;
        i += 1;

        // mov bx, 0x0500
        img[i..i + 3].copy_from_slice(&[0xBB, 0x00, 0x05]);
        i += 3;

        // cmp word ptr [bx], 0x4443 ("CD")
        img[i..i + 4].copy_from_slice(&[0x81, 0x3F, 0x43, 0x44]);
        i += 4;
        // jne fail
        img[i] = 0x75;
        i += 1;
        let jne1_off_pos = i;
        i += 1;

        // cmp word ptr [bx+2], 0x4B4F ("OK")
        img[i..i + 5].copy_from_slice(&[0x81, 0x7F, 0x02, 0x4F, 0x4B]);
        i += 5;
        // jne fail
        img[i] = 0x75;
        i += 1;
        let jne2_off_pos = i;
        i += 1;

        // success: write one byte to serial and halt.
        // mov dx, 0x3f8
        img[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;
        // mov al, SUCCESS
        img[i..i + 2].copy_from_slice(&[0xB0, SUCCESS]);
        i += 2;
        // out dx, al
        img[i] = 0xEE;
        i += 1;
        // cli; hlt
        img[i..i + 2].copy_from_slice(&[0xFA, 0xF4]);
        i += 2;

        // fail label.
        let fail_off = i;

        // mov dx, 0x3f8
        img[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;
        // mov al, FAIL
        img[i..i + 2].copy_from_slice(&[0xB0, FAIL]);
        i += 2;
        // out dx, al
        img[i] = 0xEE;
        i += 1;
        // cli; hlt
        img[i..i + 2].copy_from_slice(&[0xFA, 0xF4]);
        i += 2;

        // Disk Address Packet (DAP) for INT 13h extensions read (AH=42h).
        let dap_off = i;
        assert!(dap_off + 16 <= ISO_BLOCK_BYTES, "boot image DAP overflow");
        // DAP layout:
        // 0: size (0x10)
        // 1: reserved (0)
        // 2: sector count (u16) in 2048-byte sectors for CD-ROM drive numbers
        // 4: buffer offset (u16)
        // 6: buffer segment (u16)
        // 8: starting LBA (u64) in 2048-byte sectors
        img[i] = 0x10;
        img[i + 1] = 0x00;
        img[i + 2..i + 4].copy_from_slice(&1u16.to_le_bytes());
        img[i + 4..i + 6].copy_from_slice(&0x0500u16.to_le_bytes());
        img[i + 6..i + 8].copy_from_slice(&0u16.to_le_bytes());
        img[i + 8..i + 16].copy_from_slice(&u64::from(DATA_LBA_2048).to_le_bytes());

        // Patch `mov si, imm16` to point at the DAP.
        //
        // El Torito no-emulation boot uses load segment 0x07C0 by default, so the boot image is
        // loaded at physical 0x7C00. We keep DS=0, so DS:SI is an absolute physical pointer.
        let dap_addr = 0x7C00u16 + u16::try_from(dap_off).unwrap();
        img[si_imm_pos..si_imm_pos + 2].copy_from_slice(&dap_addr.to_le_bytes());

        // Patch branches to `fail`.
        img[jc_off_pos] = rel8(u16::try_from(jc_off_pos + 1).unwrap(), fail_off as u16);
        img[jne1_off_pos] = rel8(u16::try_from(jne1_off_pos + 1).unwrap(), fail_off as u16);
        img[jne2_off_pos] = rel8(u16::try_from(jne2_off_pos + 1).unwrap(), fail_off as u16);

        // Standard signature (not required by the El Torito boot path, but common).
        img[510] = 0x55;
        img[511] = 0xAA;

        img
    };

    let mut iso = build_minimal_iso_no_emulation(
        boot_catalog_lba,
        boot_image_lba,
        &boot_image,
        /* load_segment */ 0x07C0,
        /* sector_count */ 4,
    );

    // Data block that the boot image reads via INT 13h AH=42h (CD-ROM semantics: 2048-byte sectors).
    let mut data = [0u8; ISO_BLOCK_BYTES];
    data[0..4].copy_from_slice(b"CDOK");
    write_iso_block(&mut iso, DATA_LBA_2048 as usize, &data);

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
    assert_eq!(loaded, boot_image.to_vec());

    run_until_halt(&mut m);
    assert_eq!(m.take_serial_output(), vec![SUCCESS]);
}
