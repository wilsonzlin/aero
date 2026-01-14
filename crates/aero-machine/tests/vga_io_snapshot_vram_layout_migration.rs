#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;

fn patch_vga_state_to_vgad_v1_0(bytes: &mut [u8]) {
    let index = snapshot::inspect_snapshot(&mut Cursor::new(&*bytes)).expect("inspect snapshot");
    let devices = index
        .sections
        .iter()
        .find(|s| s.id == snapshot::SectionId::DEVICES)
        .expect("missing DEVICES section");

    let mut off = usize::try_from(devices.offset).expect("devices offset fits usize");
    let devices_end = off
        .checked_add(usize::try_from(devices.len).expect("devices len fits usize"))
        .expect("devices end overflow");

    assert!(off + 4 <= bytes.len(), "devices section out of range");
    let count = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
    off += 4;
    let mut patched = false;

    for _ in 0..count {
        if off + 4 + 2 + 2 + 8 > devices_end || off + 4 + 2 + 2 + 8 > bytes.len() {
            break;
        }

        let id = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        off += 4;
        let _version = u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap());
        off += 2;

        let flags_pos = off;
        let _flags = u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap());
        off += 2;

        let len = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap()) as usize;
        off += 8;

        let data_start = off;
        let data_end = data_start
            .checked_add(len)
            .expect("device data end overflow");
        if data_end > devices_end || data_end > bytes.len() {
            break;
        }

        if id == snapshot::DeviceId::VGA.0 {
            // Outer DeviceState flags must match the io-snapshot header's device_version.minor.
            bytes[flags_pos..flags_pos + 2].copy_from_slice(&0u16.to_le_bytes());

            // Patch the embedded io-snapshot header to device_version 1.0.
            let data = &mut bytes[data_start..data_end];
            assert!(
                data.len() >= 16,
                "expected io-snapshot header, got {} bytes",
                data.len()
            );
            data[14..16].copy_from_slice(&0u16.to_le_bytes()); // device_version.minor

            // Patch VRAM bytes to simulate the pre-partition layout: VBE framebuffer begins at
            // vram[0..] instead of vram[VBE_FRAMEBUFFER_OFFSET..].
            //
            // We specifically move the first pixel from the new VBE region into the legacy base and
            // clear it at the new location so the restore logic must perform the migration for the
            // pixel to be visible.
            let mut off = 16usize; // io-snapshot header length
            while off + 6 <= data.len() {
                let tag = u16::from_le_bytes([data[off], data[off + 1]]);
                let len = u32::from_le_bytes([
                    data[off + 2],
                    data[off + 3],
                    data[off + 4],
                    data[off + 5],
                ]) as usize;
                let value_start = off + 6;
                let value_end = value_start
                    .checked_add(len)
                    .expect("tlv value end overflow");
                if value_end > data.len() {
                    break;
                }

                const TAG_VRAM: u16 = 20;
                if tag == TAG_VRAM {
                    let vram = &mut data[value_start..value_end];
                    assert!(
                        vram.len() > VBE_FRAMEBUFFER_OFFSET + 4,
                        "unexpected VGA VRAM length {}",
                        vram.len()
                    );
                    let src = VBE_FRAMEBUFFER_OFFSET;
                    let px = [vram[src], vram[src + 1], vram[src + 2], vram[src + 3]];
                    vram[0..4].copy_from_slice(&px);
                    vram[src..src + 4].fill(0);
                    break;
                }

                off = value_end;
            }

            patched = true;
            break;
        }

        off = data_end;
    }

    assert!(patched, "did not find VGA DeviceState entry to patch");
}

#[test]
fn machine_restore_migrates_vgad_v1_0_vram_layout() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg.clone()).unwrap();

    // Program Bochs VBE_DISPI to 64x64x32 with LFB enabled.
    vm.io_write(0x01CE, 2, 0x0001);
    vm.io_write(0x01CF, 2, 64);
    vm.io_write(0x01CE, 2, 0x0002);
    vm.io_write(0x01CF, 2, 64);
    vm.io_write(0x01CE, 2, 0x0003);
    vm.io_write(0x01CF, 2, 32);
    vm.io_write(0x01CE, 2, 0x0004);
    vm.io_write(0x01CF, 2, 0x0041);

    // Write one red pixel at (0,0) in packed 32bpp BGRX.
    vm.write_physical_u32(u64::from(vm.vbe_lfb_base()), 0x00FF_0000);

    let mut snap = vm.take_snapshot_full().unwrap();
    patch_vga_state_to_vgad_v1_0(&mut snap);

    let mut vm2 = Machine::new(cfg).unwrap();
    vm2.restore_snapshot_bytes(&snap).unwrap();

    vm2.display_present();
    assert_eq!(vm2.display_resolution(), (64, 64));
    assert_eq!(vm2.display_framebuffer()[0], 0xFF00_00FF);
}
