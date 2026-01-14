use aero_machine::{Machine, MachineConfig, PcMachine, RunExit};
use aero_platform::reset::ResetKind;

fn boot_sector_requests_reset_then_writes_marker() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut code: Vec<u8> = Vec::new();

    // Deterministic real-mode environment.
    // cli
    code.push(0xFA);
    // xor ax, ax
    code.extend_from_slice(&[0x31, 0xC0]);
    // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xD8]);
    // mov es, ax
    code.extend_from_slice(&[0x8E, 0xC0]);
    // mov ss, ax
    code.extend_from_slice(&[0x8E, 0xD0]);
    // mov sp, 0x7C00
    code.extend_from_slice(&[0xBC, 0x00, 0x7C]);

    // Write the common reset value 0x06 to port 0xCF9 (Reset Control Register).
    // mov dx, 0x0CF9
    code.extend_from_slice(&[0xBA, 0xF9, 0x0C]);
    // mov al, 0x06
    code.extend_from_slice(&[0xB0, 0x06]);
    // out dx, al
    code.push(0xEE);

    // If the machine does not observe the reset request immediately, this instruction will run.
    // mov byte ptr [0x2000], 0xAA
    code.extend_from_slice(&[0xC6, 0x06, 0x00, 0x20, 0xAA]);

    // hlt (in case reset is not observed, stop execution deterministically)
    code.push(0xF4);

    assert!(code.len() <= 510, "boot sector too large: {}", code.len());

    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    sector[..code.len()].copy_from_slice(&code);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn machine_reset_request_is_observed_before_next_instruction() {
    const MARKER_ADDR: u64 = 0x2000;

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: true,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot_sector_requests_reset_then_writes_marker().to_vec())
        .unwrap();
    m.reset();
    m.write_physical_u8(MARKER_ADDR, 0);

    // A single slice should report the reset request, without requiring the host to call
    // `run_slice` again.
    let exit = m.run_slice(10_000);
    match exit {
        RunExit::ResetRequested { kind, .. } => assert_eq!(kind, ResetKind::System),
        other => panic!("unexpected run exit: {other:?}"),
    }

    // The instruction after the OUT 0xCF9 must not have executed.
    assert_eq!(m.read_physical_u8(MARKER_ADDR), 0);
}

#[test]
fn pc_machine_reset_request_is_observed_before_next_instruction() {
    const MARKER_ADDR: u64 = 0x2000;

    let mut pc = PcMachine::new(2 * 1024 * 1024);
    pc.set_disk_image(boot_sector_requests_reset_then_writes_marker().to_vec())
        .unwrap();
    pc.reset();
    pc.platform_mut().memory.write_u8(MARKER_ADDR, 0);

    let exit = pc.run_slice(10_000);
    match exit {
        RunExit::ResetRequested { kind, .. } => assert_eq!(kind, ResetKind::System),
        other => panic!("unexpected run exit: {other:?}"),
    }

    assert_eq!(pc.platform_mut().memory.read_u8(MARKER_ADDR), 0);
}
