use aero_types::{Cond, Gpr, Width};
use aero_x86::tier1::{decode_one_mode, AluOp, InstKind, Operand, Reg, ShiftOp};

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

#[test]
fn decode_one_mode_32bit_push_ebx_uses_w32() {
    let inst = decode_one_mode(0x1000, &[0x53], 32);
    assert_eq!(inst.len, 1);
    assert_eq!(
        inst.kind,
        InstKind::Push {
            src: Operand::Reg(Reg {
                gpr: Gpr::Rbx,
                width: Width::W32,
                high8: false,
            }),
        }
    );
}

#[test]
fn decode_one_mode_32bit_pop_ebx_uses_w32() {
    let inst = decode_one_mode(0x1000, &[0x5b], 32);
    assert_eq!(inst.len, 1);
    assert_eq!(
        inst.kind,
        InstKind::Pop {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rbx,
                width: Width::W32,
                high8: false,
            }),
        }
    );
}

#[test]
fn decode_one_mode_64bit_push_pop_use_w64() {
    let push = decode_one_mode(0x1000, &[0x53], 64);
    assert_eq!(push.len, 1);
    assert_eq!(
        push.kind,
        InstKind::Push {
            src: Operand::Reg(Reg {
                gpr: Gpr::Rbx,
                width: Width::W64,
                high8: false,
            }),
        }
    );

    let pop = decode_one_mode(0x1000, &[0x5b], 64);
    assert_eq!(pop.len, 1);
    assert_eq!(
        pop.kind,
        InstKind::Pop {
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rbx,
                width: Width::W64,
                high8: false,
            }),
        }
    );
}

#[test]
fn decode_one_mode_16bit_jmp_rel16_uses_imm16() {
    // jmp near -2 (rel16) in 16-bit mode:
    // next IP = 0x1003, target = 0x1001
    let inst = decode_one_mode(0x1000, &[0xe9, 0xfe, 0xff], 16);
    assert_eq!(inst.len, 3);
    assert_eq!(inst.kind, InstKind::JmpRel { target: 0x1001 });
}

#[test]
fn decode_one_mode_32bit_opsize_override_jmp_rel16() {
    // In 32-bit mode, 0x66 selects 16-bit operand size, and near JMP uses a rel16.
    // 66 E9 FC FF = jmp -4
    // next EIP = 0x1004, target = 0x1000
    let inst = decode_one_mode(0x1000, &[0x66, 0xe9, 0xfc, 0xff], 32);
    assert_eq!(inst.len, 4);
    assert_eq!(inst.kind, InstKind::JmpRel { target: 0x1000 });
}

#[test]
fn decode_one_mode_16bit_call_rel16_uses_imm16() {
    // call near +0 (rel16) in 16-bit mode:
    // next IP = 0x2003, target = 0x2003
    let inst = decode_one_mode(0x2000, &[0xe8, 0x00, 0x00], 16);
    assert_eq!(inst.len, 3);
    assert_eq!(inst.kind, InstKind::CallRel { target: 0x2003 });
}

#[test]
fn decode_one_mode_16bit_jcc_rel16_uses_imm16() {
    // jnz near -2 (rel16): 0F 85 FE FF
    // next IP = 0x3004, target = 0x3002
    let inst = decode_one_mode(0x3000, &[0x0f, 0x85, 0xfe, 0xff], 16);
    assert_eq!(inst.len, 4);
    assert_eq!(
        inst.kind,
        InstKind::JccRel {
            cond: Cond::Ne,
            target: 0x3002
        }
    );
}

#[test]
fn decode_one_mode_32bit_opsize_override_jcc_rel16() {
    // In 32-bit mode, 0x66 selects 16-bit operand size, and near Jcc uses a rel16.
    // 66 0F 84 02 00 = jz +2
    // next EIP = 0x4005, target = 0x4007
    let inst = decode_one_mode(0x4000, &[0x66, 0x0f, 0x84, 0x02, 0x00], 32);
    assert_eq!(inst.len, 5);
    assert_eq!(
        inst.kind,
        InstKind::JccRel {
            cond: Cond::E,
            target: 0x4007
        }
    );
}

#[test]
fn decode_one_mode_16bit_add_ax_imm16_is_not_imm32() {
    // add ax, 0x1234
    let inst = decode_one_mode(0x1000, &[0x05, 0x34, 0x12], 16);
    assert_eq!(inst.len, 3);
    assert_eq!(
        inst.kind,
        InstKind::Alu {
            op: AluOp::Add,
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rax,
                width: Width::W16,
                high8: false,
            }),
            src: Operand::Imm(0x1234),
            width: Width::W16,
        }
    );
}

#[test]
fn decode_one_mode_32bit_opsize_override_add_ax_imm16() {
    // 66 05 34 12 = add ax, 0x1234
    let inst = decode_one_mode(0x1000, &[0x66, 0x05, 0x34, 0x12], 32);
    assert_eq!(inst.len, 4);
    assert_eq!(
        inst.kind,
        InstKind::Alu {
            op: AluOp::Add,
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rax,
                width: Width::W16,
                high8: false,
            }),
            src: Operand::Imm(0x1234),
            width: Width::W16,
        }
    );
}

#[test]
fn decode_one_mode_16bit_81_group_reads_imm16() {
    // 81 /0 = add r/m16, imm16
    // add ax, 0x1234
    let inst = decode_one_mode(0x1000, &[0x81, 0xc0, 0x34, 0x12], 16);
    assert_eq!(inst.len, 4);
    assert_eq!(
        inst.kind,
        InstKind::Alu {
            op: AluOp::Add,
            dst: Operand::Reg(Reg {
                gpr: Gpr::Rax,
                width: Width::W16,
                high8: false,
            }),
            src: Operand::Imm(0x1234),
            width: Width::W16,
        }
    );
}

#[test]
fn decode_one_mode_16bit_push_imm16_reads_imm16() {
    // push 0x1234
    let inst = decode_one_mode(0x1000, &[0x68, 0x34, 0x12], 16);
    assert_eq!(inst.len, 3);
    assert_eq!(
        inst.kind,
        InstKind::Push {
            src: Operand::Imm(0x1234),
        }
    );
}

#[test]
fn decode_one_mode_32bit_opsize_override_push_imm16() {
    // In 32-bit mode, 0x66 selects 16-bit operand size, and PUSH imm uses an imm16.
    let inst = decode_one_mode(0x1000, &[0x66, 0x68, 0x34, 0x12], 32);
    assert_eq!(inst.len, 4);
    // Tier-1 translation does not currently model operand-size overridden stack ops, so the
    // minimal decoder treats them as unsupported to avoid miscompilation.
    assert_eq!(inst.kind, InstKind::Invalid);
}

#[test]
fn decode_one_mode_bails_on_address_size_override_prefix() {
    // 67 8B 07 would be `mov eax, [bx]`-style (16-bit) addressing in 32-bit mode, which Tier-1
    // does not currently model. Ensure we bail out instead of mis-decoding the address.
    let inst = decode_one_mode(0x1000, &[0x67, 0x8b, 0x07], 32);
    assert_eq!(inst.len, 2);
    assert_eq!(inst.kind, InstKind::Invalid);
}
