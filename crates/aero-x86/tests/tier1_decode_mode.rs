use aero_types::{Gpr, Width};
use aero_x86::tier1::{decode_one_mode, InstKind, Operand, Reg, ShiftOp};

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

#[test]
fn decode_one_mode_32bit_d0_disp32_is_not_rip_relative() {
    // shl byte ptr [0x1234], 1
    //
    // `0xD0 /4` uses the Group2 shift decoder path in Tier1.
    // The ModRM `mod=00 rm=101` encoding is absolute disp32 addressing in 32-bit mode.
    let inst = decode_one_mode(0x1000, &[0xd0, 0x25, 0x34, 0x12, 0x00, 0x00], 32);
    assert_eq!(inst.len, 6);
    assert_eq!(
        inst.kind,
        InstKind::Shift {
            op: ShiftOp::Shl,
            dst: Operand::Mem(aero_x86::tier1::Address {
                base: None,
                index: None,
                scale: 1,
                disp: 0x1234,
                rip_relative: false,
            }),
            count: 1,
            width: Width::W8,
        }
    );
}

#[test]
fn decode_one_mode_32bit_jmp_rel8_wraps_eip() {
    // jmp +1 at 0xFFFF_FFFF:
    // next EIP would be 0x1_0000_0001, plus rel8(1) => 0x1_0000_0002, then wrapped to 0x0000_0002.
    let inst = decode_one_mode(0xffff_ffff, &[0xeb, 0x01], 32);
    assert_eq!(inst.len, 2);
    assert_eq!(inst.kind, InstKind::JmpRel { target: 0x2 });
}

#[test]
fn decode_one_mode_16bit_inc_ax_is_not_rex() {
    // 0x40 is INC AX in 16-bit mode; it must not be consumed as a REX prefix.
    let inst = decode_one_mode(0x1000, &[0x40], 16);
    assert_eq!(inst.len, 1);
    assert_eq!(
        inst.kind,
        InstKind::Inc {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rax,
                width: Width::W16,
                high8: false,
            }),
            width: Width::W16,
        }
    );
}

#[test]
fn decode_one_mode_16bit_opsize_override_inc_eax() {
    // In 16-bit mode, 0x66 selects 32-bit operand size.
    // 66 40 = inc eax
    let inst = decode_one_mode(0x1000, &[0x66, 0x40], 16);
    assert_eq!(inst.len, 2);
    assert_eq!(
        inst.kind,
        InstKind::Inc {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rax,
                width: Width::W32,
                high8: false,
            }),
            width: Width::W32,
        }
    );
}

#[test]
fn decode_one_mode_32bit_opsize_override_dec_cx() {
    // In 32-bit mode, 0x66 selects 16-bit operand size.
    // 66 49 = dec cx
    let inst = decode_one_mode(0x1000, &[0x66, 0x49], 32);
    assert_eq!(inst.len, 2);
    assert_eq!(
        inst.kind,
        InstKind::Dec {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rcx,
                width: Width::W16,
                high8: false,
            }),
            width: Width::W16,
        }
    );
}

#[test]
fn decode_one_mode_16bit_modrm_bx_si_disp8() {
    // mov ax, [bx+si+0x10]
    //
    // 16-bit addressing uses the ModRM rm encoding table (no SIB).
    let inst = decode_one_mode(0x1000, &[0x8b, 0x40, 0x10], 16);
    assert_eq!(inst.len, 3);
    assert_eq!(
        inst.kind,
        InstKind::Mov {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rax,
                width: Width::W16,
                high8: false,
            }),
            src: Operand::Mem(aero_x86::tier1::Address {
                base: Some(Gpr::Rbx),
                index: Some(Gpr::Rsi),
                scale: 1,
                disp: 0x10,
                rip_relative: false,
            }),
            width: Width::W16,
        }
    );
}
