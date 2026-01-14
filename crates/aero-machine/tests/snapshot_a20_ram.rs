use aero_devices::a20_gate::A20_GATE_PORT;
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("did not halt");
}

fn build_a20_snapshot_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    // Real-mode boot sector that:
    // 1) Enables A20 via port 0x92
    // 2) Writes distinct bytes to physical 0x000000 and 0x100000
    // 3) Disables A20 via port 0x92
    // 4) Runs a long delay loop while A20 is disabled
    // 5) Re-enables A20 and checks that the byte at 0x100000 is still distinct
    // 6) Outputs "OK" to COM1 if correct, else "FAIL"
    //
    // This is a regression test for snapshot RAM serialization: it must bypass
    // A20-masked physical reads/writes so underlying RAM contents are preserved.
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov dx, 0x0092
    let a20_port = A20_GATE_PORT.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xBA, a20_port[0], a20_port[1]]);
    i += 3;
    // mov al, 0x02
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x02]);
    i += 2;
    // out dx, al
    sector[i] = 0xEE;
    i += 1;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov byte [0x0000], 0x11
    sector[i..i + 5].copy_from_slice(&[0xC6, 0x06, 0x00, 0x00, 0x11]);
    i += 5;

    // mov ax, 0xFFFF
    sector[i..i + 3].copy_from_slice(&[0xB8, 0xFF, 0xFF]);
    i += 3;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov byte [es:0x0010], 0x22  (physical 0x100000)
    sector[i..i + 6].copy_from_slice(&[0x26, 0xC6, 0x06, 0x10, 0x00, 0x22]);
    i += 6;

    // xor al, al
    sector[i..i + 2].copy_from_slice(&[0x30, 0xC0]);
    i += 2;
    // out dx, al  (disable A20)
    sector[i] = 0xEE;
    i += 1;

    // mov cx, 0x1000
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x00, 0x10]);
    i += 3;
    // delay: dec cx
    sector[i] = 0x49;
    i += 1;
    // jnz delay (rel8 = -3)
    sector[i..i + 2].copy_from_slice(&[0x75, 0xFD]);
    i += 2;

    // mov al, 0x02
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x02]);
    i += 2;
    // out dx, al (re-enable A20)
    sector[i] = 0xEE;
    i += 1;

    // mov al, [es:0x0010]
    sector[i..i + 4].copy_from_slice(&[0x26, 0xA0, 0x10, 0x00]);
    i += 4;
    // cmp al, 0x22
    sector[i..i + 2].copy_from_slice(&[0x3C, 0x22]);
    i += 2;
    // jne fail (rel8 = +10)
    sector[i..i + 2].copy_from_slice(&[0x75, 0x0A]);
    i += 2;

    // ok:
    // mov dx, 0x3F8
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
    i += 3;
    // mov al, 'O'; out dx, al
    sector[i..i + 3].copy_from_slice(&[0xB0, b'O', 0xEE]);
    i += 3;
    // mov al, 'K'; out dx, al
    sector[i..i + 3].copy_from_slice(&[0xB0, b'K', 0xEE]);
    i += 3;
    // hlt
    sector[i] = 0xF4;
    i += 1;

    // fail:
    // mov dx, 0x3F8
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
    i += 3;
    // mov al, 'F'; out dx, al
    sector[i..i + 3].copy_from_slice(&[0xB0, b'F', 0xEE]);
    i += 3;
    // mov al, 'A'; out dx, al
    sector[i..i + 3].copy_from_slice(&[0xB0, b'A', 0xEE]);
    i += 3;
    // mov al, 'I'; out dx, al
    sector[i..i + 3].copy_from_slice(&[0xB0, b'I', 0xEE]);
    i += 3;
    // mov al, 'L'; out dx, al
    sector[i..i + 3].copy_from_slice(&[0xB0, b'L', 0xEE]);
    i += 3;
    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn snapshot_ram_bypasses_a20_masking() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };

    let boot = build_a20_snapshot_boot_sector();

    let mut vm = Machine::new(cfg.clone()).unwrap();
    vm.set_disk_image(boot.to_vec()).unwrap();
    vm.reset();

    // Run into the delay loop while A20 is disabled, then snapshot.
    let mut saw_a20_disabled = false;
    for _ in 0..10_000 {
        match vm.run_slice(100) {
            RunExit::Completed { .. } => {}
            other => panic!("unexpected exit before snapshot: {other:?}"),
        }

        if !vm.cpu().a20_enabled {
            saw_a20_disabled = true;
            // Run a few more instructions into the loop to ensure we're snapshotting mid-execution.
            assert!(matches!(vm.run_slice(10), RunExit::Completed { .. }));
            break;
        }
    }
    assert!(saw_a20_disabled, "guest never disabled A20");
    assert!(
        !vm.cpu().a20_enabled,
        "expected A20 disabled at snapshot time"
    );

    let snap = vm.take_snapshot_full().unwrap();

    let mut resumed = Machine::new(cfg).unwrap();
    resumed.set_disk_image(boot.to_vec()).unwrap();
    resumed.reset();
    resumed.restore_snapshot_bytes(&snap).unwrap();

    run_until_halt(&mut resumed);
    assert_eq!(resumed.take_serial_output(), b"OK");
}
