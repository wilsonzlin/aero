use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::PlatformInterruptMode;

const AP1_SIPI_VECTOR: u8 = 0x08; // 0x8000
const AP2_SIPI_VECTOR: u8 = 0x09; // 0x9000

const AP1_PADDR: u64 = (AP1_SIPI_VECTOR as u64) << 12;
const AP2_PADDR: u64 = (AP2_SIPI_VECTOR as u64) << 12;

const FLAG_ADDR: u64 = 0x0500;
const FLAG_VALUE: u8 = 0xA5;

fn boot_sector_spin_forever() -> [u8; aero_storage::SECTOR_SIZE] {
    // Minimal MBR/boot sector: `cli; jmp $`.
    //
    // The BIOS loads the boot sector to 0x7C00 and transfers control to it. Running slices against
    // this loop keeps the BSP deterministic while allowing the machine scheduler to run AP vCPUs.
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    sector[0] = 0xFA; // cli
    sector[1] = 0xEB; // jmp short -2
    sector[2] = 0xFE;
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn build_ap1_protected_mode_init_sipi_sender(dest_apic_id: u8, sipi_vector: u8) -> Vec<u8> {
    // APs start in real mode after SIPI. Real mode cannot reach the LAPIC MMIO window at
    // 0xFEE0_0000, so we switch into 32-bit protected mode with flat segments and then write to
    // the LAPIC ICR to deliver INIT+SIPI to another AP.
    //
    // This is a minimal "just enough" protected-mode transition:
    // - Install a 3-entry GDT (null, code, data) within the SIPI page
    // - Set CR0.PE=1 and far jump into 32-bit code
    // - Write xAPIC ICR_HIGH/LOW to deliver INIT+SIPI
    const PROT_ENTRY_OFF: usize = 0x40;
    const GDT_OFF: usize = 0x100;
    const GDT_SIZE: usize = 8 * 3;
    const GDTR_OFF: usize = GDT_OFF + GDT_SIZE;

    const LAPIC_ICR_LOW: u32 = 0xFEE0_0300;
    const LAPIC_ICR_HIGH: u32 = 0xFEE0_0310;

    let base = AP1_PADDR as u16;
    let prot_entry_phys = base.wrapping_add(PROT_ENTRY_OFF as u16);
    let gdt_phys = (AP1_PADDR as u32).wrapping_add(GDT_OFF as u32);
    let gdtr_phys = base.wrapping_add(GDTR_OFF as u16);

    let mut code = Vec::new();

    // -------------------------------------------------------------------------
    // Real mode: switch to protected mode.
    // -------------------------------------------------------------------------
    code.push(0xFA); // cli
    code.extend_from_slice(&[0x31, 0xC0]); // xor ax, ax
    code.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax (DS=0 so disp16 addresses are absolute)

    // lgdt [gdtr] (0F 01 16 disp16)
    code.extend_from_slice(&[0x0F, 0x01, 0x16]);
    code.extend_from_slice(&gdtr_phys.to_le_bytes());

    // mov eax, cr0; or eax, 1; mov cr0, eax  (use 66 prefixes in real mode)
    code.extend_from_slice(&[0x66, 0x0F, 0x20, 0xC0]); // mov eax, cr0
    code.extend_from_slice(&[0x66, 0x83, 0xC8, 0x01]); // or eax, 1
    code.extend_from_slice(&[0x66, 0x0F, 0x22, 0xC0]); // mov cr0, eax

    // Far jump into protected mode: jmp 0x08:prot_entry (EA off16 sel16)
    code.push(0xEA);
    code.extend_from_slice(&prot_entry_phys.to_le_bytes());
    code.extend_from_slice(&0x0008u16.to_le_bytes());

    // Pad until protected-mode entry point.
    code.resize(PROT_ENTRY_OFF, 0x90);

    // -------------------------------------------------------------------------
    // Protected mode (32-bit): flat segments + write LAPIC ICR.
    // -------------------------------------------------------------------------
    code.extend_from_slice(&[0x66, 0xB8, 0x10, 0x00]); // mov ax, 0x10 (data selector)
    code.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xC0]); // mov es, ax
    code.extend_from_slice(&[0x8E, 0xD0]); // mov ss, ax
    code.extend_from_slice(&[0x8E, 0xE0]); // mov fs, ax
    code.extend_from_slice(&[0x8E, 0xE8]); // mov gs, ax

    // mov esp, 0x7000
    code.push(0xBC);
    code.extend_from_slice(&0x0000_7000u32.to_le_bytes());

    // Destination in ICR_HIGH (bits 56..63 -> bits 24..31 of the high dword).
    let icr_high = u32::from(dest_apic_id) << 24;
    code.push(0xB8); // mov eax, imm32
    code.extend_from_slice(&icr_high.to_le_bytes());
    code.push(0xA3); // mov [imm32], eax
    code.extend_from_slice(&LAPIC_ICR_HIGH.to_le_bytes());

    // INIT IPI (delivery mode 0b101) + Level=Assert.
    let icr_init_low = (0b101u32 << 8) | (1u32 << 14);
    code.push(0xB8);
    code.extend_from_slice(&icr_init_low.to_le_bytes());
    code.push(0xA3);
    code.extend_from_slice(&LAPIC_ICR_LOW.to_le_bytes());

    // SIPI (delivery mode 0b110) + Level=Assert, vector in bits 0..7.
    let icr_sipi_low = u32::from(sipi_vector) | (0b110u32 << 8) | (1u32 << 14);
    code.push(0xB8);
    code.extend_from_slice(&icr_sipi_low.to_le_bytes());
    code.push(0xA3);
    code.extend_from_slice(&LAPIC_ICR_LOW.to_le_bytes());

    // hlt; jmp $
    code.push(0xF4);
    code.extend_from_slice(&[0xEB, 0xFE]);

    assert!(
        code.len() <= GDT_OFF,
        "AP1 code too large for reserved GDT region: code_len=0x{:x} gdt_off=0x{:x}",
        code.len(),
        GDT_OFF
    );
    code.resize(GDTR_OFF + 6, 0);

    // -------------------------------------------------------------------------
    // GDT + GDTR (within the same SIPI page so real mode can address it with disp16).
    // -------------------------------------------------------------------------
    // Null descriptor.
    code[GDT_OFF..GDT_OFF + 8].fill(0);
    // Code descriptor: base=0, limit=4GiB, present, ring0, code, readable, 32-bit, 4KiB gran.
    code[GDT_OFF + 8..GDT_OFF + 16].copy_from_slice(&[
        0xFF, 0xFF, // limit low
        0x00, 0x00, // base low
        0x00, // base mid
        0x9A, // access
        0xCF, // flags + limit high
        0x00, // base high
    ]);
    // Data descriptor: base=0, limit=4GiB, present, ring0, data, writable, 32-bit, 4KiB gran.
    code[GDT_OFF + 16..GDT_OFF + 24].copy_from_slice(&[
        0xFF, 0xFF, // limit low
        0x00, 0x00, // base low
        0x00, // base mid
        0x92, // access
        0xCF, // flags + limit high
        0x00, // base high
    ]);

    // GDTR: limit (u16) + base (u32).
    let limit = (GDT_SIZE as u16).wrapping_sub(1);
    code[GDTR_OFF..GDTR_OFF + 2].copy_from_slice(&limit.to_le_bytes());
    code[GDTR_OFF + 2..GDTR_OFF + 6].copy_from_slice(&gdt_phys.to_le_bytes());

    code
}

