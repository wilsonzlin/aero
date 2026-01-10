pub use crate::inst::DecodeMode;
use crate::inst::{
    AddressSize, DecodedInst, Immediate, InstFlags, OpcodeBytes, OpcodeMap, Operand, OperandSize,
    Prefixes, RepPrefix, RexPrefix, SegmentReg,
};
use crate::opcode_tables;

/// Maximum x86 instruction length (architectural limit).
pub const MAX_INST_LEN: usize = 15;

/// Decoder error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The byte stream ended before the instruction could be fully decoded.
    UnexpectedEof,
    /// The decoded instruction exceeds the architectural 15-byte length limit.
    TooLong,
    /// The instruction is invalid/undefined for the requested mode.
    Invalid,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "unexpected end of instruction bytes"),
            Self::TooLong => write!(f, "instruction exceeds 15-byte length limit"),
            Self::Invalid => write!(f, "invalid instruction"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode a single instruction from the start of `bytes`.
///
/// `ip` is used to compute RIP-relative memory operands and relative branch targets.
pub fn decode(bytes: &[u8], mode: DecodeMode, ip: u64) -> Result<DecodedInst, DecodeError> {
    let (prefixes, prefix_len) = scan_prefixes(bytes, mode)?;

    let operand_size = effective_operand_size(mode, prefixes);
    let address_size = effective_address_size(mode, prefixes);

    let (mut opcode, opcode_len) = parse_opcode(bytes, prefix_len)?;

    // For group opcodes, include ModRM.reg in `opcode_ext` where it matters.
    if opcode_uses_modrm_reg(opcode.map, opcode.opcode) {
        let modrm_off = prefix_len + opcode_len;
        let modrm = *bytes.get(modrm_off).ok_or(DecodeError::UnexpectedEof)?;
        opcode.opcode_ext = Some((modrm >> 3) & 0x7);
    }

    // Validate opcodes whose ModRM.reg must be a fixed extension.
    if opcode.map == OpcodeMap::Primary
        && matches!(opcode.opcode, 0xC6 | 0xC7)
        && opcode.opcode_ext != Some(0)
    {
        return Err(DecodeError::Invalid);
    }

    // Some relative branch/call opcodes have operand-size-dependent immediate widths, and not all
    // upstream decoders agree on how to interpret `0x66` in 16-bit mode. Decode these cases
    // ourselves so we match iced-x86 for block formation.
    if let Some((operands, inst_len)) = decode_relative_immediate(
        bytes,
        mode,
        ip,
        prefixes,
        prefix_len,
        opcode_len,
        opcode,
        operand_size,
    )? {
        if inst_len > MAX_INST_LEN {
            return Err(DecodeError::TooLong);
        }

        let mut operands = operands;
        fixup_implicit_operands(
            opcode,
            mode,
            prefixes,
            operand_size,
            address_size,
            &mut operands,
        );
        let flags = classify_inst(opcode, &operands);

        return Ok(DecodedInst {
            length: inst_len as u8,
            opcode,
            prefixes,
            operand_size,
            address_size,
            operands,
            flags,
        });
    }

    let (mut operands, inst_len) = match decode_with_yaxpeax(
        bytes,
        mode,
        ip,
        prefixes,
        operand_size,
        address_size,
        opcode,
    ) {
        Ok(v) => v,
        Err(DecodeError::Invalid) if mode == DecodeMode::Bits64 => {
            let prefix_bytes = bytes.get(..prefix_len).unwrap_or(&[]);
            let rex_count = prefix_bytes
                .iter()
                .filter(|b| (0x40..=0x4F).contains(*b))
                .count();
            let has_ignored_seg_prefix = prefix_bytes
                .iter()
                .any(|b| matches!(b, 0x26 | 0x2E | 0x36 | 0x3E));

            if rex_count > 1 || has_ignored_seg_prefix {
                // In 64-bit mode, CS/DS/ES/SS segment override prefixes are accepted but ignored.
                // Multiple REX prefixes are also accepted, with the last one taking effect.
                //
                // Some upstream decoders reject these encodings, so retry decoding with:
                // - ignorable segment prefixes removed
                // - all but the last REX prefix removed
                //
                // while still reporting the original instruction length.
                let last_rex_pos = prefix_bytes.iter().rposition(|b| (0x40..=0x4F).contains(b));

                let n = bytes.len().min(MAX_INST_LEN);
                let mut patched = [0u8; MAX_INST_LEN];
                let mut out_len = 0usize;
                let mut removed = 0usize;
                for (i, b) in bytes[..n].iter().copied().enumerate() {
                    if i < prefix_len {
                        if matches!(b, 0x26 | 0x2E | 0x36 | 0x3E) {
                            removed += 1;
                            continue;
                        }
                        if (0x40..=0x4F).contains(&b) && last_rex_pos != Some(i) {
                            removed += 1;
                            continue;
                        }
                    }
                    patched[out_len] = b;
                    out_len += 1;
                }

                let (ops, len) = decode_with_yaxpeax(
                    &patched[..out_len],
                    mode,
                    ip.wrapping_add(removed as u64),
                    prefixes,
                    operand_size,
                    address_size,
                    opcode,
                )?;
                (ops, len + removed)
            } else {
                return Err(DecodeError::Invalid);
            }
        }
        Err(DecodeError::Invalid)
            if matches!(mode, DecodeMode::Bits16 | DecodeMode::Bits32)
                && opcode.map == OpcodeMap::Primary
                && matches!(opcode.opcode, 0x9A | 0xEA) =>
        {
            // Far call/jmp (`CALLF/JMPF ptr16:16/32`) is used during boot and mode transitions.
            // Some third-party decoders treat it as invalid in certain configurations; if the
            // upstream decoder rejects it, decode its length manually so the interpreter/JIT can
            // keep going.
            let ptr_len = match operand_size {
                OperandSize::Bits16 => 4,
                OperandSize::Bits32 => 6,
                _ => return Err(DecodeError::Invalid),
            };
            let len = prefix_len + opcode_len + ptr_len;
            if len > MAX_INST_LEN {
                return Err(DecodeError::TooLong);
            }
            if bytes.len() < len {
                return Err(DecodeError::UnexpectedEof);
            }
            (Vec::new(), len)
        }
        Err(DecodeError::Invalid)
            if mode != DecodeMode::Bits64
                && opcode.map == OpcodeMap::Primary
                && opcode.opcode == 0x82 =>
        {
            // 0x82 is an obsolete/undocumented alias of Group 1 (`0x80 /r ib`) that is still
            // accepted by some decoders (including iced-x86). Fall back to decoding it as 0x80.
            let mut patched = [0u8; MAX_INST_LEN];
            let n = bytes.len().min(MAX_INST_LEN);
            patched[..n].copy_from_slice(&bytes[..n]);
            if prefix_len < n {
                patched[prefix_len] = 0x80;
            }
            decode_with_yaxpeax(
                &patched[..n],
                mode,
                ip,
                prefixes,
                operand_size,
                address_size,
                opcode,
            )?
        }
        Err(e) => return Err(e),
    };

    if inst_len > MAX_INST_LEN {
        return Err(DecodeError::TooLong);
    }

    // If prefix scanning already consumed 15 bytes, we don't have space for opcode.
    if prefix_len >= MAX_INST_LEN {
        return Err(DecodeError::TooLong);
    }

    fixup_implicit_operands(
        opcode,
        mode,
        prefixes,
        operand_size,
        address_size,
        &mut operands,
    );

    // Control-flow classification for block formation.
    let flags = classify_inst(opcode, &operands);

    Ok(DecodedInst {
        length: inst_len as u8,
        opcode,
        prefixes,
        operand_size,
        address_size,
        operands,
        flags,
    })
}

fn fixup_implicit_operands(
    opcode: OpcodeBytes,
    mode: DecodeMode,
    prefixes: Prefixes,
    operand_size: OperandSize,
    address_size: AddressSize,
    operands: &mut Vec<Operand>,
) {
    // Some instructions have implicit operands that `yaxpeax-x86` does not surface as explicit
    // operands. Since our downstream users (and iced-x86 differential tests) expect these to be
    // present, we inject the most important ones here.
    //
    // This list is intentionally small and grows on demand as new mismatches are discovered.
    match (opcode.map, opcode.opcode, opcode.opcode_ext) {
        (OpcodeMap::Primary, 0xD7, _) if operands.is_empty() => {
            // XLAT: memory operand at [BX/EBX/RBX + AL] (default segment = DS, overridable).
            //
            // We can't precisely model "AL-indexed" addressing with the current `MemoryOperand`
            // structure; representing it as [BX + RAX] is still sufficient for operand-kind
            // classification and basic-block formation.
            let _ = mode;
            operands.push(Operand::Memory(crate::inst::MemoryOperand {
                segment: prefixes.segment,
                addr_size: address_size,
                base: Some(crate::inst::Gpr { index: 3 }), // BX/EBX/RBX
                index: Some(crate::inst::Gpr { index: 0 }), // (approx) AL
                scale: 1,
                disp: 0,
                rip_relative: false,
            }));
        }
        (OpcodeMap::Primary, 0xCC, _) => {
            // INT3 is a dedicated 1-byte encoding and has no explicit operands.
            operands.clear();
        }
        (OpcodeMap::Primary, 0xCF, _) => {
            // IRET has no explicit operands.
            operands.clear();
        }
        (OpcodeMap::Primary, 0xF1, _) => {
            // INT1 / ICEBP has no explicit operands.
            operands.clear();
        }
        (OpcodeMap::Map0F, 0x18..=0x1F, Some(reg))
            if operands.len() == 2
                && operands.get(0) == operands.get(1)
                && matches!(operands.get(0), Some(Operand::Memory(_))) =>
        {
            // Some decoders expose the `0F 18..1F /r` "reserved NOP" encodings as `r/m, r` where the
            // ModRM.reg field selects a register operand. `yaxpeax-x86` currently models these as
            // `mem, mem`; rewrite the second operand to the expected register form.
            let mut reg_index = reg;
            if mode == DecodeMode::Bits64 && prefixes.rex.map_or(false, |r| r.r) {
                reg_index |= 0b1000;
            }
            operands[1] = Operand::Gpr {
                reg: crate::inst::Gpr { index: reg_index },
                size: operand_size,
                high8: false,
            };
        }
        // Some unary Group opcodes are modelled by yaxpeax as `dst, dst`. Collapse them.
        (OpcodeMap::Primary, 0xF6 | 0xF7, Some(2..=7))
        | (OpcodeMap::Primary, 0xFE, Some(0 | 1))
        | (OpcodeMap::Primary, 0xFF, Some(0 | 1))
            if operands.len() == 2 && operands.get(0) == operands.get(1) =>
        {
            operands.pop();
        }
        _ => {}
    }
}

fn scan_prefixes(bytes: &[u8], mode: DecodeMode) -> Result<(Prefixes, usize), DecodeError> {
    let mut idx = 0usize;
    let mut prefixes = Prefixes::default();

    // Prefixes are scanned in a single pass. In 64-bit mode, REX prefixes participate in the same
    // prefix stream and can appear interleaved with legacy prefixes. The last prefix in each group
    // wins.
    while idx < bytes.len() && idx < MAX_INST_LEN {
        let b = bytes[idx];

        // REX prefixes (64-bit mode only).
        if mode == DecodeMode::Bits64 && (0x40..=0x4F).contains(&b) {
            prefixes.rex = Some(RexPrefix {
                w: (b & 0b1000) != 0,
                r: (b & 0b0100) != 0,
                x: (b & 0b0010) != 0,
                b: (b & 0b0001) != 0,
            });
            idx += 1;
            continue;
        }

        if let Some(seg) = opcode_tables::segment_override(b) {
            prefixes.segment = Some(seg);
            idx += 1;
            continue;
        }

        match b {
            0xF0 => {
                // LOCK/REP share the same legacy prefix group; last one wins.
                prefixes.lock = true;
                prefixes.rep = None;
                idx += 1;
                continue;
            }
            0xF2 => {
                prefixes.rep = Some(RepPrefix::Repne);
                prefixes.lock = false;
                idx += 1;
                continue;
            }
            0xF3 => {
                prefixes.rep = Some(RepPrefix::Rep);
                prefixes.lock = false;
                idx += 1;
                continue;
            }
            0x66 => {
                prefixes.operand_size_override = true;
                idx += 1;
                continue;
            }
            0x67 => {
                prefixes.address_size_override = true;
                idx += 1;
                continue;
            }
            _ => {}
        }

        break;
    }

    if idx >= MAX_INST_LEN {
        // Consumed 15 bytes worth of prefixes; opcode can't fit.
        return Err(DecodeError::TooLong);
    }

    Ok((prefixes, idx))
}

fn effective_operand_size(mode: DecodeMode, prefixes: Prefixes) -> OperandSize {
    match mode {
        DecodeMode::Bits16 => {
            if prefixes.operand_size_override {
                OperandSize::Bits32
            } else {
                OperandSize::Bits16
            }
        }
        DecodeMode::Bits32 => {
            if prefixes.operand_size_override {
                OperandSize::Bits16
            } else {
                OperandSize::Bits32
            }
        }
        DecodeMode::Bits64 => {
            if prefixes.rex.map_or(false, |r| r.w) {
                OperandSize::Bits64
            } else if prefixes.operand_size_override {
                OperandSize::Bits16
            } else {
                OperandSize::Bits32
            }
        }
    }
}

fn effective_address_size(mode: DecodeMode, prefixes: Prefixes) -> AddressSize {
    match mode {
        DecodeMode::Bits16 => {
            if prefixes.address_size_override {
                AddressSize::Bits32
            } else {
                AddressSize::Bits16
            }
        }
        DecodeMode::Bits32 => {
            if prefixes.address_size_override {
                AddressSize::Bits16
            } else {
                AddressSize::Bits32
            }
        }
        DecodeMode::Bits64 => {
            if prefixes.address_size_override {
                AddressSize::Bits32
            } else {
                AddressSize::Bits64
            }
        }
    }
}

fn parse_opcode(bytes: &[u8], off: usize) -> Result<(OpcodeBytes, usize), DecodeError> {
    let b0 = *bytes.get(off).ok_or(DecodeError::UnexpectedEof)?;
    if b0 == 0x0F {
        let b1 = *bytes.get(off + 1).ok_or(DecodeError::UnexpectedEof)?;
        if b1 == 0x38 {
            let b2 = *bytes.get(off + 2).ok_or(DecodeError::UnexpectedEof)?;
            Ok((
                OpcodeBytes {
                    map: OpcodeMap::Map0F38,
                    opcode: b2,
                    opcode_ext: None,
                },
                3,
            ))
        } else if b1 == 0x3A {
            let b2 = *bytes.get(off + 2).ok_or(DecodeError::UnexpectedEof)?;
            Ok((
                OpcodeBytes {
                    map: OpcodeMap::Map0F3A,
                    opcode: b2,
                    opcode_ext: None,
                },
                3,
            ))
        } else {
            Ok((
                OpcodeBytes {
                    map: OpcodeMap::Map0F,
                    opcode: b1,
                    opcode_ext: None,
                },
                2,
            ))
        }
    } else if matches!(b0, 0xC4 | 0xC5 | 0x62) {
        // VEX2/VEX3/EVEX start bytes. Full decode is delegated to yaxpeax, but we flag these as
        // "extended" so downstream users don't accidentally treat them as legacy one-byte opcodes.
        Ok((
            OpcodeBytes {
                map: OpcodeMap::Extended,
                opcode: b0,
                opcode_ext: None,
            },
            1,
        ))
    } else {
        Ok((
            OpcodeBytes {
                map: OpcodeMap::Primary,
                opcode: b0,
                opcode_ext: None,
            },
            1,
        ))
    }
}

fn decode_relative_immediate(
    bytes: &[u8],
    mode: DecodeMode,
    ip: u64,
    prefixes: Prefixes,
    prefix_len: usize,
    opcode_len: usize,
    opcode: OpcodeBytes,
    operand_size: OperandSize,
) -> Result<Option<(Vec<Operand>, usize)>, DecodeError> {
    let (imm_len, rel_size) = match (opcode.map, opcode.opcode) {
        (OpcodeMap::Primary, 0xE8 | 0xE9) => {
            if mode == DecodeMode::Bits64 {
                (4usize, OperandSize::Bits32)
            } else if operand_size == OperandSize::Bits16 {
                (2usize, OperandSize::Bits16)
            } else {
                (4usize, OperandSize::Bits32)
            }
        }
        (OpcodeMap::Map0F, 0x80..=0x8F) => {
            if mode == DecodeMode::Bits64 {
                (4usize, OperandSize::Bits32)
            } else if operand_size == OperandSize::Bits16 {
                (2usize, OperandSize::Bits16)
            } else {
                (4usize, OperandSize::Bits32)
            }
        }
        _ => return Ok(None),
    };

    // LOCK is not valid on near branches/calls.
    //
    // Note: REP/REPNZ prefixes are accepted (and ignored) by iced-x86 for these
    // opcodes, so we do not treat them as invalid here.
    if prefixes.lock {
        return Err(DecodeError::Invalid);
    }

    let imm_off = prefix_len + opcode_len;
    let inst_len = imm_off + imm_len;
    if inst_len > MAX_INST_LEN {
        return Err(DecodeError::TooLong);
    }
    if bytes.len() < inst_len {
        return Err(DecodeError::UnexpectedEof);
    }

    let rel = match imm_len {
        1 => i8::from_le_bytes([bytes[imm_off]]) as i64,
        2 => i16::from_le_bytes([bytes[imm_off], bytes[imm_off + 1]]) as i64,
        4 => i32::from_le_bytes([
            bytes[imm_off],
            bytes[imm_off + 1],
            bytes[imm_off + 2],
            bytes[imm_off + 3],
        ]) as i64,
        _ => return Err(DecodeError::Invalid),
    };

    let next_ip = ip.wrapping_add(inst_len as u64);
    let ip_mask = if mode == DecodeMode::Bits64 {
        u64::MAX
    } else if operand_size == OperandSize::Bits16 {
        0xFFFF
    } else {
        0xFFFF_FFFF
    };

    let target = next_ip.wrapping_add(rel as u64) & ip_mask;
    Ok(Some((
        vec![Operand::Relative {
            target,
            size: rel_size,
        }],
        inst_len,
    )))
}

fn opcode_uses_modrm_reg(map: OpcodeMap, opcode: u8) -> bool {
    match map {
        OpcodeMap::Primary => matches!(
            opcode,
            0x80 | 0x81
                | 0x82
                | 0x83
                | 0xC0
                | 0xC1
                | 0xC6
                | 0xC7
                | 0xD0
                | 0xD1
                | 0xD2
                | 0xD3
                | 0xF6
                | 0xF7
                | 0xFE
                | 0xFF
        ),
        OpcodeMap::Map0F => matches!(opcode, 0x00 | 0x01 | 0x18..=0x1F | 0xBA | 0xC7),
        _ => false,
    }
}

fn classify_inst(opcode: OpcodeBytes, operands: &[Operand]) -> InstFlags {
    // We keep this intentionally simple and based on opcode bytes, not mnemonic.
    let mut flags = InstFlags::default();

    match (opcode.map, opcode.opcode) {
        (OpcodeMap::Primary, 0xE8) => flags.is_call = true,
        (OpcodeMap::Primary, 0x9A) => flags.is_call = true, // CALLF ptr16:16/32
        (OpcodeMap::Primary, 0xFF) => {
            if matches!(opcode.opcode_ext, Some(2 | 3)) {
                flags.is_call = true;
            } else if matches!(opcode.opcode_ext, Some(4 | 5)) {
                flags.is_branch = true;
            }
        }
        (OpcodeMap::Primary, 0xE9 | 0xEB | 0xEA) => flags.is_branch = true,
        (OpcodeMap::Primary, 0x70..=0x7F) => flags.is_branch = true,
        (OpcodeMap::Map0F, 0x80..=0x8F) => flags.is_branch = true,
        (OpcodeMap::Primary, 0xC2 | 0xC3 | 0xCA | 0xCB) => flags.is_ret = true,
        _ => {}
    }

    flags.is_branch |= operands
        .iter()
        .any(|op| matches!(op, Operand::Relative { .. }));
    flags
}

fn decode_with_yaxpeax(
    bytes: &[u8],
    mode: DecodeMode,
    ip: u64,
    prefixes: Prefixes,
    default_op_size: OperandSize,
    address_size: AddressSize,
    opcode: OpcodeBytes,
) -> Result<(Vec<Operand>, usize), DecodeError> {
    use yaxpeax_arch::{Decoder, U8Reader};

    // Restrict to 15 bytes to avoid accepting overlong instructions in case the underlying decoder
    // is more permissive than the architectural limit.
    let bytes = if bytes.len() > MAX_INST_LEN {
        &bytes[..MAX_INST_LEN]
    } else {
        bytes
    };

    match mode {
        DecodeMode::Bits16 => {
            let decoder = yaxpeax_x86::real_mode::InstDecoder::default();
            let mut reader = U8Reader::new(bytes);
            let inst = decoder
                .decode(&mut reader)
                .map_err(|_| DecodeError::Invalid)?;
            let len =
                <U8Reader<'_> as yaxpeax_arch::Reader<u64, u8>>::total_offset(&mut reader) as usize;
            let next_ip = ip.wrapping_add(len as u64);
            let ops = convert_operands_real(
                &inst,
                opcode,
                next_ip,
                prefixes,
                default_op_size,
                address_size,
            )?;
            Ok((ops, len))
        }
        DecodeMode::Bits32 => {
            let decoder = yaxpeax_x86::protected_mode::InstDecoder::default();
            let mut reader = U8Reader::new(bytes);
            let inst = decoder
                .decode(&mut reader)
                .map_err(|_| DecodeError::Invalid)?;
            let len =
                <U8Reader<'_> as yaxpeax_arch::Reader<u64, u8>>::total_offset(&mut reader) as usize;
            let next_ip = ip.wrapping_add(len as u64);
            let ops = convert_operands_protected(
                &inst,
                opcode,
                next_ip,
                prefixes,
                default_op_size,
                address_size,
            )?;
            Ok((ops, len))
        }
        DecodeMode::Bits64 => {
            let decoder = yaxpeax_x86::long_mode::InstDecoder::default();
            let mut reader = U8Reader::new(bytes);
            let inst = decoder
                .decode(&mut reader)
                .map_err(|_| DecodeError::Invalid)?;
            let len =
                <U8Reader<'_> as yaxpeax_arch::Reader<u64, u8>>::total_offset(&mut reader) as usize;
            let next_ip = ip.wrapping_add(len as u64);
            let ops = convert_operands_long(
                &inst,
                opcode,
                next_ip,
                prefixes,
                default_op_size,
                address_size,
            )?;
            Ok((ops, len))
        }
    }
}

fn convert_operands_real(
    inst: &yaxpeax_x86::real_mode::Instruction,
    opcode: OpcodeBytes,
    next_ip: u64,
    prefixes: Prefixes,
    default_op_size: OperandSize,
    address_size: AddressSize,
) -> Result<Vec<Operand>, DecodeError> {
    use yaxpeax_x86::real_mode::Operand as O;

    let ip_mask = match default_op_size {
        OperandSize::Bits16 => 0xFFFF,
        OperandSize::Bits32 => 0xFFFF_FFFF,
        _ => 0xFFFF,
    };

    let mut out = Vec::new();
    for i in 0..4u8 {
        match inst.operand(i) {
            O::Nothing => {}
            O::Register(reg) => {
                if let Some(op) = map_reg_real(reg, prefixes) {
                    out.push(op);
                }
            }
            O::ImmediateI8(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits8,
                ip_mask,
            )),
            O::ImmediateU8(v) => push_imm(&mut out, v as u64, OperandSize::Bits8, false),
            O::ImmediateI16(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits16,
                ip_mask,
            )),
            O::ImmediateU16(v) => push_imm(&mut out, v as u64, OperandSize::Bits16, false),
            O::ImmediateI32(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits32,
                ip_mask,
            )),
            O::ImmediateU32(v) => push_imm(&mut out, v as u64, OperandSize::Bits32, false),
            O::DisplacementU16(d) => {
                out.push(Operand::Memory(mem_disp(prefixes, address_size, d as i64)))
            }
            O::DisplacementU32(d) => {
                out.push(Operand::Memory(mem_disp(prefixes, address_size, d as i64)))
            }
            O::RegDeref(base) => {
                out.push(Operand::Memory(mem_base(prefixes, address_size, base, 0)))
            }
            O::RegDisp(base, disp) => out.push(Operand::Memory(mem_reg_disp(
                prefixes,
                address_size,
                base,
                disp as i64,
            ))),
            O::RegIndexBaseScale(base, index, scale) => out.push(Operand::Memory(mem_base_index(
                prefixes,
                address_size,
                base,
                Some(index),
                scale,
                0,
            ))),
            O::RegIndexBaseScaleDisp(base, index, scale, disp) => {
                out.push(Operand::Memory(mem_base_index(
                    prefixes,
                    address_size,
                    base,
                    Some(index),
                    scale,
                    disp as i64,
                )))
            }
            O::RegScaleDisp(index, scale, disp) => out.push(Operand::Memory(mem_index_only(
                prefixes,
                address_size,
                index,
                scale,
                disp as i64,
            ))),
            O::RegScale(index, scale) => out.push(Operand::Memory(mem_index_only(
                prefixes,
                address_size,
                index,
                scale,
                0,
            ))),
            _ => {}
        }
    }

    Ok(out)
}

