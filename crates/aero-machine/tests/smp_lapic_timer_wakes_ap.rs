use aero_cpu_core::state::RFLAGS_IF;
use aero_devices::a20_gate::A20_GATE_PORT;
use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::PlatformInterruptMode;

const AP_CPU_INDEX: usize = 1;

// SIPI vector encodes a 4KiB page number; the AP begins at `vector << 12`.
const SIPI_VECTOR: u8 = 0x08;
const AP_START_PADDR: u64 = (SIPI_VECTOR as u64) << 12;
const AP_CS_SELECTOR: u16 = (SIPI_VECTOR as u16) << 8;

// Chosen local APIC timer vector. Avoid exception vectors (0..31).
const TIMER_VECTOR: u8 = 0x60;

const SENTINEL_PADDR: u64 = 0x0500;
const SENTINEL_VALUE: u8 = 0xA5;

const ICR_LOW_OFF: u64 = 0x300;
const ICR_HIGH_OFF: u64 = 0x310;

fn build_bsp_hlt_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // Minimal real-mode setup (DS=SS=0, SP=0x7C00).
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

    // Keep IF=1 so the machine's idle tick loop can advance time while the BSP is halted.
    // sti
    sector[i] = 0xFB;
    i += 1;
    // hlt; jmp short $-3 (re-halt after wake)
    sector[i..i + 3].copy_from_slice(&[0xF4, 0xEB, 0xFD]);
    i += 3;

    assert!(i <= 510);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn build_ap_sipi_payload(
    vector: u8,
    cs_selector: u16,
    sentinel_addr: u16,
    sentinel_value: u8,
) -> (Vec<u8>, u16) {
    let mut code: Vec<u8> = Vec::new();

    // Disable interrupts and set up a known real-mode environment.
    // cli
    code.push(0xFA);
    // xor ax, ax
    code.extend_from_slice(&[0x31, 0xC0]);
    // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xD8]);
    // mov ss, ax
    code.extend_from_slice(&[0x8E, 0xD0]);
    // mov sp, 0x7000
    code.extend_from_slice(&[0xBC, 0x00, 0x70]);

    let ivt_off = u16::from(vector) * 4;

    // mov word ptr [ivt_off], handler_offset (patched later)
    // C7 06 <addr16> <imm16>
    let patch_off = code.len() + 4;
    code.extend_from_slice(&[0xC7, 0x06]);
    code.extend_from_slice(&ivt_off.to_le_bytes());
    code.extend_from_slice(&[0, 0]); // imm16 placeholder

    // mov word ptr [ivt_off+2], cs_selector
    code.extend_from_slice(&[0xC7, 0x06]);
    code.extend_from_slice(&(ivt_off + 2).to_le_bytes());
    code.extend_from_slice(&cs_selector.to_le_bytes());

    // sti; hlt
    code.extend_from_slice(&[0xFB, 0xF4]);
    // Stop after wake: cli; hlt
    code.extend_from_slice(&[0xFA, 0xF4]);

    let handler_off = code.len() as u16;
    code[patch_off..patch_off + 2].copy_from_slice(&handler_off.to_le_bytes());

    // Handler: write sentinel then return.
    // mov byte ptr [sentinel_addr], sentinel_value
    code.extend_from_slice(&[0xC6, 0x06]);
    code.extend_from_slice(&sentinel_addr.to_le_bytes());
    code.push(sentinel_value);
    // iret
    code.push(0xCF);

    (code, handler_off)
}

fn run_slice_allow_hlt(m: &mut Machine, max_insts: u64) {
    match m.run_slice(max_insts) {
        RunExit::Completed { .. } | RunExit::Halted { .. } => {}
        other => panic!("unexpected machine exit: {other:?}"),
    }
}

