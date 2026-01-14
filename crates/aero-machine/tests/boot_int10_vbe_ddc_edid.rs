use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

const RESULT_BUF_BASE: u64 = 0x0500;
const RESULT0_AX: u64 = RESULT_BUF_BASE;
const RESULT0_BX: u64 = RESULT_BUF_BASE + 2;
const RESULT0_FLAGS: u64 = RESULT_BUF_BASE + 4;
const RESULT1_AX: u64 = RESULT_BUF_BASE + 6;
const RESULT1_FLAGS: u64 = RESULT_BUF_BASE + 8;

const EDID_BUF_ADDR: u64 = 0x0600;

fn build_int10_vbe_ddc_edid_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;

    // --------------------------
    // VBE DDC capability query
    // --------------------------
    // mov ax, 0x4F15
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x15, 0x4F]);
    i += 3;
    // xor bx, bx  ; BL=0x00
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;
    // pushf ; capture CF
    sector[i] = 0x9C;
    i += 1;
    // pop cx
    sector[i] = 0x59;
    i += 1;

    // Restore DS=0 while preserving AX (the return code).
    // push ax
    sector[i] = 0x50;
    i += 1;
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // pop ax
    sector[i] = 0x58;
    i += 1;

    // mov [0x0500], ax
    sector[i..i + 3].copy_from_slice(&[0xA3, 0x00, 0x05]);
    i += 3;
    // mov [0x0502], bx
    sector[i..i + 4].copy_from_slice(&[0x89, 0x1E, 0x02, 0x05]);
    i += 4;
    // mov [0x0504], cx
    sector[i..i + 4].copy_from_slice(&[0x89, 0x0E, 0x04, 0x05]);
    i += 4;

    // --------------------------
    // VBE DDC EDID read (block 0)
    // --------------------------
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov es, ax  ; ES=0x0000
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov di, 0x0600 ; EDID buffer
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x06]);
    i += 3;
    // xor dx, dx ; block 0
    sector[i..i + 2].copy_from_slice(&[0x31, 0xD2]);
    i += 2;
    // mov bx, 0x0001 ; BL=0x01
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x01, 0x00]);
    i += 3;
    // mov ax, 0x4F15
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x15, 0x4F]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;
    // pushf
    sector[i] = 0x9C;
    i += 1;
    // pop cx
    sector[i] = 0x59;
    i += 1;

    // Restore DS=0 while preserving AX.
    // push ax
    sector[i] = 0x50;
    i += 1;
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // pop ax
    sector[i] = 0x58;
    i += 1;

    // mov [0x0506], ax
    sector[i..i + 3].copy_from_slice(&[0xA3, 0x06, 0x05]);
    i += 3;
    // mov [0x0508], cx
    sector[i..i + 4].copy_from_slice(&[0x89, 0x0E, 0x08, 0x05]);
    i += 4;

    // hlt
    sector[i] = 0xF4;

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
fn boot_int10_vbe_ddc_edid() {
    let boot = build_int10_vbe_ddc_edid_boot_sector();

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

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    let ax0 = m.read_physical_u16(RESULT0_AX);
    let bx0 = m.read_physical_u16(RESULT0_BX);
    let flags0 = m.read_physical_u16(RESULT0_FLAGS);

    assert_eq!(ax0, 0x004F);
    assert_eq!(bx0, 0x0200);
    assert_eq!(flags0 & 0x0001, 0, "CF set for VBE DDC capability query");

    let ax1 = m.read_physical_u16(RESULT1_AX);
    let flags1 = m.read_physical_u16(RESULT1_FLAGS);

    assert_eq!(ax1, 0x004F);
    assert_eq!(flags1 & 0x0001, 0, "CF set for VBE DDC EDID read");

    let expected = aero_edid::read_edid(0).expect("missing base EDID");
    let mut actual = [0u8; aero_edid::EDID_BLOCK_SIZE];
    for (i, b) in actual.iter_mut().enumerate() {
        *b = m.read_physical_u8(EDID_BUF_ADDR + i as u64);
    }
    assert_eq!(actual, expected);
}
