use aero_machine::{Machine, MachineConfig};

use firmware::bios::{BIOS_ALIAS_BASE, BIOS_SIZE, RESET_VECTOR_ALIAS_PHYS};

#[test]
fn bios_rom_is_mapped_at_reset_vector_alias() {
    // Keep this machine minimal: we only care about the canonical Machine physical memory bus
    // mapping the BIOS ROM at the architectural reset vector alias (top of 4GiB).
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // The BIOS ROM reset vector must contain a FAR JMP instruction:
    // `JMP FAR F000:E000` => EA 00 E0 00 F0 (offset little-endian, then segment).
    let reset = m.read_physical_bytes(RESET_VECTOR_ALIAS_PHYS, 5);
    assert_eq!(reset.as_slice(), &[0xEA, 0x00, 0xE0, 0x00, 0xF0]);

    // Also verify the BIOS ROM signature at the end of the aliased mapping.
    let sig_addr = BIOS_ALIAS_BASE + (BIOS_SIZE as u64) - 2;
    let sig = m.read_physical_bytes(sig_addr, 2);
    assert_eq!(sig.as_slice(), &[0x55, 0xAA]);
}
