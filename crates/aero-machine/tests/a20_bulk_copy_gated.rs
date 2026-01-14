use aero_devices::a20_gate::A20_GATE_PORT;
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

fn build_a20_aliasing_rep_movsb_df1_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    // Exercise a corner case where:
    // - we run in 32-bit protected mode (so `CpuState::apply_a20` does not mask linear addresses),
    // - the chipset A20 gate is disabled (masking physical bit 20),
    // - and REP MOVSB runs with DF=1 (copy high->low).
    //
    // We arrange for the source and destination *linear* ranges to be disjoint, but for the
    // destination to alias into the source range after A20 masking. If Tier-0 incorrectly uses the
    // `CpuBus::bulk_copy` fast path (memmove semantics based on linear overlap), it will copy
    // forward and corrupt the overlapping region. Correct CPU semantics for DF=1 should preserve
    // the source bytes.
    const SRC_BASE: u32 = 0x0000_2000;
    const DST_BASE: u32 = 0x0010_2010; // aliases to 0x0000_2010 when A20 is disabled
    const LEN: u32 = 64;

    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // -------------------------------------------------------------------------
    // Real mode: initialize src bytes [SRC_BASE..SRC_BASE+LEN) with values 0..LEN-1.
    // -------------------------------------------------------------------------

    // cli
    sector[i] = 0xFA;
    i += 1;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // mov si, SRC_BASE
    sector[i] = 0xBE;
    i += 1;
    sector[i..i + 2].copy_from_slice(&(SRC_BASE as u16).to_le_bytes());
    i += 2;

    // mov cx, LEN
    sector[i] = 0xB9;
    i += 1;
    sector[i..i + 2].copy_from_slice(&(LEN as u16).to_le_bytes());
    i += 2;

    let loop_start = i;
    // mov [si], al
    sector[i..i + 2].copy_from_slice(&[0x88, 0x04]);
    i += 2;
    // inc al
    sector[i..i + 2].copy_from_slice(&[0xFE, 0xC0]);
    i += 2;
    // inc si
    sector[i] = 0x46;
    i += 1;
    // loop rel8
    sector[i] = 0xE2;
    i += 1;
    let next_ip = i + 1;
    let rel = (loop_start as i32 - next_ip as i32) as i8;
    sector[i] = rel as u8;
    i += 1;

    // -------------------------------------------------------------------------
    // Enter protected mode.
    // -------------------------------------------------------------------------

    // lgdt [gdtr]
    sector[i..i + 3].copy_from_slice(&[0x0F, 0x01, 0x16]);
    i += 3;
    let gdtr_disp_pos = i;
    sector[i..i + 2].fill(0);
    i += 2;

    // mov eax, cr0  (66 0F 20 C0)
    sector[i..i + 4].copy_from_slice(&[0x66, 0x0F, 0x20, 0xC0]);
    i += 4;
    // or eax, 1     (66 83 C8 01)
    sector[i..i + 4].copy_from_slice(&[0x66, 0x83, 0xC8, 0x01]);
    i += 4;
    // mov cr0, eax  (66 0F 22 C0)
    sector[i..i + 4].copy_from_slice(&[0x66, 0x0F, 0x22, 0xC0]);
    i += 4;

    // far jmp to flush prefetch queue: jmp 0x08:prot_entry (EA off16 sel16)
    sector[i] = 0xEA;
    i += 1;
    let far_off_pos = i;
    sector[i..i + 2].fill(0);
    i += 2;
    sector[i..i + 2].copy_from_slice(&0x0008u16.to_le_bytes());
    i += 2;

    let prot_entry_offset = i;

    // -------------------------------------------------------------------------
    // Protected mode entry (32-bit code segment).
    // -------------------------------------------------------------------------

    // mov ax, 0x10 (data selector)  (66 B8 10 00)
    sector[i..i + 4].copy_from_slice(&[0x66, 0xB8, 0x10, 0x00]);
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
    // mov fs, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xE0]);
    i += 2;
    // mov gs, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xE8]);
    i += 2;

    // mov esp, 0x7000
    sector[i] = 0xBC;
    i += 1;
    sector[i..i + 4].copy_from_slice(&0x0000_7000u32.to_le_bytes());
    i += 4;

    // Disable A20 (fast A20 gate): out 0x92, 0
    sector[i..i + 2].copy_from_slice(&[0xB0, 0x00]); // mov al, 0
    i += 2;
    sector[i..i + 2].copy_from_slice(&[0xE6, A20_GATE_PORT as u8]); // out 0x92, al
    i += 2;

    // std (DF=1)
    sector[i] = 0xFD;
    i += 1;

    // mov esi, src_end
    sector[i] = 0xBE;
    i += 1;
    sector[i..i + 4].copy_from_slice(&(SRC_BASE + LEN - 1).to_le_bytes());
    i += 4;

    // mov edi, dst_end
    sector[i] = 0xBF;
    i += 1;
    sector[i..i + 4].copy_from_slice(&(DST_BASE + LEN - 1).to_le_bytes());
    i += 4;

    // mov ecx, LEN
    sector[i] = 0xB9;
    i += 1;
    sector[i..i + 4].copy_from_slice(&LEN.to_le_bytes());
    i += 4;

    // rep movsb
    sector[i..i + 2].copy_from_slice(&[0xF3, 0xA4]);
    i += 2;

    // hlt
    sector[i] = 0xF4;
    i += 1;

    // -------------------------------------------------------------------------
    // GDT + GDTR.
    // -------------------------------------------------------------------------

    let gdt_offset = align_up(i, 8);
    const GDT_SIZE: usize = 8 * 3;
    let gdtr_offset = gdt_offset + GDT_SIZE;
    assert!(
        gdtr_offset + 6 <= 510,
        "boot sector too small for GDT/GDTR (code_len={i} gdt_offset={gdt_offset})"
    );

    // Null descriptor.
    sector[gdt_offset..gdt_offset + 8].fill(0);
    // Code descriptor: base=0, limit=4GiB, present, ring0, code, readable, 32-bit, 4KiB gran.
    sector[gdt_offset + 8..gdt_offset + 16].copy_from_slice(&[
        0xFF, 0xFF, // limit low
        0x00, 0x00, // base low
        0x00, // base mid
        0x9A, // access
        0xCF, // flags + limit high
        0x00, // base high
    ]);
    // Data descriptor: base=0, limit=4GiB, present, ring0, data, writable, 32-bit, 4KiB gran.
    sector[gdt_offset + 16..gdt_offset + 24].copy_from_slice(&[
        0xFF, 0xFF, // limit low
        0x00, 0x00, // base low
        0x00, // base mid
        0x92, // access
        0xCF, // flags + limit high
        0x00, // base high
    ]);

    let gdtr_phys = 0x7C00u32 + gdtr_offset as u32;
    let gdt_phys = 0x7C00u32 + gdt_offset as u32;

    // Patch LGDT disp16.
    sector[gdtr_disp_pos..gdtr_disp_pos + 2].copy_from_slice(&(gdtr_phys as u16).to_le_bytes());

    // Patch far jump offset.
    let prot_entry_phys = 0x7C00u32 + prot_entry_offset as u32;
    sector[far_off_pos..far_off_pos + 2].copy_from_slice(&(prot_entry_phys as u16).to_le_bytes());

    // GDTR: limit (u16) + base (u32).
    let limit = (GDT_SIZE as u16).wrapping_sub(1);
    sector[gdtr_offset..gdtr_offset + 2].copy_from_slice(&limit.to_le_bytes());
    sector[gdtr_offset + 2..gdtr_offset + 6].copy_from_slice(&gdt_phys.to_le_bytes());

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xAA;

    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn a20_disabled_gates_rep_movs_bulk_fast_path_even_in_protected_mode() {
    const SRC_BASE: u64 = 0x0000_2000;
    const DEST_ALIAS_PHYS: u64 = 0x0000_2010;
    const LEN: usize = 64;

    let boot = build_a20_aliasing_rep_movsb_df1_boot_sector();

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_vga: false,
        // Keep the fast A20 gate enabled so the guest can toggle A20 via port 0x92.
        enable_a20_gate: true,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Destination aliases into low memory at DEST_ALIAS_PHYS when A20 is disabled.
    let got = m.read_physical_bytes(DEST_ALIAS_PHYS, LEN);
    let expected: Vec<u8> = (0..LEN as u8).collect();
    assert_eq!(got, expected);

    // Sanity: source bytes are written at SRC_BASE and are not all clobbered by the overlapping
    // copy (only bytes SRC_BASE+0x10..SRC_BASE+0x3F overlap with destination).
    let src_head = m.read_physical_bytes(SRC_BASE, 0x10);
    let expected_head: Vec<u8> = (0..0x10u8).collect();
    assert_eq!(src_head, expected_head);
}
