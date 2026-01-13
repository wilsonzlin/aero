use aero_cpu_core::state::gpr;
use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bios::{BIOS_SEGMENT, VGA_FONT_8X16_OFFSET};

const CP437_GLYPH: u8 = 0xC3; // "├"
const GLYPH_HEIGHT_BYTES: u16 = 16;

fn build_int10_get_font_and_read_cp437_glyph_boot_sector() -> [u8; 512] {
    // This boot sector:
    // 1. Calls INT 10h AX=1130h (Get Font Information) requesting the 8x16 ROM font table.
    // 2. Reads the first byte of the CP437 box-drawing glyph at index 0xC3 ("├" in CP437).
    // 3. Stores that byte to physical address 0x0500.
    // 4. Halts.
    //
    // The host-side test asserts the value written at 0x0500 is non-zero, which validates that:
    // - the BIOS ROM is mapped into guest address space,
    // - INT 10h returns a correct, guest-visible ES:BP pointer, and
    // - the embedded ROM font includes non-blank CP437 drawing glyphs.
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // mov ax, 0x1130  ; AH=11h AL=30h Get Font Information
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x30, 0x11]);
    i += 3;
    // mov bh, 0x06    ; request 8x16 font (any non-0x03 value uses 8x16 in this BIOS)
    sector[i..i + 2].copy_from_slice(&[0xB7, 0x06]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // ES:BP now points at the start of the font table. Each glyph is 16 bytes.
    // add bp, 0x0C30  ; 0xC3 * 16
    sector[i..i + 4].copy_from_slice(&[0x81, 0xC5, 0x30, 0x0C]);
    i += 4;
    // mov al, [es:bp]
    sector[i..i + 4].copy_from_slice(&[0x26, 0x8A, 0x46, 0x00]);
    i += 4;

    // Ensure DS=0 so [0x0500] is physical 0x0500.
    // xor bx, bx
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // mov ds, bx
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xDB]);
    i += 2;

    // mov [0x0500], al
    sector[i..i + 3].copy_from_slice(&[0xA2, 0x00, 0x05]);
    i += 3;

    // hlt
    sector[i] = 0xF4;

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
fn boot_int10_font_cp437() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(build_int10_get_font_and_read_cp437_glyph_boot_sector().to_vec())
        .unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Validate that INT 10h returned a pointer into the BIOS ROM segment, and that CX advertises
    // the expected glyph height (bytes per character) for the 8x16 font.
    assert_eq!(
        m.cpu().segments.es.selector, BIOS_SEGMENT,
        "INT 10h AX=1130h returned ES != BIOS segment"
    );
    let bp = (m.cpu().gpr[gpr::RBP] & 0xFFFF) as u16;
    let expected_bp =
        VGA_FONT_8X16_OFFSET.wrapping_add(u16::from(CP437_GLYPH).saturating_mul(GLYPH_HEIGHT_BYTES));
    assert_eq!(
        bp, expected_bp,
        "INT 10h AX=1130h returned unexpected font pointer (ES:BP)"
    );
    let cx = (m.cpu().gpr[gpr::RCX] & 0xFFFF) as u16;
    assert_eq!(
        cx, GLYPH_HEIGHT_BYTES,
        "INT 10h AX=1130h returned unexpected glyph height in CX"
    );

    let glyph_byte = m.read_physical_u8(0x0500);
    assert!(
        glyph_byte != 0 && glyph_byte != 0xFF,
        "INT 10h AX=1130h returned an unexpected glyph byte 0x{glyph_byte:02X} \
         (0x00 = blank glyph; 0xFF usually indicates open-bus/unmapped ROM reads)"
    );
}
