use aero_x86::inst::{Operand, OperandSize, RepPrefix, SegmentReg};
use aero_x86::decoder::{decode, DecodeError, DecodeMode};

#[test]
fn prefixes_precedence_and_rex_high8() {
    // REP prefixes: last one wins.
    let inst = decode(&[0xF3, 0xF2, 0x90], DecodeMode::Bits32, 0).unwrap();
    assert_eq!(inst.prefixes.rep, Some(RepPrefix::Repne));

    // REX presence disables AH/CH/DH/BH and enables SPL/BPL/SIL/DIL.
    let inst = decode(&[0x40, 0x88, 0xC4], DecodeMode::Bits64, 0).unwrap();
    assert!(inst.prefixes.rex.is_some());
    assert!(inst
        .operands
        .iter()
        .any(|op| matches!(op, Operand::Gpr { reg, size: OperandSize::Bits8, high8: false } if reg.index == 4)));

    let inst = decode(&[0x88, 0xC4], DecodeMode::Bits64, 0).unwrap();
    assert!(inst.prefixes.rex.is_none());
    assert!(inst
        .operands
        .iter()
        .any(|op| matches!(op, Operand::Gpr { reg, size: OperandSize::Bits8, high8: true } if reg.index == 0)));

    // REX.W takes precedence over 0x66 in long mode when selecting the effective operand size.
    let inst = decode(&[0x66, 0x48, 0x90], DecodeMode::Bits64, 0).unwrap();
    assert_eq!(inst.operand_size, OperandSize::Bits64);
}

