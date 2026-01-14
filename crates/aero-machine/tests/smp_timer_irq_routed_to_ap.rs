//! SMP regression test: IOAPIC destination routing for timer interrupts.
//!
//! This test boots a 2-vCPU PC platform machine, starts the AP via INIT+SIPI, parks it in
//! `sti; hlt`, and then routes the PIT timer interrupt (GSI2) to APIC ID 1 via the IOAPIC
//! redirection table.
//!
//! The AP should receive and execute the real-mode interrupt handler; the BSP is kept in `cli; hlt`
//! so it cannot accidentally service the interrupt if destination routing is broken.

use aero_devices::pit8254::{PIT_CH0, PIT_CMD};
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::{PlatformInterruptMode, PlatformInterrupts};

const VECTOR: u8 = 0x60;
const FLAG_ADDR: u16 = 0x0500;
const FLAG_VALUE: u8 = 0xA5;

const PIT_GSI: u32 = 2; // ISA IRQ0 override (MADT ISO) routes PIT to IOAPIC GSI2.

const APIC_ID_AP: u8 = 1;
const SIPI_VECTOR: u8 = 0x08; // 0x8000 (aligned to 4KiB and below 1MiB).
const AP_TRAMPOLINE_PADDR: u64 = (SIPI_VECTOR as u64) << 12;

fn build_bsp_hlt_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    // Real-mode boot sector loaded at 0x7C00 by the BIOS.
    //
    // Program: `cli; xor ax, ax; mov ds, ax; mov ss, ax; mov sp, 0x8000; hlt; jmp $-3`
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

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn install_real_mode_flag_handler(m: &mut Machine, handler_addr: u64, flag_addr: u16, value: u8) {
    let flag_addr_bytes = flag_addr.to_le_bytes();
    // mov byte ptr [imm16], imm8; iret
    let code = [
        0xC6,
        0x06,
        flag_addr_bytes[0],
        flag_addr_bytes[1],
        value,
        0xCF,
    ];
    m.write_physical(handler_addr, &code);
}

fn write_ivt_entry(m: &mut Machine, vector: u8, segment: u16, offset: u16) {
    let base = u64::from(vector) * 4;
    m.write_physical_u16(base, offset);
    m.write_physical_u16(base + 2, segment);
}

fn build_ap_trampoline(stack_ptr: u16) -> Vec<u8> {
    // Real-mode AP entry point (SIPI vector):
    //   cli
    //   xor ax, ax
    //   mov ds, ax
    //   mov ss, ax
    //   mov sp, stack_ptr
    //   sti
    //   hlt
    //   jmp short $-3
    let mut code = Vec::new();
    code.push(0xFA); // cli
    code.extend_from_slice(&[0x31, 0xC0]); // xor ax, ax
    code.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xD0]); // mov ss, ax
    code.push(0xBC); // mov sp, imm16
    code.extend_from_slice(&stack_ptr.to_le_bytes());
    code.push(0xFB); // sti
    code.extend_from_slice(&[0xF4, 0xEB, 0xFD]); // hlt; jmp short $-3
    code
}

fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

fn run_until_bsp_halted(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit while waiting for BSP HLT: {other:?}"),
        }
    }
    panic!("BSP did not reach HLT in time");
}

fn run_until_ap_halted(m: &mut Machine, cpu_index: usize) {
    for _ in 0..200 {
        let _ = m.run_slice(10_000);
        if m.vcpu_state(cpu_index)
            .expect("AP vCPU should exist")
            .halted
        {
            return;
        }
    }
    panic!("AP did not reach HLT in time (SIPI may not have been delivered or vCPU may not be scheduled)");
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
fn smp_timer_irq_routed_to_ap() {
    let cfg = MachineConfig {
        cpu_count: 2,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_a20_gate: true,
        ..Default::default()
    };

    let boot = build_bsp_hlt_boot_sector();

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Install a minimal real-mode ISR at a stable address and point IVT[0x60] at it.
    let handler_addr = 0x1000u64;
    install_real_mode_flag_handler(&mut m, handler_addr, FLAG_ADDR, FLAG_VALUE);
    write_ivt_entry(&mut m, VECTOR, 0x0000, handler_addr as u16);
    m.write_physical_u8(u64::from(FLAG_ADDR), 0);

    // Place an AP trampoline at the SIPI vector address.
    let ap_code = build_ap_trampoline(0x9000);
    m.write_physical(AP_TRAMPOLINE_PADDR, &ap_code);

    // Park the BSP in HLT with interrupts disabled so it cannot service the timer IRQ if IOAPIC
    // destination routing is broken.
    run_until_bsp_halted(&mut m);

    // Start the AP via INIT+SIPI.
    send_init_sipi(&mut m, APIC_ID_AP, SIPI_VECTOR);

    // Ensure the AP executes its `sti; hlt` loop.
    run_until_ap_halted(&mut m, 1);

    // Switch to APIC mode and route the PIT timer interrupt to APIC ID 1.
    {
        let interrupts = m.platform_interrupts().expect("pc platform enabled");
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_entry(
            &mut ints,
            PIT_GSI,
            u32::from(VECTOR),
            u32::from(APIC_ID_AP) << 24,
        );
    }

    // Program PIT channel 0 for ~1kHz periodic interrupts (mode 2, lo/hi).
    // PIT tick rate is 1.193182 MHz, so divisor 1193 ~= 1ms period.
    m.io_write(PIT_CMD, 1, 0x34);
    m.io_write(PIT_CH0, 1, 1193 & 0xFF);
    m.io_write(PIT_CH0, 1, 1193 >> 8);

    // Run until the AP handler flips the flag.
    for _ in 0..200 {
        match m.run_slice(10_000) {
            RunExit::Completed { .. } | RunExit::Halted { .. } => {}
            other => panic!("unexpected exit while waiting for AP interrupt: {other:?}"),
        }
        if m.read_physical_u8(u64::from(FLAG_ADDR)) == FLAG_VALUE {
            // Optional sanity: the BSP should remain halted (it runs `cli; hlt`).
            assert!(
                m.vcpu_state(0).expect("BSP vCPU must exist").halted,
                "expected BSP to remain halted"
            );
            return;
        }
    }

    panic!(
        "timer IRQ did not reach AP (flag=0x{:02x}, expected=0x{:02x})",
        m.read_physical_u8(u64::from(FLAG_ADDR)),
        FLAG_VALUE
    );
}
