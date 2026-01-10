use super::{
    BIOS_SIZE, DEFAULT_INT_STUB_OFFSET, INT10_STUB_OFFSET, INT13_STUB_OFFSET, INT15_STUB_OFFSET,
    INT16_STUB_OFFSET, INT1A_STUB_OFFSET,
};

/// Build the 64KiB BIOS ROM image.
///
/// We only embed tiny interrupt stubs used for HLE dispatch:
/// `HLT; IRET`.
pub fn build_bios_rom() -> Vec<u8> {
    let mut rom = vec![0xFFu8; BIOS_SIZE];

    // Install a conventional x86 reset vector at F000:FFF0.
    //
    // Aero performs POST in host code, but guests and tooling may still expect
    // the reset vector to contain a FAR JMP instruction.
    //
    // Encoding: JMP FAR ptr16:16 => EA iw (offset) iw (segment)
    // Target: F000:E000.
    let reset_off = 0xFFF0usize;
    rom[reset_off + 0] = 0xEA;
    rom[reset_off + 1] = 0x00; // offset low
    rom[reset_off + 2] = 0xE0; // offset high (0xE000)
    rom[reset_off + 3] = 0x00; // segment low
    rom[reset_off + 4] = 0xF0; // segment high (0xF000)

    // Safe fallback at F000:E000: `cli; hlt; jmp $-2`.
    //
    // In a full-system integration this address is never reached because POST
    // is performed in host code, but it provides deterministic behavior if it is.
    let stub_off = 0xE000usize;
    rom[stub_off + 0] = 0xFA;
    rom[stub_off + 1] = 0xF4;
    rom[stub_off + 2] = 0xEB;
    rom[stub_off + 3] = 0xFE;

    let stub = [0xF4u8, 0xCFu8]; // HLT; IRET
    write_stub(&mut rom, DEFAULT_INT_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT10_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT13_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT15_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT16_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT1A_STUB_OFFSET, &stub);

    // Optional ROM signature (harmless and convenient for identification).
    rom[BIOS_SIZE - 2] = 0x55;
    rom[BIOS_SIZE - 1] = 0xAA;

    rom
}

fn write_stub(rom: &mut [u8], offset: u16, stub: &[u8]) {
    let off = offset as usize;
    let end = off + stub.len();
    rom[off..end].copy_from_slice(stub);
}
