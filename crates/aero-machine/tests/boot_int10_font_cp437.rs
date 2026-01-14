use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bios::{build_bios_rom, BIOS_SEGMENT, VGA_FONT_8X16_OFFSET};

const RESULT_BASE: u64 = 0x0500;
const RESULT_OR: u64 = RESULT_BASE;
const RESULT_AND: u64 = RESULT_BASE + 1;
const RESULT_ES: u64 = RESULT_BASE + 2;
const RESULT_CX: u64 = RESULT_BASE + 4;
const RESULT_BP: u64 = RESULT_BASE + 6;

const GLYPH_CP437: u8 = 0xC4; // box drawing '─'

fn build_int10_font_cp437_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax  ; DS=0 so we can store results to known physical addresses.
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // mov ax, 0x0003 ; INT 10h AH=00h Set Video Mode (mode 03h)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x03, 0x00]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ax, 0x1130 ; INT 10h AH=11h AL=30h Get Font Information
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x30, 0x11]);
    i += 3;
    // mov bh, 0x06 ; request 8x16 font
    sector[i..i + 2].copy_from_slice(&[0xB7, 0x06]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Restore DS=0 (BIOS does not guarantee DS preservation).
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // Persist returned ES:BP and CX for host-side assertions.
    //
    // mov ax, es
    sector[i..i + 2].copy_from_slice(&[0x8C, 0xC0]);
    i += 2;
    // mov [0x0502], ax
    sector[i..i + 3].copy_from_slice(&[0xA3, 0x02, 0x05]);
    i += 3;
    // mov [0x0504], cx
    sector[i..i + 4].copy_from_slice(&[0x89, 0x0E, 0x04, 0x05]);
    i += 4;
    // mov [0x0506], bp
    sector[i..i + 4].copy_from_slice(&[0x89, 0x2E, 0x06, 0x05]);
    i += 4;

    // Load DS=ES (AX currently holds ES) so DS:SI can dereference the BIOS ROM font table.
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // Compute DS:SI = ES:(BP + (GLYPH_CP437 * CX)).
    // mov ax, GLYPH_CP437
    sector[i..i + 3].copy_from_slice(&[0xB8, GLYPH_CP437, 0x00]);
    i += 3;
    // mul cx ; DX:AX = AX * CX
    sector[i..i + 2].copy_from_slice(&[0xF7, 0xE1]);
    i += 2;
    // mov si, bp
    sector[i..i + 2].copy_from_slice(&[0x89, 0xEE]);
    i += 2;
    // add si, ax
    sector[i..i + 2].copy_from_slice(&[0x01, 0xC6]);
    i += 2;

    // cld ; ensure LODSB increments SI
    sector[i] = 0xFC;
    i += 1;

    // OR accumulator in BL, AND accumulator in BH.
    // xor bl, bl
    sector[i..i + 2].copy_from_slice(&[0x30, 0xDB]);
    i += 2;
    // mov bh, 0xFF
    sector[i..i + 2].copy_from_slice(&[0xB7, 0xFF]);
    i += 2;

    let loop_start = i;
    // lodsb ; AL = [DS:SI], SI++
    sector[i] = 0xAC;
    i += 1;
    // or bl, al
    sector[i..i + 2].copy_from_slice(&[0x08, 0xC3]);
    i += 2;
    // and bh, al
    sector[i..i + 2].copy_from_slice(&[0x20, 0xC7]);
    i += 2;
    // loop rel8
    sector[i] = 0xE2;
    i += 1;
    let next_ip = i + 1;
    let rel = (loop_start as i32 - next_ip as i32) as i8;
    sector[i] = rel as u8;
    i += 1;

    // Restore DS=0 so stores land in low RAM.
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // Store the OR accumulator at 0x0500 (required) and AND accumulator at 0x0501 (helps detect
    // unmapped ROM reads returning all 1s).
    // mov [0x0500], bl
    sector[i..i + 4].copy_from_slice(&[0x88, 0x1E, 0x00, 0x05]);
    i += 4;
    // mov [0x0501], bh
    sector[i..i + 4].copy_from_slice(&[0x88, 0x3E, 0x01, 0x05]);
    i += 4;

    // hlt
    sector[i] = 0xF4;
    i += 1;

    assert!(i <= 510, "boot sector too large (len={i})");
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("machine did not halt within budget");
}

#[test]
fn boot_int10_font_cp437_glyph_is_non_blank_and_rom_is_mapped() {
    let boot = build_int10_font_cp437_boot_sector();

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // OR-reduction of the glyph bytes must be non-zero => glyph isn't blank.
    let glyph_or = m.read_physical_u8(RESULT_OR);
    assert_ne!(glyph_or, 0, "CP437 glyph 0xC4 read as all-zero bytes");

    // AND-reduction must not be 0xFF => detect unmapped memory reads returning all 1s.
    let glyph_and = m.read_physical_u8(RESULT_AND);
    assert_ne!(
        glyph_and, 0xFF,
        "CP437 glyph bytes read as all-ones (BIOS ROM not mapped/readable?)"
    );

    // Validate the INT10 handler returned a BIOS ROM pointer for the 8x16 font table.
    let es = m.read_physical_u16(RESULT_ES);
    let cx = m.read_physical_u16(RESULT_CX);
    let bp = m.read_physical_u16(RESULT_BP);

    assert_eq!(es, BIOS_SEGMENT);
    assert_eq!(cx, 16);
    assert_eq!(bp, VGA_FONT_8X16_OFFSET);

    // Compare the glyph bytes against the firmware ROM image to ensure the ROM is mapped and
    // readable, and that the INT 10h handler returned a pointer into the correct table.
    let glyph_paddr = ((es as u64) << 4) + (bp as u64) + u64::from(GLYPH_CP437) * u64::from(cx);
    let mut glyph = [0u8; 16];
    for (i, slot) in glyph.iter_mut().enumerate() {
        *slot = m.read_physical_u8(glyph_paddr + i as u64);
    }

    let rom = build_bios_rom();
    let rom_off = (bp as usize) + (usize::from(GLYPH_CP437) * (cx as usize));
    assert_eq!(&glyph, &rom[rom_off..rom_off + 16]);
}
