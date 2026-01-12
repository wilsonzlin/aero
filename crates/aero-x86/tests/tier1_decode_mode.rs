use aero_types::{Gpr, Width};
use aero_x86::tier1::{decode_one_mode, InstKind, Operand, Reg};

#[test]
fn decode_one_mode_32bit_dec_ecx_is_not_rex() {
    let inst = decode_one_mode(0x1000, &[0x49], 32);
    assert_eq!(inst.len, 1);
    assert_eq!(
        inst.kind,
        InstKind::Dec {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rcx,
                width: Width::W32,
                high8: false,
            }),
            width: Width::W32,
        }
    );
}

#[test]
fn decode_one_mode_32bit_modrm_disp32_is_not_rip_relative() {
    // mov eax, [0x1234]
    //
    // In 32-bit mode this is absolute disp32 addressing.
    // In 64-bit mode the same encoding would be RIP-relative.
    let inst = decode_one_mode(0x1000, &[0x8b, 0x05, 0x34, 0x12, 0x00, 0x00], 32);
    assert_eq!(inst.len, 6);
    assert_eq!(
        inst.kind,
        InstKind::Mov {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rax,
                width: Width::W32,
                high8: false
            }),
            src: Operand::Mem(aero_x86::tier1::Address {
                base: None,
                index: None,
                scale: 1,
                disp: 0x1234,
                rip_relative: false,
            }),
            width: Width::W32,
        }
    );
}
