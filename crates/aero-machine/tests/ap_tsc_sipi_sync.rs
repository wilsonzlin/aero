use aero_machine::{Machine, MachineConfig};

fn boot_sector_spin_forever() -> [u8; 512] {
    // Minimal MBR/boot sector: `cli; jmp $`.
    //
    // The BIOS loads the boot sector to 0x7C00 and transfers control to it. Running slices against
    // this loop deterministically advances the BSP TSC.
    let mut sector = [0u8; 512];
    sector[0] = 0xFA; // cli
    sector[1] = 0xEB; // jmp short -2
    sector[2] = 0xFE;
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn ap_rdtsc_is_nonzero_and_in_sync_after_sipi() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        cpu_count: 2,
        enable_pc_platform: true,
        ..Default::default()
    })
    .unwrap();
    m.set_disk_image(boot_sector_spin_forever().to_vec())
        .unwrap();
    m.reset();

    // Advance BSP time so the global TSC is non-zero.
    for _ in 0..10 {
        let _ = m.run_slice(1000);
    }
    let tsc_before = m.cpu().msr.tsc;
    assert!(tsc_before > 0, "sanity: expected BSP TSC to be non-zero");

    // Install AP code at physical 0x8000 (SIPI vector 0x08):
    //   rdtsc
    //   mov dword ptr [0x0500], eax
    //   hlt
    //
    // Store only low32 to keep the real-mode memory write simple.
    const AP_CODE_ADDR: u64 = 0x8000;
    const AP_TSC_STORE_ADDR: u64 = 0x0500;
    let ap_code: [u8; 7] = [0x0F, 0x31, 0x66, 0xA3, 0x00, 0x05, 0xF4];
    m.write_physical(AP_CODE_ADDR, &ap_code);
    m.write_physical_u32(AP_TSC_STORE_ADDR, 0);

    // Send INIT + SIPI to APIC ID 1 (vCPU1) via host-facing LAPIC MMIO APIs.
    const ICR_LOW_OFF: u64 = 0x300;
    const ICR_HIGH_OFF: u64 = 0x310;

    let icr_high = 1u32 << 24;
    // INIT (0b101) + level=assert.
    let icr_init_low = (0b101u32 << 8) | (1u32 << 14);
    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_init_low);

    // SIPI (0b110) with vector 0x08.
    //
    // STARTUP IPIs are edge-triggered; the ICR "Level" bit is treated as don't-care. Keep it
    // clear here to ensure we don't accidentally regress into level-gated delivery.
    let icr_sipi_low = (0b110u32 << 8) | 0x08u32;
    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_sipi_low);
    assert!(
        !m.cpu_by_index(1).halted,
        "expected AP to become runnable after SIPI"
    );

    // Run slices until the AP has executed and stored its TSC.
    let mut ap_tsc_low32 = 0u32;
    for _ in 0..100 {
        let _ = m.run_slice(1000);
        ap_tsc_low32 = m.read_physical_u32(AP_TSC_STORE_ADDR);
        if ap_tsc_low32 != 0 {
            break;
        }
    }
    assert_ne!(
        ap_tsc_low32, 0,
        "expected AP to execute and store a non-zero TSC"
    );

    let tsc_after = m.cpu().msr.tsc;
    let before_low32 = tsc_before as u32;
    let after_low32 = tsc_after as u32;
    assert!(
        ap_tsc_low32 >= before_low32 && ap_tsc_low32 <= after_low32,
        "expected AP TSC low32 (0x{ap_tsc_low32:08x}) to be within [0x{before_low32:08x}, 0x{after_low32:08x}]",
    );
}