fn convert_operands_protected(
    inst: &yaxpeax_x86::protected_mode::Instruction,
    opcode: OpcodeBytes,
    next_ip: u64,
    prefixes: Prefixes,
    default_op_size: OperandSize,
    address_size: AddressSize,
) -> Result<Vec<Operand>, DecodeError> {
    use yaxpeax_x86::protected_mode::Operand as O;

    let ip_mask = match default_op_size {
        OperandSize::Bits16 => 0xFFFF,
        OperandSize::Bits32 => 0xFFFF_FFFF,
        _ => 0xFFFF_FFFF,
    };

    let mut out = Vec::new();
    for i in 0..4u8 {
        match inst.operand(i) {
            O::Nothing => {}
            O::Register(reg) => {
                if let Some(op) = map_reg_protected(reg, prefixes) {
                    out.push(op);
                }
            }
            O::ImmediateI8(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits8,
                ip_mask,
            )),
            O::ImmediateU8(v) => push_imm(&mut out, v as u64, OperandSize::Bits8, false),
            O::ImmediateI16(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits16,
                ip_mask,
            )),
            O::ImmediateU16(v) => push_imm(&mut out, v as u64, OperandSize::Bits16, false),
            O::ImmediateI32(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits32,
                ip_mask,
            )),
            O::ImmediateU32(v) => push_imm(&mut out, v as u64, OperandSize::Bits32, false),
            O::DisplacementU16(d) => {
                out.push(Operand::Memory(mem_disp(prefixes, address_size, d as i64)))
            }
            O::DisplacementU32(d) => {
                out.push(Operand::Memory(mem_disp(prefixes, address_size, d as i64)))
            }
            O::RegDeref(base) => {
                out.push(Operand::Memory(mem_base(prefixes, address_size, base, 0)))
            }
            O::RegDisp(base, disp) => out.push(Operand::Memory(mem_reg_disp(
                prefixes,
                address_size,
                base,
                disp as i64,
            ))),
            O::RegIndexBaseScale(base, index, scale) => out.push(Operand::Memory(mem_base_index(
                prefixes,
                address_size,
                base,
                Some(index),
                scale,
                0,
            ))),
            O::RegIndexBaseScaleDisp(base, index, scale, disp) => {
                out.push(Operand::Memory(mem_base_index(
                    prefixes,
                    address_size,
                    base,
                    Some(index),
                    scale,
                    disp as i64,
                )))
            }
            O::RegScaleDisp(index, scale, disp) => out.push(Operand::Memory(mem_index_only(
                prefixes,
                address_size,
                index,
                scale,
                disp as i64,
            ))),
            O::RegScale(index, scale) => out.push(Operand::Memory(mem_index_only(
                prefixes,
                address_size,
                index,
                scale,
                0,
            ))),
            _ => {}
        }
    }

    Ok(out)
}

