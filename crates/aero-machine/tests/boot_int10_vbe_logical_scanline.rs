use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_logical_scanline_boot_sector(lfb_base: u32) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // Real-mode setup: DS=0 so we can address our GDT/GDTR with absolute disp16 operands.
    //
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;

    // INT 10h AX=4F02: Set VBE mode 0x118 with linear framebuffer (BX=0x4118).
    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4118
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // INT 10h AX=4F06: Set logical scanline length in pixels to 2048 (BL=0x00, CX=2048).
    // mov ax, 0x4F06
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x06, 0x4F]);
    i += 3;
    // xor bx, bx (BL=0x00)
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // mov cx, 0x0800 (2048)
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x00, 0x08]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Re-assert DS=0 in case BIOS calls clobbered it.
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // Switch to protected mode (32-bit flat segments) so we can write to the VBE LFB.
    //
    // cli
    sector[i] = 0xFA;
    i += 1;

    // lgdt [gdtr]
    //
    // The boot sector is loaded at 0x0000:0x7C00. We store the GDTR at offset 0x1D8 within the
    // sector, so its physical address is 0x7C00 + 0x1D8 = 0x7DD8.
    const GDTR_PHYS: u16 = 0x7DD8;
    sector[i..i + 5].copy_from_slice(&[
        0x0F,
        0x01,
        0x16,
        (GDTR_PHYS & 0xFF) as u8,
        (GDTR_PHYS >> 8) as u8,
    ]);
    i += 5;

    // mov eax, cr0
    sector[i..i + 3].copy_from_slice(&[0x0F, 0x20, 0xC0]);
    i += 3;
    // or eax, 1  (operand-size override so this is 32-bit in real-mode code)
    sector[i..i + 4].copy_from_slice(&[0x66, 0x83, 0xC8, 0x01]);
    i += 4;
    // mov cr0, eax
    sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xC0]);
    i += 3;

    // Far jump into protected mode (flush prefetch + load CS).
    //
    // Place the protected-mode entrypoint immediately after this JMP so we can compute the target
    // IP as a constant.
    const CODE_SEL: u16 = 0x0008;
    let pm_entry_ip: u16 = 0x7C00u16.wrapping_add((i as u16).wrapping_add(5));
    sector[i..i + 5].copy_from_slice(&[
        0xEA,
        (pm_entry_ip & 0xFF) as u8,
        (pm_entry_ip >> 8) as u8,
        (CODE_SEL & 0xFF) as u8,
        (CODE_SEL >> 8) as u8,
    ]);
    i += 5;

    // -------------------------------------------------------------------------
    // Protected-mode (32-bit) entrypoint.
    // -------------------------------------------------------------------------
    const DATA_SEL: u16 = 0x0010;

    // mov ax, DATA_SEL (operand-size override: 16-bit immediate -> AX)
    sector[i..i + 4].copy_from_slice(&[0x66, 0xB8, (DATA_SEL & 0xFF) as u8, (DATA_SEL >> 8) as u8]);
    i += 4;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov ss, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD0]);
    i += 2;
    // mov esp, 0x00007C00
    sector[i..i + 5].copy_from_slice(&[0xBC, 0x00, 0x7C, 0x00, 0x00]);
    i += 5;

    // Write BGRX=00 00 FF 00 to LFB_BASE + 8192 (row 1 with stride_pixels=2048).
    //
    // mov dword ptr [addr32], imm32
    let lfb_row1 = lfb_base.wrapping_add(2048 * 4);
    sector[i..i + 2].copy_from_slice(&[0xC7, 0x05]);
    i += 2;
    sector[i..i + 4].copy_from_slice(&lfb_row1.to_le_bytes());
    i += 4;
    sector[i..i + 4].copy_from_slice(&0x00FF_0000u32.to_le_bytes());
    i += 4;

    // hlt
    sector[i] = 0xF4;

    // -------------------------------------------------------------------------
    // GDT + GDTR (used by the protected-mode switch)
    // -------------------------------------------------------------------------
    //
    // Layout:
    // - GDT at sector offset 0x1C0 (0x7DC0 physical)
    // - GDTR at sector offset 0x1D8 (0x7DD8 physical)
    const GDT_OFFSET: usize = 0x1C0;
    const GDTR_OFFSET: usize = 0x1D8;
    const GDT_PHYS_BASE: u32 = 0x0000_7C00u32 + (GDT_OFFSET as u32);

    // Ensure we didn't run into the reserved GDT region.
    assert!(i < GDT_OFFSET, "boot sector code too large: i={i}");

    // GDT: null, code, data (each 8 bytes).
    let gdt: [u8; 24] = [
        // null
        0, 0, 0, 0, 0, 0, 0, 0, // code: base=0, limit=4GiB, 32-bit
        0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0xCF, 0x00,
        // data: base=0, limit=4GiB, 32-bit
        0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0xCF, 0x00,
    ];
    sector[GDT_OFFSET..GDT_OFFSET + gdt.len()].copy_from_slice(&gdt);

    // GDTR pseudo-descriptor: limit (u16) + base (u32).
    let gdtr: [u8; 6] = [
        0x17,
        0x00, // limit = 24-1
        (GDT_PHYS_BASE & 0xFF) as u8,
        ((GDT_PHYS_BASE >> 8) & 0xFF) as u8,
        ((GDT_PHYS_BASE >> 16) & 0xFF) as u8,
        ((GDT_PHYS_BASE >> 24) & 0xFF) as u8,
    ];
    sector[GDTR_OFFSET..GDTR_OFFSET + gdtr.len()].copy_from_slice(&gdtr);

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn boot_int10_vbe_logical_scanline_updates_stride_and_renderer_uses_it() {
    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    let boot = build_int10_vbe_logical_scanline_boot_sector(m.vbe_lfb_base_u32());

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    m.display_present();
    assert_eq!(m.display_resolution(), (1024, 768));
    assert_eq!(m.display_framebuffer()[1024], 0xFF00_00FF);
}