fn build_ap2_real_mode_flag_setter(flag_addr: u16, value: u8) -> Vec<u8> {
    // Real-mode AP payload:
    //   cli
    //   xor ax, ax
    //   mov ds, ax
    //   mov byte ptr [flag_addr], value
    //   hlt
    //   jmp $
    let mut code = Vec::new();
    code.push(0xFA); // cli
    code.extend_from_slice(&[0x31, 0xC0]); // xor ax, ax
    code.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    code.extend_from_slice(&[0xC6, 0x06]); // mov byte ptr [imm16], imm8
    code.extend_from_slice(&flag_addr.to_le_bytes());
    code.push(value);
    code.push(0xF4); // hlt
    code.extend_from_slice(&[0xEB, 0xFE]); // jmp short -2
    code
}

#[test]
fn smp_ap_can_start_another_ap_via_init_sipi() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        cpu_count: 3,
        enable_pc_platform: true,
        // Keep the machine minimal; this test only needs LAPIC MMIO + AP scheduling.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot_sector_spin_forever().to_vec())
        .unwrap();
    m.reset();
    m.platform_interrupts()
        .unwrap()
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Install AP1 payload (protected-mode LAPIC IPI sender) at 0x8000 and AP2 payload (flag setter)
    // at 0x9000.
    let ap1_code = build_ap1_protected_mode_init_sipi_sender(2, AP2_SIPI_VECTOR);
    let ap2_code = build_ap2_real_mode_flag_setter(FLAG_ADDR as u16, FLAG_VALUE);
    m.write_physical(AP1_PADDR, &ap1_code);
    m.write_physical(AP2_PADDR, &ap2_code);
    m.write_physical_u8(FLAG_ADDR, 0);

    // Start AP1 via host-facing LAPIC MMIO APIs.
    const ICR_LOW_OFF: u64 = 0x300;
    const ICR_HIGH_OFF: u64 = 0x310;

    let icr_high = 1u32 << 24;
    let icr_init_low = (0b101u32 << 8) | (1u32 << 14);
    let icr_sipi_low = u32::from(AP1_SIPI_VECTOR) | (0b110u32 << 8) | (1u32 << 14);

    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_init_low);
    m.write_lapic_u32(0, ICR_HIGH_OFF, icr_high);
    m.write_lapic_u32(0, ICR_LOW_OFF, icr_sipi_low);

    // Run slices until AP2 flips the sentinel byte.
    for _ in 0..200 {
        let _ = m.run_slice(1000);
        if m.read_physical_u8(FLAG_ADDR) == FLAG_VALUE {
            // Sanity: AP2 should have been started with vector 0x09 (physical 0x9000).
            let ap2 = m.cpu_by_index(2);
            assert_eq!(ap2.segments.cs.selector, u16::from(AP2_SIPI_VECTOR) << 8);
            assert_eq!(ap2.segments.cs.base, AP2_PADDR);
            return;
        }
    }

    panic!(
        "AP2 did not run after AP1 sent INIT+SIPI (flag=0x{:02x}, expected=0x{:02x})",
        m.read_physical_u8(FLAG_ADDR),
        FLAG_VALUE
    );
}