#[test]
fn smp_lapic_timer_wakes_ap() {
    let cfg = MachineConfig {
        cpu_count: 2,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_a20_gate: true,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    // Keep the BSP in a stable `sti; hlt` loop so machine time advances deterministically.
    let bsp_boot = build_bsp_hlt_boot_sector();
    m.set_disk_image(bsp_boot.to_vec()).unwrap();
    m.reset();

    // Enable A20 so platform MMIO ranges are not aliased by the legacy A20 gate behaviour.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    // Switch the platform interrupt router into APIC mode so LAPIC1 timer delivery is observable.
    {
        let interrupts = m
            .platform_interrupts()
            .expect("PC platform interrupts missing");
        interrupts
            .borrow_mut()
            .set_mode(PlatformInterruptMode::Apic);
    }

    // Place the AP SIPI payload at the SIPI entry page and clear the sentinel byte.
    m.write_physical_u8(SENTINEL_PADDR, 0);
    let (ap_code, handler_off) = build_ap_sipi_payload(
        TIMER_VECTOR,
        AP_CS_SELECTOR,
        SENTINEL_PADDR as u16,
        SENTINEL_VALUE,
    );
    assert!(
        ap_code.len() <= 4096,
        "AP payload too large for SIPI page: {} bytes",
        ap_code.len()
    );
    m.write_physical(AP_START_PADDR, &ap_code);

    // Start the AP via INIT + SIPI (xAPIC physical destination mode).
    // INIT (delivery mode 0b101), Level=Assert.
    let icr_high = 1u32 << 24;
    let icr_low_init = (0b101u32 << 8) | (1u32 << 14);
    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_low_init);

    // SIPI (delivery mode 0b110), Level=Assert, vector=SIPI_VECTOR.
    let icr_low_sipi = (u32::from(SIPI_VECTOR)) | (0b110u32 << 8) | (1u32 << 14);
    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_low_sipi);

    // Run until the AP reaches `sti; hlt` and installs its IVT handler.
    let ivt_off = u64::from(TIMER_VECTOR) * 4;
    for _ in 0..5_000 {
        run_slice_allow_hlt(&mut m, 50_000);

        let ap = m
            .vcpu_state(AP_CPU_INDEX)
            .expect("AP vCPU state unavailable");
        if ap.halted && (ap.rflags() & RFLAGS_IF) != 0 {
            // Sanity: IVT entry should now point at the handler in the SIPI payload.
            assert_eq!(
                m.read_physical_u16(ivt_off),
                handler_off,
                "AP did not install IVT offset for timer vector"
            );
            assert_eq!(
                m.read_physical_u16(ivt_off + 2),
                AP_CS_SELECTOR,
                "AP did not install IVT segment for timer vector"
            );
            break;
        }
    }

    let ap = m
        .vcpu_state(AP_CPU_INDEX)
        .expect("AP vCPU state unavailable");
    assert!(
        ap.halted && (ap.rflags() & RFLAGS_IF) != 0,
        "AP did not reach `sti; hlt` (halted={}, rflags=0x{:x})",
        ap.halted,
        ap.rflags()
    );

    // Program LAPIC1 timer to deliver TIMER_VECTOR.
    //
    // Register offsets (xAPIC):
    // - SVR: 0xF0 (bit8 enable)
    // - LVT Timer: 0x320
    // - Divide Config: 0x3E0
    // - Initial Count: 0x380
    const SVR_OFF: u64 = 0xF0;
    const LVT_TIMER_OFF: u64 = 0x320;
    const DIVIDE_CONFIG_OFF: u64 = 0x3E0;
    const INITIAL_COUNT_OFF: u64 = 0x380;

    // Enable the local APIC (SVR[8]) and set spurious vector to 0xFF.
    m.write_lapic_u32(AP_CPU_INDEX, SVR_OFF, 0x1FF);
    // Unmask timer and set its vector.
    m.write_lapic_u32(AP_CPU_INDEX, LVT_TIMER_OFF, u32::from(TIMER_VECTOR));
    // Divide by 1.
    m.write_lapic_u32(AP_CPU_INDEX, DIVIDE_CONFIG_OFF, 0xB);
    // Fire after ~1ms (1_000_000 ns) to line up with the machine idle tick granularity.
    m.write_lapic_u32(AP_CPU_INDEX, INITIAL_COUNT_OFF, 1_000_000);

    // Advance until the AP-local timer fires and the AP ISR writes the sentinel.
    for _ in 0..2_000 {
        run_slice_allow_hlt(&mut m, 50_000);

        if m.read_physical_u8(SENTINEL_PADDR) == SENTINEL_VALUE {
            return;
        }
    }

    panic!(
        "AP LAPIC timer ISR did not run (sentinel=0x{:02x})",
        m.read_physical_u8(SENTINEL_PADDR)
    );
}
