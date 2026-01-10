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

    let stub = [0xF4u8, 0xCFu8]; // HLT; IRET
    write_stub(&mut rom, DEFAULT_INT_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT10_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT13_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT15_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT16_STUB_OFFSET, &stub);
    write_stub(&mut rom, INT1A_STUB_OFFSET, &stub);

    rom
}

fn write_stub(rom: &mut [u8], offset: u16, stub: &[u8]) {
    let off = offset as usize;
    let end = off + stub.len();
    rom[off..end].copy_from_slice(stub);
}