fn convert_operands_long(
    inst: &yaxpeax_x86::long_mode::Instruction,
    opcode: OpcodeBytes,
    next_ip: u64,
    prefixes: Prefixes,
    _default_op_size: OperandSize,
    address_size: AddressSize,
) -> Result<Vec<Operand>, DecodeError> {
    use yaxpeax_x86::long_mode::Operand as O;

    let mut out = Vec::new();
    for i in 0..4u8 {
        match inst.operand(i) {
            O::Nothing => {}
            O::Register(reg) => {
                if let Some(op) = map_reg_long(reg, prefixes) {
                    out.push(op);
                }
            }
            O::ImmediateI8(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits8,
                u64::MAX,
            )),
            O::ImmediateU8(v) => push_imm(&mut out, v as u64, OperandSize::Bits8, false),
            O::ImmediateI16(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits16,
                u64::MAX,
            )),
            O::ImmediateU16(v) => push_imm(&mut out, v as u64, OperandSize::Bits16, false),
            O::ImmediateI32(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v as i64,
                OperandSize::Bits32,
                u64::MAX,
            )),
            O::ImmediateU32(v) => push_imm(&mut out, v as u64, OperandSize::Bits32, false),
            O::ImmediateI64(v) => out.push(maybe_rel_imm(
                opcode,
                next_ip,
                v,
                OperandSize::Bits64,
                u64::MAX,
            )),
            O::ImmediateU64(v) => push_imm(&mut out, v, OperandSize::Bits64, false),
            O::DisplacementU32(d) => {
                out.push(Operand::Memory(mem_disp(prefixes, address_size, d as i64)))
            }
            O::DisplacementU64(d) => {
                out.push(Operand::Memory(mem_disp(prefixes, address_size, d as i64)))
            }
            O::RegDeref(base) => {
                out.push(Operand::Memory(mem_base(prefixes, address_size, base, 0)))
            }
            O::RegDisp(base, disp) => out.push(Operand::Memory(mem_reg_disp(
                prefixes,
                address_size,
                base,
                disp as i64,
            ))),
            O::RegIndexBaseScale(base, index, scale) => out.push(Operand::Memory(mem_base_index(
                prefixes,
                address_size,
                base,
                Some(index),
                scale,
                0,
            ))),
            O::RegIndexBaseScaleDisp(base, index, scale, disp) => {
                out.push(Operand::Memory(mem_base_index(
                    prefixes,
                    address_size,
                    base,
                    Some(index),
                    scale,
                    disp as i64,
                )))
            }
            O::RegScaleDisp(index, scale, disp) => out.push(Operand::Memory(mem_index_only(
                prefixes,
                address_size,
                index,
                scale,
                disp as i64,
            ))),
            O::RegScale(index, scale) => out.push(Operand::Memory(mem_index_only(
                prefixes,
                address_size,
                index,
                scale,
                0,
            ))),
            _ => {}
        }
    }

    Ok(out)
}

