#![allow(dead_code)]

use aero_machine::{Machine, RunExit};

pub fn build_real_mode_interrupt_wait_boot_sector(
    vector: u8,
    flag_addr: u16,
    flag_value: u8,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov ss, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD0]);
    i += 2;
    // mov sp, 0x7c00
    sector[i..i + 3].copy_from_slice(&[0xBC, 0x00, 0x7C]);
    i += 3;

    let ivt_off = (vector as u16) * 4;

    // mov word ptr [ivt_off], handler_offset (patched later)
    // C7 06 <addr16> <imm16>
    let patch_off = i + 4;
    sector[i..i + 2].copy_from_slice(&[0xC7, 0x06]);
    sector[i + 2..i + 4].copy_from_slice(&ivt_off.to_le_bytes());
    // imm16 placeholder
    sector[i + 4..i + 6].copy_from_slice(&[0, 0]);
    i += 6;

    // mov word ptr [ivt_off+2], 0x0000 (segment)
    sector[i..i + 2].copy_from_slice(&[0xC7, 0x06]);
    sector[i + 2..i + 4].copy_from_slice(&(ivt_off + 2).to_le_bytes());
    sector[i + 4..i + 6].copy_from_slice(&[0, 0]);
    i += 6;

    // sti
    sector[i] = 0xFB;
    i += 1;

    // hlt; jmp short $-3  (busy wait at HLT)
    sector[i..i + 3].copy_from_slice(&[0xF4, 0xEB, 0xFD]);
    i += 3;

    // Handler lives directly after the loop, still within the boot sector (loaded at 0x7C00).
    let handler_addr = 0x7C00u16 + (i as u16);
    sector[patch_off..patch_off + 2].copy_from_slice(&handler_addr.to_le_bytes());

    // mov byte ptr [flag_addr], flag_value
    sector[i..i + 2].copy_from_slice(&[0xC6, 0x06]);
    i += 2;
    sector[i..i + 2].copy_from_slice(&flag_addr.to_le_bytes());
    i += 2;
    sector[i] = flag_value;
    i += 1;
    // iret
    sector[i] = 0xCF;

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

pub fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit while waiting for HLT: {other:?}"),
        }
    }
    panic!("machine did not reach HLT in time");
}

pub fn ioapic_default_polarity_low(gsi: u32) -> bool {
    // Keep this logic in sync with `aero_interrupts::apic::IoApic` default PC wiring assumptions:
    // - SCI (GSI9) is active-low
    // - PCI INTx GSIs (10-13) are active-low
    // - GSIs >= 16 are also treated as active-low by default.
    gsi == 9 || (10..=13).contains(&gsi) || gsi >= 16
}

pub fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    low: u32,
    high: u32,
) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}
