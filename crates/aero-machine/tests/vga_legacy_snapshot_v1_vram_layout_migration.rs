#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;

fn patch_vga_state_to_legacy_vga_snapshot_v1(bytes: &mut [u8], legacy_payload: &[u8]) {
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

    for _ in 0..count {
        if off + 4 + 2 + 2 + 8 > devices_end || off + 4 + 2 + 2 + 8 > bytes.len() {
            break;
        }

        let id = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        off += 4;

        // version
        off += 2;
        // flags
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
            assert!(
                legacy_payload.len() <= len,
                "legacy payload ({} bytes) does not fit in existing VGA entry ({} bytes)",
                legacy_payload.len(),
                len
            );
            bytes[data_start..data_start + legacy_payload.len()].copy_from_slice(legacy_payload);
            return;
        }

        off = data_end;
    }

    panic!("missing VGA device state entry to patch");
}

#[test]
fn machine_restore_migrates_legacy_vga_snapshot_v1_vram_layout() {
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
    vm.write_physical_u32(vm.vbe_lfb_base(), 0x00FF_0000);

    // Build a legacy `VgaSnapshotV1` payload that simulates the pre-partition VRAM layout:
    // packed-pixel VBE framebuffer starts at vram[0..].
    let legacy_payload = {
        let vga = vm.vga().expect("machine should include VGA");
        let mut snap = vga.borrow().snapshot_v1();
        assert!(
            snap.vram.len() >= VBE_FRAMEBUFFER_OFFSET + 4,
            "unexpected vram length {}",
            snap.vram.len()
        );
        let px = [
            snap.vram[VBE_FRAMEBUFFER_OFFSET],
            snap.vram[VBE_FRAMEBUFFER_OFFSET + 1],
            snap.vram[VBE_FRAMEBUFFER_OFFSET + 2],
            snap.vram[VBE_FRAMEBUFFER_OFFSET + 3],
        ];
        snap.vram[0..4].copy_from_slice(&px);
        snap.vram[VBE_FRAMEBUFFER_OFFSET..VBE_FRAMEBUFFER_OFFSET + 4].fill(0);
        snap.encode()
    };

    let mut snap_bytes = vm.take_snapshot_full().unwrap();
    patch_vga_state_to_legacy_vga_snapshot_v1(&mut snap_bytes, &legacy_payload);

    let mut vm2 = Machine::new(cfg).unwrap();
    vm2.restore_snapshot_bytes(&snap_bytes).unwrap();

    vm2.display_present();
    assert_eq!(vm2.display_resolution(), (64, 64));
    assert_eq!(vm2.display_framebuffer()[0], 0xFF00_00FF);
}
