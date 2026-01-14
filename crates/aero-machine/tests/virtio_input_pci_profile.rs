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
        enable_virtio_input_tablet: true,
        // Keep deterministic and focused.
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let keyboard_profile = profile::VIRTIO_INPUT_KEYBOARD;
    let mouse_profile = profile::VIRTIO_INPUT_MOUSE;
    let tablet_profile = profile::VIRTIO_INPUT_TABLET;

    let keyboard = keyboard_profile.bdf;
    let mouse = mouse_profile.bdf;
    let tablet = tablet_profile.bdf;

    let vid_did = |p: aero_devices::pci::profile::PciDeviceProfile| -> u32 {
        (u32::from(p.device_id) << 16) | u32::from(p.vendor_id)
    };
    let subsys = |p: aero_devices::pci::profile::PciDeviceProfile| -> u32 {
        (u32::from(p.subsystem_id) << 16) | u32::from(p.subsystem_vendor_id)
    };

    // Vendor/device IDs: match the canonical PCI profile.
    assert_eq!(
        cfg_read(&mut m, keyboard, 0x00, 4),
        vid_did(keyboard_profile)
    );
    assert_eq!(cfg_read(&mut m, mouse, 0x00, 4), vid_did(mouse_profile));
    assert_eq!(cfg_read(&mut m, tablet, 0x00, 4), vid_did(tablet_profile));

    // Revision ID = 0x01 (contract v1).
    assert_eq!(
        cfg_read(&mut m, keyboard, 0x08, 1),
        u32::from(keyboard_profile.revision_id)
    );
    assert_eq!(
        cfg_read(&mut m, mouse, 0x08, 1),
        u32::from(mouse_profile.revision_id)
    );
    assert_eq!(
        cfg_read(&mut m, tablet, 0x08, 1),
        u32::from(tablet_profile.revision_id)
    );

    // Subsystem IDs distinguish keyboard vs mouse vs tablet.
    assert_eq!(
        cfg_read(&mut m, keyboard, 0x2c, 4),
        subsys(keyboard_profile)
    );
    assert_eq!(cfg_read(&mut m, mouse, 0x2c, 4), subsys(mouse_profile));
    assert_eq!(cfg_read(&mut m, tablet, 0x2c, 4), subsys(tablet_profile));

    // Function 0 (keyboard) must be marked as multi-function so OSes enumerate fn1.
    assert_eq!(
        cfg_read(&mut m, keyboard, 0x0e, 1),
        u32::from(keyboard_profile.header_type)
    );
    assert_eq!(
        cfg_read(&mut m, mouse, 0x0e, 1),
        u32::from(mouse_profile.header_type)
    );
    assert_eq!(
        cfg_read(&mut m, tablet, 0x0e, 1),
        u32::from(tablet_profile.header_type)
    );
}
