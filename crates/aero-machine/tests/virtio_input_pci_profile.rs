#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciBdf};
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1F) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    m.io_read(0xCFC + (offset & 3), size)
}

#[test]
fn virtio_input_pci_ids_match_aero_w7_virtio_contract_v1() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let keyboard = profile::VIRTIO_INPUT_KEYBOARD.bdf;
    let mouse = profile::VIRTIO_INPUT_MOUSE.bdf;

    // Vendor/device IDs: virtio-pci modern input: 1AF4:1052.
    assert_eq!(cfg_read(&mut m, keyboard, 0x00, 4), 0x1052_1af4);
    assert_eq!(cfg_read(&mut m, mouse, 0x00, 4), 0x1052_1af4);

    // Revision ID = 0x01 (contract v1).
    assert_eq!(cfg_read(&mut m, keyboard, 0x08, 1), 0x01);
    assert_eq!(cfg_read(&mut m, mouse, 0x08, 1), 0x01);

    // Subsystem IDs distinguish keyboard vs mouse.
    assert_eq!(cfg_read(&mut m, keyboard, 0x2c, 4), 0x0010_1af4);
    assert_eq!(cfg_read(&mut m, mouse, 0x2c, 4), 0x0011_1af4);

    // Function 0 (keyboard) must be marked as multi-function so OSes enumerate fn1.
    assert_eq!(cfg_read(&mut m, keyboard, 0x0e, 1), 0x80);
    assert_eq!(cfg_read(&mut m, mouse, 0x0e, 1), 0x00);
}

