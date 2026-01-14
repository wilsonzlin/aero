//! SMP regression test: platform idle tick must **not** advance time when all vCPUs are halted
//! with interrupts disabled (IF=0).
//!
//! `Machine::run_slice` advances deterministic platform time while the BSP is halted via
//! `idle_tick_platform_1ms()`. That tick is intentionally gated: if no vCPU is runnable and no vCPU
//! can observe maskable interrupts, time should not advance (otherwise a terminal `cli; hlt` state
//! would burn host time).
//!
//! This test builds a 2-vCPU PC platform machine where:
//! - BSP executes `cli; hlt`
//! - AP remains in its initial wait-for-SIPI state (halted, IF=0)
//!
//! Repeated `run_slice` calls must not advance the ACPI PM timer (`DEFAULT_PM_TMR_BLK`).

use aero_cpu_core::state::RFLAGS_IF;
use aero_devices::acpi_pm::DEFAULT_PM_TMR_BLK;
use aero_machine::{Machine, MachineConfig, RunExit};

fn build_bsp_cli_hlt_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    // Real-mode boot sector loaded at 0x7C00 by the BIOS.
    //
    // Program:
    //   cli
    //   xor ax, ax
    //   mov ds, ax
    //   mov ss, ax
    //   mov sp, 0x8000
    //   hlt
    //   jmp $-3
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    sector[i] = 0xFA; // cli
    i += 1;

    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]); // xor ax, ax
    i += 2;
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    i += 2;
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD0]); // mov ss, ax
    i += 2;
    sector[i..i + 3].copy_from_slice(&[0xBC, 0x00, 0x80]); // mov sp, 0x8000
    i += 3;

    // hlt; jmp short $-3
    sector[i..i + 3].copy_from_slice(&[0xF4, 0xEB, 0xFD]);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_bsp_halted(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit while waiting for BSP HLT: {other:?}"),
        }
    }
    panic!("BSP did not reach HLT in time");
}

#[test]
fn smp_idle_tick_all_hlt_if0_does_not_advance_pm_timer() {
    let cfg = MachineConfig {
        cpu_count: 2,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_a20_gate: true,
        ..Default::default()
    };

    let boot = build_bsp_cli_hlt_boot_sector();

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_bsp_halted(&mut m);

    let bsp = m.vcpu_state(0).expect("BSP vCPU must exist");
    assert!(bsp.halted, "expected BSP to be halted");
    assert_eq!(
        bsp.rflags() & RFLAGS_IF,
        0,
        "expected BSP to keep interrupts disabled (rflags=0x{:x})",
        bsp.rflags()
    );

    // AP should be in the initial wait-for-SIPI state.
    let ap = m.vcpu_state(1).expect("AP vCPU must exist");
    assert!(ap.halted, "expected AP to start halted waiting for SIPI");
    assert_eq!(
        ap.rflags() & RFLAGS_IF,
        0,
        "expected AP baseline interrupts disabled (rflags=0x{:x})",
        ap.rflags()
    );

    let baseline = m.io_read(DEFAULT_PM_TMR_BLK, 4);

    // Repeated slices while the BSP is halted with IF=0 should not advance deterministic time.
    for _ in 0..50 {
        assert!(matches!(
            m.run_slice(50_000),
            RunExit::Halted { executed: 0 }
        ));
        assert_eq!(
            m.io_read(DEFAULT_PM_TMR_BLK, 4),
            baseline,
            "PM timer advanced while all vCPUs were halted with IF=0 (baseline=0x{baseline:08x})"
        );
    }
}
