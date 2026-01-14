use aero_machine::{Machine, MachineConfig, RunExit};
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use pretty_assertions::assert_eq;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => break,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
}

fn rel8(from_next: u16, to: u16) -> u8 {
    let diff = i32::from(to) - i32::from(from_next);
    assert!(
        (-128..=127).contains(&diff),
        "rel8 out of range: from_next=0x{from_next:04x} to=0x{to:04x} diff={diff}"
    );
    (diff as i8) as u8
}

fn build_int13_ext_read_boot_sector(success: u8, fail: u8) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // mov si, imm16  (patched after DAP placement is known)
    sector[i] = 0xBE;
    i += 1;
    let si_imm_pos = i;
    i += 2;

    // mov ah, 0x42
    sector[i..i + 2].copy_from_slice(&[0xB4, 0x42]);
    i += 2;
    // int 0x13
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x13]);
    i += 2;

    // jc fail
    sector[i] = 0x72;
    i += 1;
    let jc_off_pos = i;
    i += 1;

    // mov bx, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x00, 0x05]);
    i += 3;

    // cmp word ptr [bx], 0x414D  ("MA")
    sector[i..i + 4].copy_from_slice(&[0x81, 0x3F, 0x4D, 0x41]);
    i += 4;
    // jne fail
    sector[i] = 0x75;
    i += 1;
    let jne1_off_pos = i;
    i += 1;

    // cmp word ptr [bx+2], 0x4B52 ("RK")
    sector[i..i + 5].copy_from_slice(&[0x81, 0x7F, 0x02, 0x52, 0x4B]);
    i += 5;
    // jne fail
    sector[i] = 0x75;
    i += 1;
    let jne2_off_pos = i;
    i += 1;

    // success: write one byte to serial and halt.
    // mov dx, 0x3f8
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
    i += 3;
    // mov al, success
    sector[i..i + 2].copy_from_slice(&[0xB0, success]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // hlt
    sector[i] = 0xF4;
    i += 1;

    // fail label.
    let fail_off = i;

    // mov dx, 0x3f8
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
    i += 3;
    // mov al, fail
    sector[i..i + 2].copy_from_slice(&[0xB0, fail]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;
    // hlt
    sector[i] = 0xF4;
    i += 1;

    // Disk Address Packet (DAP) for INT 13h extensions read (AH=42h).
    let dap_off = i;
    assert!(dap_off + 16 <= 510, "boot sector DAP overflow");

    // DAP layout:
    // 0: size (0x10)
    // 1: reserved (0)
    // 2: sector count (u16)
    // 4: buffer offset (u16)
    // 6: buffer segment (u16)
    // 8: starting LBA (u64)
    sector[i] = 0x10;
    sector[i + 1] = 0x00;
    sector[i + 2..i + 4].copy_from_slice(&1u16.to_le_bytes());
    sector[i + 4..i + 6].copy_from_slice(&0x0500u16.to_le_bytes());
    sector[i + 6..i + 8].copy_from_slice(&0u16.to_le_bytes());
    sector[i + 8..i + 16].copy_from_slice(&1u64.to_le_bytes());

    // Patch `mov si, imm16` to point at the DAP (DS=0 after BIOS boot).
    let dap_addr = 0x7C00u16 + u16::try_from(dap_off).unwrap();
    sector[si_imm_pos..si_imm_pos + 2].copy_from_slice(&dap_addr.to_le_bytes());

    // Patch branches to `fail`.
    let fail_addr = 0x7C00u16 + u16::try_from(fail_off).unwrap();
    sector[jc_off_pos] = rel8(
        0x7C00u16 + u16::try_from(jc_off_pos + 1).unwrap(),
        fail_addr,
    );
    sector[jne1_off_pos] = rel8(
        0x7C00u16 + u16::try_from(jne1_off_pos + 1).unwrap(),
        fail_addr,
    );
    sector[jne2_off_pos] = rel8(
        0x7C00u16 + u16::try_from(jne2_off_pos + 1).unwrap(),
        fail_addr,
    );

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xAA;

    sector
}

#[test]
fn boots_and_reads_lba_via_int13_extensions_from_virtual_disk() {
    // Build a minimal disk with:
    // - LBA0: boot sector that reads LBA1 via INT 13h extensions (AH=42h)
    // - LBA1: marker bytes to validate the read succeeded
    let boot = build_int13_ext_read_boot_sector(b'S', b'F');
    let mut marker = [0u8; aero_storage::SECTOR_SIZE];
    marker[0..4].copy_from_slice(b"MARK");

    let mut disk = RawDisk::create(MemBackend::new(), (2 * SECTOR_SIZE) as u64).unwrap();
    disk.write_sectors(0, &boot).unwrap();
    disk.write_sectors(1, &marker).unwrap();

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();
    m.shared_disk().set_backend(Box::new(disk));
    m.reset();

    run_until_halt(&mut m);
    assert_eq!(m.take_serial_output(), vec![b'S']);
}