fn push_imm(out: &mut Vec<Operand>, value: u64, size: OperandSize, is_signed: bool) {
    out.push(Operand::Immediate(Immediate {
        value,
        size,
        is_signed,
    }));
}

fn maybe_rel_imm(
    opcode: OpcodeBytes,
    next_ip: u64,
    imm: i64,
    size: OperandSize,
    ip_mask: u64,
) -> Operand {
    if is_rel_branch_or_call(opcode) {
        let target = next_ip.wrapping_add(imm as u64) & ip_mask;
        Operand::Relative { target, size }
    } else {
        Operand::Immediate(Immediate {
            value: imm as u64,
            size,
            is_signed: true,
        })
    }
}

fn is_rel_branch_or_call(opcode: OpcodeBytes) -> bool {
    match (opcode.map, opcode.opcode) {
        (OpcodeMap::Primary, 0xE8 | 0xE9 | 0xEB) => true,
        (OpcodeMap::Primary, 0x70..=0x7F) => true,
        (OpcodeMap::Primary, 0xE0..=0xE3) => true, // LOOP/LOOPZ/LOOPNZ/JCXZ
        (OpcodeMap::Map0F, 0x80..=0x8F) => true,
        _ => false,
    }
}

trait RegSpecLike: Copy {
    type Class: core::fmt::Debug;