#[test]
fn modrm_sib_addressing_and_rip_relative() {
    // mov eax, [rax+rcx*4+0x10]
    let inst = decode(&[0x8B, 0x44, 0x88, 0x10], DecodeMode::Bits64, 0).unwrap();
    let mem = inst
        .operands
        .iter()
        .find_map(|op| match op {
            Operand::Memory(m) => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!(mem.base.map(|r| r.index), Some(0));
    assert_eq!(mem.index.map(|r| r.index), Some(1));
    assert_eq!(mem.scale, 4);
    assert_eq!(mem.disp, 16);
    assert!(!mem.rip_relative);

    // mov eax, [rip+0x1234]
    let inst = decode(&[0x8B, 0x05, 0x34, 0x12, 0x00, 0x00], DecodeMode::Bits64, 0).unwrap();
    let mem = inst
        .operands
        .iter()
        .find_map(|op| match op {
            Operand::Memory(m) => Some(m),
            _ => None,
        })
        .unwrap();
    assert!(mem.rip_relative);
    assert_eq!(mem.base, None);
    assert_eq!(mem.index, None);
    assert_eq!(mem.disp, 0x1234);

    // mov eax, [rcx*4 + 0x10] (no base)
    let inst = decode(
        &[0x8B, 0x04, 0x8D, 0x10, 0x00, 0x00, 0x00],
        DecodeMode::Bits64,
        0,
    )
    .unwrap();
    let mem = inst
        .operands
        .iter()
        .find_map(|op| match op {
            Operand::Memory(m) => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!(mem.base, None);
    assert_eq!(mem.index.map(|r| r.index), Some(1));
    assert_eq!(mem.scale, 4);
    assert_eq!(mem.disp, 16);

    // 16-bit addressing: mov ax, [bx+si+0x10]
    let inst = decode(&[0x8B, 0x40, 0x10], DecodeMode::Bits16, 0).unwrap();
    let mem = inst
        .operands
        .iter()
        .find_map(|op| match op {
            Operand::Memory(m) => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!(mem.base.map(|r| r.index), Some(3)); // BX
    assert_eq!(mem.index.map(|r| r.index), Some(6)); // SI
    assert_eq!(mem.scale, 1);
    assert_eq!(mem.disp, 16);
}

#[test]
fn enforces_15_byte_max_len() {
    // A valid 15-byte instruction (4 prefixes + 11-byte `imul r32, r/m32, imm32` with SIB+disp32).
    //
    // Prefixes: multiple segment overrides are permitted; only the last one is effective.
    let bytes = [
        0x2E, 0x36, 0x3E, 0x26, // CS, SS, DS, ES (effective = ES)
        0x69, 0x84, 0x8A, // imul r32, r/m32, imm32 + ModRM + SIB
        0x00, 0x00, 0x00, 0x00, // disp32
        0x00, 0x00, 0x00, 0x00, // imm32
    ];
    let inst = decode(&bytes, DecodeMode::Bits32, 0).unwrap();
    assert_eq!(inst.length, 15);
    assert!(inst.prefixes.segment.is_some());

    // 15 prefixes leaves no room for an opcode.
    let bytes = vec![0x66u8; 15];
    assert!(matches!(
        decode(&bytes, DecodeMode::Bits32, 0),
        Err(DecodeError::TooLong)
    ));

    // 15 prefixes + 1 opcode => 16-byte instruction => reject.
    let mut bytes = vec![0x66u8; 15];
    bytes.push(0x90);
    assert!(matches!(
        decode(&bytes, DecodeMode::Bits32, 0),
        Err(DecodeError::TooLong)
    ));
}

#[test]
fn decodes_large_buffer_without_panics() {
    // Deterministic pseudo-random byte stream.
    let mut buf = [0u8; 8192];
    let mut x = 0x1234_5678u32;
    for b in &mut buf {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        *b = (x >> 24) as u8;
    }

    let mut off = 0usize;
    while off < buf.len() {
        let end = (off + 15).min(buf.len());
        let slice = &buf[off..end];
        match decode(slice, DecodeMode::Bits64, off as u64) {
            Ok(inst) if inst.length > 0 => off += inst.length as usize,
            _ => off += 1,
        }
    }
}

#[test]
fn decodes_simple_reg_reg_operands_in_16bit_mode() {
    // add al, al
    let inst = decode(&[0x00, 0xC0], DecodeMode::Bits16, 0).unwrap();
    assert_eq!(inst.operands.len(), 2);
    assert!(matches!(inst.operands[0], Operand::Gpr { .. }));
    assert!(matches!(inst.operands[1], Operand::Gpr { .. }));
}

#[test]
fn unary_group_ops_collapse_dst_dst() {
    // not al (F6 /2)
    let inst = decode(&[0xF6, 0xD0], DecodeMode::Bits16, 0).unwrap();
    assert_eq!(inst.operands.len(), 1);
}

#[test]
fn regression_cases_are_stable() {
    // 16-bit relative branches wrap the IP like real hardware (iced-x86 behaviour).
    let inst = decode(&[0x70, 0x80], DecodeMode::Bits16, 0).unwrap();
    assert!(inst
        .operands
        .iter()
        .any(|op| matches!(op, Operand::Relative { target: 0xFF82, .. })));
    // With an operand-size override, the branch uses EIP semantics (wraps to 32-bit).
    let inst = decode(&[0x66, 0x70, 0x80, 0x00], DecodeMode::Bits16, 0).unwrap();
    assert!(inst.operands.iter().any(|op| matches!(
        op,
        Operand::Relative {
            target: 0xFFFF_FF83,
            ..
        }
    )));

    // In 64-bit mode, REX participates in the same prefix stream as legacy prefixes.
    let inst = decode(&[0x40, 0x64, 0x70, 0x00], DecodeMode::Bits64, 0).unwrap();
    assert_eq!(inst.length, 4);
    assert_eq!(inst.prefixes.segment, Some(SegmentReg::FS));
    assert!(inst.prefixes.rex.is_some());
    assert!(inst
        .operands
        .iter()
        .any(|op| matches!(op, Operand::Relative { target: 4, .. })));

    // XLAT, INT3, IRET, and ICEBP have implicit operands that must be normalised.
    let inst = decode(&[0xD7], DecodeMode::Bits16, 0).unwrap();
    assert!(inst
        .operands
        .iter()
        .any(|op| matches!(op, Operand::Memory(_))));

    let inst = decode(&[0xCC], DecodeMode::Bits16, 0).unwrap();
    assert!(inst.operands.is_empty());

    let inst = decode(&[0xCF], DecodeMode::Bits16, 0).unwrap();
    assert!(inst.operands.is_empty());

    let inst = decode(&[0xF1], DecodeMode::Bits16, 0).unwrap();
    assert!(inst.operands.is_empty());

    // Far call/jmp are used during boot and mode transitions; the decoder must at least recover
    // length even if the upstream decoder rejects the opcode.
    let inst = decode(&[0x9A, 0, 0, 0, 0, 0, 0], DecodeMode::Bits16, 0).unwrap();
    assert_eq!(inst.length, 5);
    let inst = decode(&[0xEA, 0, 0, 0, 0, 0, 0], DecodeMode::Bits16, 0).unwrap();
    assert_eq!(inst.length, 5);

    // 0x82 is an obsolete alias of Group1; it is accepted in 16/32-bit modes but is invalid in
    // 64-bit mode.
    assert!(decode(&[0x82, 0x07, 0x00], DecodeMode::Bits16, 0).is_ok());
    assert!(matches!(
        decode(&[0x82, 0xC0, 0x00], DecodeMode::Bits64, 0),
        Err(DecodeError::Invalid)
    ));

    // MOV r/m8, imm8 requires ModRM.reg == 0.
    assert!(matches!(
        decode(&[0xC6, 0xE0, 0x00], DecodeMode::Bits16, 0),
        Err(DecodeError::Invalid)
    ));

    // Operand-size override changes the displacement width of CALL/JMP rel16/rel32 in 16-bit mode.
    assert!(decode(&[0x66, 0xE8, 0x00, 0x00], DecodeMode::Bits16, 0).is_err());

    // REP prefixes are accepted (and ignored) on near CALL/JMP encodings.
    let inst = decode(&[0xF2, 0xE8, 0x00, 0x00, 0x00, 0x00], DecodeMode::Bits16, 0).unwrap();
    assert_eq!(inst.length, 4);
    assert!(inst
        .operands
        .iter()
        .any(|op| matches!(op, Operand::Relative { target: 4, .. })));
}

#[test]
fn reserved_nop_0f18_models_rm_and_reg_operands() {
    // iced-x86 exposes `0F 18 /r` as a "reserved NOP" with two operands (`r/m`, `r`).
    for bytes in [[0x0F, 0x18, 0x60, 0x00], [0x0F, 0x19, 0x07, 0x00]] {
        let inst = decode(&bytes, DecodeMode::Bits16, 0).unwrap();
        assert_eq!(inst.operands.len(), 2);
        assert!(inst
            .operands
            .iter()
            .any(|op| matches!(op, Operand::Memory(_))));
        assert!(inst
            .operands
            .iter()
            .any(|op| matches!(op, Operand::Gpr { .. })));
    }
}
