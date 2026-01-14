#![cfg(not(target_arch = "wasm32"))]

use std::io::{Cursor, Read, Seek, SeekFrom};

use aero_gpu_vga::{PortIO as _, VgaDevice};
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_snapshot as snapshot;
use aero_snapshot::io_snapshot_bridge::apply_io_snapshot_to_device;
use firmware::bda::{BDA_CURSOR_POS_PAGE0_ADDR, BDA_CURSOR_SHAPE_ADDR, BDA_SCREEN_COLS_ADDR};
use pretty_assertions::assert_eq;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest never reached HLT");
}

fn read_crtc_cursor_regs(m: &mut Machine) -> (u8, u8, u16) {
    m.io_write(0x3D4, 1, 0x0A);
    let start = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0B);
    let end = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0E);
    let hi = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0F);
    let lo = m.io_read(0x3D5, 1) as u8;
    (start, end, ((hi as u16) << 8) | (lo as u16))
}

fn write_crtc_cursor_regs(m: &mut Machine, start: u8, end: u8, pos: u16) {
    m.io_write(0x3D4, 1, 0x0A);
    m.io_write(0x3D5, 1, u32::from(start));
    m.io_write(0x3D4, 1, 0x0B);
    m.io_write(0x3D5, 1, u32::from(end));
    m.io_write(0x3D4, 1, 0x0E);
    m.io_write(0x3D5, 1, u32::from((pos >> 8) as u8));
    m.io_write(0x3D4, 1, 0x0F);
    m.io_write(0x3D5, 1, u32::from((pos & 0x00FF) as u8));
}

fn build_int10_set_cursor_boot_sector(row: u8, col: u8, start: u8, end: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ah, 0x02  ; INT 10h AH=02h Set Cursor Position
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x02]);
    i += 2;
    // mov bh, 0x00  ; page 0
    sector[i..i + 2].copy_from_slice(&[0xB7, 0x00]);
    i += 2;
    // mov dh, row
    sector[i..i + 2].copy_from_slice(&[0xB6, row]);
    i += 2;
    // mov dl, col
    sector[i..i + 2].copy_from_slice(&[0xB2, col]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ah, 0x01  ; INT 10h AH=01h Set Cursor Shape
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x01]);
    i += 2;
    // mov ch, start
    sector[i..i + 2].copy_from_slice(&[0xB5, start]);
    i += 2;
    // mov cl, end
    sector[i..i + 2].copy_from_slice(&[0xB1, end]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn decode_vga_snapshot(bytes: &[u8]) -> snapshot::DeviceState {
    let index = snapshot::inspect_snapshot(&mut Cursor::new(bytes)).unwrap();
    let devices_section = index
        .sections
        .iter()
        .find(|s| s.id == snapshot::SectionId::DEVICES)
        .expect("missing DEVICES section");

    let mut cursor = Cursor::new(bytes);
    cursor
        .seek(SeekFrom::Start(devices_section.offset))
        .unwrap();
    let mut r = cursor.take(devices_section.len);

    let count = {
        let mut buf = [0u8; 4];
        r.read_exact(&mut buf).unwrap();
        u32::from_le_bytes(buf) as usize
    };

    for _ in 0..count {
        let dev =
            snapshot::DeviceState::decode(&mut r, snapshot::limits::MAX_DEVICE_ENTRY_LEN).unwrap();
        if dev.id == snapshot::DeviceId::VGA {
            return dev;
        }
    }

    panic!("missing VGA device state entry");
}

#[test]
fn snapshot_restore_resyncs_text_cursor_from_bda() {
    // Keep the snapshot small and focused: no PC platform devices.
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    // Use an INT 10h boot sector to update the BDA cursor position/shape.
    let boot = build_int10_set_cursor_boot_sector(5, 10, 0x20, 0x07);
    let mut m = Machine::new(cfg.clone()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Assert BDA cursor state is non-default so the test is meaningful.
    let cols = m.read_physical_u16(BDA_SCREEN_COLS_ADDR).max(1);
    let pos_word = m.read_physical_u16(BDA_CURSOR_POS_PAGE0_ADDR);
    let shape_word = m.read_physical_u16(BDA_CURSOR_SHAPE_ADDR);
    let row = (pos_word >> 8) as u8;
    let col = (pos_word & 0x00FF) as u8;
    let expected_pos = u16::from(row)
        .saturating_mul(cols)
        .saturating_add(u16::from(col));
    let expected_start = (shape_word >> 8) as u8;
    let expected_end = (shape_word & 0x00FF) as u8;
    assert_eq!((row, col), (5, 10));
    assert_eq!((expected_start, expected_end), (0x20, 0x07));

    // Corrupt the VGA cursor registers (as if restoring an old snapshot taken before the BDA->CRTC
    // sync existed), without updating the BDA.
    write_crtc_cursor_regs(&mut m, 0x00, 0x0F, 0);
    assert_eq!(read_crtc_cursor_regs(&mut m), (0x00, 0x0F, 0));

    // Take a snapshot that contains inconsistent cursor state.
    let snap = m.take_snapshot_full().unwrap();

    // Prove the snapshot's VGA device state is "wrong" (cursor regs do not match the BDA).
    let vga_state = decode_vga_snapshot(&snap);
    let mut vga = VgaDevice::new();
    apply_io_snapshot_to_device(&vga_state, &mut vga).unwrap();
    vga.port_write(0x3D4, 1, 0x0A);
    let snap_cursor_start = vga.port_read(0x3D5, 1) as u8;
    vga.port_write(0x3D4, 1, 0x0B);
    let snap_cursor_end = vga.port_read(0x3D5, 1) as u8;
    vga.port_write(0x3D4, 1, 0x0E);
    let hi = vga.port_read(0x3D5, 1) as u8;
    vga.port_write(0x3D4, 1, 0x0F);
    let lo = vga.port_read(0x3D5, 1) as u8;
    let snap_cursor_pos = ((hi as u16) << 8) | (lo as u16);
    assert_eq!(
        (snap_cursor_start, snap_cursor_end, snap_cursor_pos),
        (0x00, 0x0F, 0)
    );

    // Restore into a fresh machine: post-restore logic should resync VGA CRTC cursor regs from the
    // BDA.
    let mut m2 = Machine::new(cfg).unwrap();
    m2.restore_snapshot_bytes(&snap).unwrap();

    let (start, end, pos) = read_crtc_cursor_regs(&mut m2);
    assert_eq!(
        (start, end, pos),
        (expected_start, expected_end, expected_pos)
    );
}
