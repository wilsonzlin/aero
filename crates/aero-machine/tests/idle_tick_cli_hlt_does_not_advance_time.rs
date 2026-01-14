//! Regression test: `cli; hlt` must not advance deterministic time in a uniprocessor machine.
//!
//! `Machine::run_slice` advances deterministic platform time while halted via
//! `idle_tick_platform_1ms()`. That idle tick is intentionally gated so a terminal `cli; hlt`
//! state (no vCPUs runnable, no vCPU able to accept maskable interrupts) does not burn host time.
//!
//! This test boots a minimal uniprocessor machine into a `cli; hlt` loop and asserts that repeated
//! `run_slice` calls do not advance the BSP TSC or the BIOS BDA tick counter.

use aero_cpu_core::state::RFLAGS_IF;
use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bios::BDA_TICK_COUNT_ADDR;

fn build_cli_hlt_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    // Real-mode boot sector loaded at 0x7C00 by the BIOS.
    //
    // Program:
    //   cli
    //   hlt
    //   jmp $-3
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    sector[0] = 0xFA; // cli
    sector[1] = 0xF4; // hlt
    sector[2] = 0xEB; // jmp short
    sector[3] = 0xFD; // -3 (back to hlt)
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halted(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected run exit while waiting for HLT: {other:?}"),
        }
    }
    panic!("CPU did not reach HLT in time");
}

#[test]
fn idle_tick_cli_hlt_does_not_advance_time() {
    let boot = build_cli_hlt_boot_sector();

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        cpu_count: 1,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halted(&mut m);

    let bsp = m.vcpu_state(0).expect("BSP vCPU must exist");
    assert!(bsp.halted, "expected BSP to be halted");
    assert_eq!(
        bsp.rflags() & RFLAGS_IF,
        0,
        "expected BSP to keep interrupts disabled (rflags=0x{:x})",
        bsp.rflags()
    );

    let start_tsc = bsp.msr.tsc;
    let start_bda_ticks = m.read_physical_u32(BDA_TICK_COUNT_ADDR);

    for _ in 0..500 {
        assert!(matches!(m.run_slice(1), RunExit::Halted { executed: 0 }));
    }

    let end_tsc = m.vcpu_state(0).expect("BSP vCPU must exist").msr.tsc;
    let end_bda_ticks = m.read_physical_u32(BDA_TICK_COUNT_ADDR);

    assert_eq!(
        end_tsc, start_tsc,
        "expected BSP TSC to remain constant while in cli+hlt (start={start_tsc}, end={end_tsc})"
    );
    assert_eq!(
        end_bda_ticks, start_bda_ticks,
        "expected BDA tick count to remain constant while in cli+hlt (start={start_bda_ticks}, end={end_bda_ticks})"
    );
}