    fn num(self) -> u8;
    fn class(self) -> Self::Class;
}

impl RegSpecLike for yaxpeax_x86::real_mode::RegSpec {
    type Class = yaxpeax_x86::real_mode::RegisterClass;

    fn num(self) -> u8 {
        yaxpeax_x86::real_mode::RegSpec::num(&self)
    }

    fn class(self) -> Self::Class {
        yaxpeax_x86::real_mode::RegSpec::class(&self)
    }
}

impl RegSpecLike for yaxpeax_x86::protected_mode::RegSpec {
    type Class = yaxpeax_x86::protected_mode::RegisterClass;

    fn num(self) -> u8 {
        yaxpeax_x86::protected_mode::RegSpec::num(&self)
    }

    fn class(self) -> Self::Class {
        yaxpeax_x86::protected_mode::RegSpec::class(&self)
    }
}

impl RegSpecLike for yaxpeax_x86::long_mode::RegSpec {
    type Class = yaxpeax_x86::long_mode::RegisterClass;

    fn num(self) -> u8 {
        yaxpeax_x86::long_mode::RegSpec::num(&self)
    }

    fn class(self) -> Self::Class {
        yaxpeax_x86::long_mode::RegSpec::class(&self)
    }
}

fn is_rip_reg<R: RegSpecLike>(reg: R) -> bool {
    let dbg = format!("{:?}", reg.class());
    let kind = dbg
        .split("kind: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or(dbg.as_str());
    kind == "RIP"
}

fn gpr_from_regspec<R: RegSpecLike>(reg: R) -> Option<crate::inst::Gpr> {
    if is_rip_reg(reg) {
        None
    } else {
        Some(crate::inst::Gpr { index: reg.num() })
    }
}

fn mem_disp(prefixes: Prefixes, addr_size: AddressSize, disp: i64) -> crate::inst::MemoryOperand {
    crate::inst::MemoryOperand {
        segment: prefixes.segment,
        addr_size: addr_size,
        base: None,
        index: None,
        scale: 1,
        disp,
        rip_relative: false,
    }
}

fn mem_base<R: RegSpecLike>(
    prefixes: Prefixes,
    addr_size: AddressSize,
    base: R,
    disp: i64,
) -> crate::inst::MemoryOperand {
    crate::inst::MemoryOperand {
        segment: prefixes.segment,
        addr_size: addr_size,
        base: gpr_from_regspec(base),
        index: None,
        scale: 1,
        disp,
        rip_relative: false,
    }
}

fn mem_reg_disp<R: RegSpecLike>(
    prefixes: Prefixes,
    addr_size: AddressSize,
    base: R,
    disp: i64,
) -> crate::inst::MemoryOperand {
    if is_rip_reg(base) {
        crate::inst::MemoryOperand {
            segment: prefixes.segment,
            addr_size: addr_size,
            base: None,
            index: None,
            scale: 1,
            disp,
            rip_relative: true,
        }
    } else {
        mem_base(prefixes, addr_size, base, disp)
    }
}

fn mem_base_index<R: RegSpecLike>(
    prefixes: Prefixes,
    addr_size: AddressSize,
    base: R,
    index: Option<R>,
    scale: u8,
    disp: i64,
) -> crate::inst::MemoryOperand {
    if is_rip_reg(base) {
        // There is no RIP+index form in the legacy SIB encoding; keep base/index empty and mark
        // RIP-relative.
        crate::inst::MemoryOperand {
            segment: prefixes.segment,
            addr_size: addr_size,
            base: None,
            index: None,
            scale: 1,
            disp,
            rip_relative: true,
        }
    } else {
        crate::inst::MemoryOperand {
            segment: prefixes.segment,
            addr_size: addr_size,
            base: gpr_from_regspec(base),
            index: index.and_then(gpr_from_regspec),
            scale,
            disp,
            rip_relative: false,
        }
    }
}

fn mem_index_only<R: RegSpecLike>(
    prefixes: Prefixes,
    addr_size: AddressSize,
    index: R,
    scale: u8,
    disp: i64,
) -> crate::inst::MemoryOperand {
    crate::inst::MemoryOperand {
        segment: prefixes.segment,
        addr_size: addr_size,
        base: None,
        index: gpr_from_regspec(index),
        scale,
        disp,
        rip_relative: false,
    }
}

fn map_reg_real(reg: yaxpeax_x86::real_mode::RegSpec, prefixes: Prefixes) -> Option<Operand> {
    map_reg_common(reg.class(), reg.num(), prefixes)
}

fn map_reg_protected(
    reg: yaxpeax_x86::protected_mode::RegSpec,
    prefixes: Prefixes,
) -> Option<Operand> {
    map_reg_common(reg.class(), reg.num(), prefixes)
}

fn map_reg_long(reg: yaxpeax_x86::long_mode::RegSpec, prefixes: Prefixes) -> Option<Operand> {
    map_reg_common(reg.class(), reg.num(), prefixes)
}

fn map_reg_common<C: core::fmt::Debug>(class: C, idx: u8, prefixes: Prefixes) -> Option<Operand> {
    // We don't rely on the exact RegisterClass type; instead we match on its Debug output. This is
    // surprisingly stable across yaxpeax-x86 versions and avoids three copies of the mapping logic
    // for the three mode modules.
    let dbg = format!("{class:?}");
    let kind = dbg
        .split("kind: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or(dbg.as_str());

    match kind {
        "Q" => Some(Operand::Gpr {
            reg: crate::inst::Gpr { index: idx },
            size: OperandSize::Bits64,
            high8: false,
        }),
        "D" => Some(Operand::Gpr {
            reg: crate::inst::Gpr { index: idx },
            size: OperandSize::Bits32,
            high8: false,
        }),
        "W" => Some(Operand::Gpr {
            reg: crate::inst::Gpr { index: idx },
            size: OperandSize::Bits16,
            high8: false,
        }),
        "B" | "rB" => {
            let (base, high8) = if (4..=7).contains(&idx) && prefixes.rex.is_none() {
                (idx - 4, true)
            } else {
                (idx, false)
            };
            Some(Operand::Gpr {
                reg: crate::inst::Gpr { index: base },
                size: OperandSize::Bits8,
                high8,
            })
        }
        "X" => Some(Operand::Xmm {
            reg: crate::inst::Xmm { index: idx },
        }),
        "S" => {
            let seg = match idx {
                0 => SegmentReg::ES,
                1 => SegmentReg::CS,
                2 => SegmentReg::SS,
                3 => SegmentReg::DS,
                4 => SegmentReg::FS,
                5 => SegmentReg::GS,
                _ => return None,
            };
            Some(Operand::Segment { reg: seg })
        }
        "C" => Some(Operand::Control { index: idx }),
        "Dbg" | "DBG" => Some(Operand::Debug { index: idx }),
        _ => Some(Operand::OtherReg {
            class: other_reg_class_from_kind(kind),
            index: idx,
        }),
    }
}

fn other_reg_class_from_kind(kind: &str) -> crate::inst::OtherRegClass {
    use crate::inst::OtherRegClass as C;
    let k = kind.trim();
    if k.starts_with("ST") {
        C::Fpu
    } else if k.starts_with("MM") {
        C::Mmx
    } else if k.starts_with('Y') {
        C::Ymm
    } else if k.starts_with('Z') {
        C::Zmm
    } else if k.starts_with('K') {
        C::Mask
    } else {
        C::Unknown
    }
}

#[cfg(test)]
mod yaxpeax_shape_smoke {
    // This module exists purely to lock down assumptions about the upstream decoder's public API.
    // It is not a behavioural test of `aero_x86`; the real correctness tests live in `tests/`.
    #[test]
    fn operand_debug_shapes() {
        use yaxpeax_arch::{Decoder, U8Reader};

        // mov eax, 0x12345678
        let bytes = [0xB8, 0x78, 0x56, 0x34, 0x12];
        let decoder = yaxpeax_x86::protected_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov eax, [rax+4]
        let bytes = [0x8B, 0x40, 0x04];
        let decoder = yaxpeax_x86::long_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov eax, [rax]
        let bytes = [0x8B, 0x00];
        let decoder = yaxpeax_x86::long_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov eax, [rax+rcx*4+0x10]
        let bytes = [0x8B, 0x44, 0x88, 0x10];
        let decoder = yaxpeax_x86::long_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov eax, [rip+0x1234]
        let bytes = [0x8B, 0x05, 0x34, 0x12, 0x00, 0x00];
        let decoder = yaxpeax_x86::long_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov eax, [rax+rcx*4]
        let bytes = [0x8B, 0x04, 0x88];
        let decoder = yaxpeax_x86::long_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov eax, [rcx*4 + 0x10] (no base)
        let bytes = [0x8B, 0x04, 0x8D, 0x10, 0x00, 0x00, 0x00];
        let decoder = yaxpeax_x86::long_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov ax, [bx+si+0x10] (16-bit addressing)
        let bytes = [0x8B, 0x40, 0x10];
        let decoder = yaxpeax_x86::real_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // jmp rel32
        let bytes = [0xE9, 0x01, 0x00, 0x00, 0x00];
        let decoder = yaxpeax_x86::protected_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov eax, [0x12345678] (moffs32)
        let bytes = [0xA1, 0x78, 0x56, 0x34, 0x12];
        let decoder = yaxpeax_x86::protected_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // add al, 0xFF
        let bytes = [0x04, 0xFF];
        let decoder = yaxpeax_x86::protected_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }

        // mov eax, 0xFFFFFFFF
        let bytes = [0xB8, 0xFF, 0xFF, 0xFF, 0xFF];
        let decoder = yaxpeax_x86::protected_mode::InstDecoder::default();
        let mut reader = U8Reader::new(&bytes);
        let inst = decoder.decode(&mut reader).unwrap();
        for i in 0..4u8 {
            let _ = format!("{:?}", inst.operand(i));
        }
    }
}
