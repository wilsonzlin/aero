use aero_devices::clock::Clock as _;
use aero_machine::{Machine, MachineConfig, PcMachine, RunExit, DEFAULT_GUEST_CPU_HZ};

fn busy_loop_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    // cli
    sector[0] = 0xFA;
    // jmp short $-2 (infinite loop)
    sector[1] = 0xEB;
    sector[2] = 0xFE;
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn machine_platform_tick_is_not_stuck_at_zero_for_small_slices() {
    // For DEFAULT_GUEST_CPU_HZ=3GHz, 1 instruction/cycle is <1ns. We should still advance platform
    // time deterministically once enough fractional ns has accumulated.
    let cycles_per_ns = (DEFAULT_GUEST_CPU_HZ as u128).div_ceil(1_000_000_000u128);

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(busy_loop_boot_sector().to_vec()).unwrap();
    m.reset();

    let clock = m.platform_clock().expect("pc platform should be enabled");
    assert_eq!(clock.now_ns(), 0);

    for _ in 0..(cycles_per_ns as u64) {
        match m.run_slice(1) {
            RunExit::Completed { executed } => assert_eq!(executed, 1),
            other => panic!("unexpected run exit: {other:?}"),
        }
    }

    assert_eq!(clock.now_ns(), 1);
}

#[test]
fn pc_machine_platform_tick_is_not_stuck_at_zero_for_small_slices() {
    let cycles_per_ns = (DEFAULT_GUEST_CPU_HZ as u128).div_ceil(1_000_000_000u128);

    let mut pc = PcMachine::new(2 * 1024 * 1024);
    pc.set_disk_image(busy_loop_boot_sector().to_vec()).unwrap();
    pc.reset();

    let clock = pc.platform().clock();
    assert_eq!(clock.now_ns(), 0);

    for _ in 0..(cycles_per_ns as u64) {
        match pc.run_slice(1) {
            RunExit::Completed { executed } => assert_eq!(executed, 1),
            other => panic!("unexpected run exit: {other:?}"),
        }
    }

    assert_eq!(clock.now_ns(), 1);
}
