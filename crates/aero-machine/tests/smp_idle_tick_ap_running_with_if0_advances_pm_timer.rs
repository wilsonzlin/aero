//! SMP regression test: platform idle tick must advance time when BSP is `cli; hlt` and an AP is
//! running with interrupts disabled (IF=0).
//!
//! `Machine::run_slice` advances deterministic platform time while the BSP is halted via
//! `idle_tick_platform_1ms()`. That idle tick is intentionally gated so uniprocessor `cli; hlt`
//! does not burn host time, but SMP bring-up flows rely on time advancing when *any* vCPU could
//! make progress.
//!
//! This test starts a 2-vCPU PC platform machine:
//! - BSP executes `cli; hlt` (cannot accept maskable interrupts)
//! - AP executes a busy-loop that polls the ACPI PM timer at `DEFAULT_PM_TMR_BLK` with IF=0
//!
//! Platform time must advance even though no vCPU has IF=1, otherwise the PM timer never changes
//! and the AP loop never completes.

use aero_cpu_core::state::RFLAGS_IF;
use aero_devices::acpi_pm::DEFAULT_PM_TMR_BLK;
use aero_machine::{Machine, MachineConfig, RunExit};

const SIPI_VECTOR: u8 = 0x08; // 0x8000 (aligned to 4KiB and below 1MiB).
const AP_TRAMPOLINE_PADDR: u64 = (SIPI_VECTOR as u64) << 12;
const APIC_ID_AP: u8 = 1;

const BASELINE_PADDR: u64 = 0x0500;
const FLAG_PADDR: u64 = 0x0502;
const FLAG_VALUE: u8 = 0xA5;

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

fn build_ap_poll_pm_timer(flag_addr: u16, flag_value: u8, baseline_addr: u16) -> Vec<u8> {
    let mut code: Vec<u8> = Vec::new();

    // Disable interrupts and establish DS=SS=0.
    code.push(0xFA); // cli
    code.extend_from_slice(&[0x31, 0xC0]); // xor ax, ax
    code.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xD0]); // mov ss, ax
    code.extend_from_slice(&[0xBC, 0x00, 0x90]); // mov sp, 0x9000

    // mov dx, DEFAULT_PM_TMR_BLK
    code.push(0xBA);
    code.extend_from_slice(&DEFAULT_PM_TMR_BLK.to_le_bytes());

    // Read baseline timer value and store it at `baseline_addr`.
    code.push(0xED); // in ax, dx
    code.push(0xA3); // mov [moffs16], ax
    code.extend_from_slice(&baseline_addr.to_le_bytes());

    // Busy-loop until the timer changes (it only changes when platform time advances).
    let loop_start = code.len();
    code.push(0xED); // in ax, dx
    code.extend_from_slice(&[0x3B, 0x06]); // cmp ax, [disp16]
    code.extend_from_slice(&baseline_addr.to_le_bytes());
    code.push(0x74); // je rel8 (patched)
    let rel8_off = code.len();
    code.push(0);

    // mov byte ptr [flag_addr], flag_value
    code.extend_from_slice(&[0xC6, 0x06]);
    code.extend_from_slice(&flag_addr.to_le_bytes());
    code.push(flag_value);

    // Park the AP after completing (stay halted even if it spuriously wakes).
    code.extend_from_slice(&[0xF4, 0xEB, 0xFD]); // hlt; jmp short $-3

    let next_ip = (rel8_off + 1) as isize;
    let rel = (loop_start as isize) - next_ip;
    code[rel8_off] = i8::try_from(rel).expect("pm-timer poll loop too large for short jump") as u8;

    code
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

fn send_init_sipi(m: &mut Machine, dest_apic_id: u8, sipi_vector: u8) {
    // Local APIC ICR offsets (xAPIC MMIO).
    const ICR_LOW: u64 = 0x300;
    const ICR_HIGH: u64 = 0x310;

    // Program destination in ICR_HIGH (bits 56..63 -> bits 24..31 of the high dword).
    m.write_lapic_u32(0, ICR_HIGH, u32::from(dest_apic_id) << 24);

    // INIT IPI (delivery mode 0b101), level=assert.
    let icr_init = (0b101u32 << 8) | (1 << 14);
    m.write_lapic_u32(0, ICR_LOW, icr_init);

    // SIPI (startup IPI) (delivery mode 0b110), vector in bits 0..7, level=assert.
    let icr_sipi = u32::from(sipi_vector) | (0b110u32 << 8) | (1 << 14);
    m.write_lapic_u32(0, ICR_LOW, icr_sipi);
}

#[test]
fn smp_idle_tick_ap_running_with_if0_advances_pm_timer() {
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

    // Clear shared low memory used by the AP program.
    m.write_physical_u16(BASELINE_PADDR, 0);
    m.write_physical_u8(FLAG_PADDR, 0);

    // Place the AP trampoline at the SIPI vector address.
    let ap_code = build_ap_poll_pm_timer(FLAG_PADDR as u16, FLAG_VALUE, BASELINE_PADDR as u16);
    assert!(
        ap_code.len() <= 4096,
        "AP trampoline too large for SIPI page: {} bytes",
        ap_code.len()
    );
    m.write_physical(AP_TRAMPOLINE_PADDR, &ap_code);

    // Park the BSP in `cli; hlt`.
    run_until_bsp_halted(&mut m);
    let bsp = m.vcpu_state(0).expect("BSP vCPU must exist");
    assert!(bsp.halted, "expected BSP to be halted");
    assert_eq!(
        bsp.rflags() & RFLAGS_IF,
        0,
        "expected BSP to keep interrupts disabled (rflags=0x{:x})",
        bsp.rflags()
    );

    // Start the AP via INIT+SIPI.
    send_init_sipi(&mut m, APIC_ID_AP, SIPI_VECTOR);

    // Wait for the AP to become runnable and start polling.
    let mut ap_started = false;
    for _ in 0..200 {
        assert!(matches!(
            m.run_slice(50_000),
            RunExit::Halted { executed: 0 }
        ));
        let ap = m.vcpu_state(1).expect("AP vCPU must exist");
        if !ap.halted {
            assert_eq!(
                ap.rflags() & RFLAGS_IF,
                0,
                "expected AP to keep interrupts disabled (rflags=0x{:x})",
                ap.rflags()
            );
            ap_started = true;
            break;
        }
    }
    assert!(ap_started, "AP did not start running in time");

    // The AP should observe the PM timer changing once platform time advances via the BSP idle
    // tick loop and then set the completion flag.
    for _ in 0..200 {
        assert!(matches!(
            m.run_slice(50_000),
            RunExit::Halted { executed: 0 }
        ));
        if m.read_physical_u8(FLAG_PADDR) == FLAG_VALUE {
            return;
        }
    }

    let ap = m.vcpu_state(1).expect("AP vCPU must exist");
    let pm_now = m.io_read(DEFAULT_PM_TMR_BLK, 4);
    panic!(
        "AP did not observe PM timer advancing (flag=0x{:02x}, baseline=0x{:04x}, ap_halted={}, ap_rflags=0x{:x}, pm_tmr_now=0x{pm_now:08x})",
        m.read_physical_u8(FLAG_PADDR),
        m.read_physical_u16(BASELINE_PADDR),
        ap.halted,
        ap.rflags(),
    );
}
