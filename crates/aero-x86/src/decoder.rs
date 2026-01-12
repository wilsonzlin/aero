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

    let (mut opcode, opcode_len) = parse_opcode(bytes, mode, prefix_len)?;

    // For group opcodes, include ModRM.reg in `opcode_ext` where it matters.
    if opcode_uses_modrm_reg(opcode.map, opcode.opcode) {
        let modrm_off = prefix_len + opcode_len;
        let modrm = *bytes.get(modrm_off).ok_or(DecodeError::UnexpectedEof)?;
        opcode.opcode_ext = Some((modrm >> 3) & 0x7);
    }

    // Validate opcodes whose ModRM.reg is a fixed extension.
    //
    // `0xC6`/`0xC7` are normally `MOV r/m, imm` (Group 11, `/0`), but Intel TSX also
    // defines `XABORT` (`C6 F8 ib`) and `XBEGIN` (`C7 F8 iw/id`) using `/7` with a
    // fixed ModRM byte of `0xF8`.
    if opcode.map == OpcodeMap::Primary && matches!(opcode.opcode, 0xC6 | 0xC7) {
        match opcode.opcode_ext {
            Some(0) => {}
            Some(7) => {
                let modrm_off = prefix_len + opcode_len;
                let modrm = *bytes.get(modrm_off).ok_or(DecodeError::UnexpectedEof)?;
                if modrm != 0xF8 {
                    return Err(DecodeError::Invalid);
                }
            }
            _ => return Err(DecodeError::Invalid),
        }
    }

    // Intel TSX defines XABORT (C6 F8 ib) and XBEGIN (C7 F8 iw/id). Some upstream decoders treat
    // these encodings as Group 11 MOV instructions. Route to the iced-powered decoder so our
    // operand kinds match iced-x86 (including the relative operand for XBEGIN).
    if opcode.map == OpcodeMap::Primary
        && matches!(opcode.opcode, 0xC6 | 0xC7)
        && opcode.opcode_ext == Some(7)
    {
        let (mut operands, inst_len) =
            decode_with_aero_cpu_decoder(bytes, mode, ip, prefixes, address_size)?;

        if inst_len > MAX_INST_LEN {
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

    if opcode.map == OpcodeMap::Extended {
        let (mut operands, inst_len) =
            decode_with_aero_cpu_decoder(bytes, mode, ip, prefixes, address_size)?;

        if inst_len > MAX_INST_LEN {
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

    // Some relative branch/call opcodes have operand-size-dependent immediate widths, and not all
    // upstream decoders agree on how to interpret `0x66` in 16-bit mode. Decode these cases
    // ourselves so we match iced-x86 for block formation.
    let imm_off = prefix_len + opcode_len;
    if let Some((operands, inst_len)) = decode_relative_immediate(
        bytes,
        RelativeImmediateDecodeContext {
            mode,
            ip,
            prefixes,
            imm_off,
            opcode,
            operand_size,
        },
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
        Err(DecodeError::Invalid)
            if mode == DecodeMode::Bits64
                && opcode.map == OpcodeMap::Primary
                && matches!(opcode.opcode, 0x8C | 0x8E) =>
        {
            // yaxpeax-x86 rejects some long-mode encodings of MOV to/from segment registers even
            // though they are accepted by real hardware and iced-x86. Fall back to the
            // iced-powered decoder for these opcodes to keep validity in sync.
            decode_with_aero_cpu_decoder(bytes, mode, ip, prefixes, address_size)?
        }
        Err(DecodeError::Invalid) if mode == DecodeMode::Bits64 => {
            let prefix_bytes = bytes.get(..prefix_len).unwrap_or(&[]);
            let rex_count = prefix_bytes
                .iter()
                .filter(|b| (0x40..=0x4F).contains(*b))
                .count();
            let has_ignored_seg_prefix = prefix_bytes
                .iter()
                .any(|b| matches!(b, 0x26 | 0x2E | 0x36 | 0x3E));
            let needs_rex_r_mask = opcode.map == OpcodeMap::Primary
                && matches!(opcode.opcode, 0x8C | 0x8E)
                && prefixes.rex.is_some_and(|r| r.r);

            if rex_count > 1 || has_ignored_seg_prefix || needs_rex_r_mask {
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
                    let mut b = b;
                    if needs_rex_r_mask && (0x40..=0x4F).contains(&b) && last_rex_pos == Some(i) {
                        b &= !0b0100;
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
                && matches!(opcode.opcode, 0x9A | 0xEA)
                && !prefixes.lock =>
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

    // Segment register moves always operate on 16-bit operands, regardless of mode/prefixes.
    let mut inst_operand_size = operand_size;
    if opcode.map == OpcodeMap::Primary && matches!(opcode.opcode, 0x8C | 0x8E) {
        inst_operand_size = OperandSize::Bits16;
        for op in &mut operands {
            if let Operand::Gpr { size, high8, .. } = op {
                *size = OperandSize::Bits16;
                *high8 = false;
            }
        }
    }

    // If prefix scanning already consumed 15 bytes, we don't have space for opcode.
    if prefix_len >= MAX_INST_LEN {
        return Err(DecodeError::TooLong);
    }

    fixup_implicit_operands(
        opcode,
        mode,
        prefixes,
        inst_operand_size,
        address_size,
        &mut operands,
    );

    // Control-flow classification for block formation.
    let flags = classify_inst(opcode, &operands);

    Ok(DecodedInst {
        length: inst_len as u8,
        opcode,
        prefixes,
        operand_size: inst_operand_size,
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
        (OpcodeMap::Map0F, 0xF7, _)
            if !operands.iter().any(|op| matches!(op, Operand::Memory(_))) =>
        {
            // MASKMOVQ/MASKMOVDQU: implicit destination memory operand at [DI/EDI/RDI] (default
            // segment selection, overridable).
            operands.push(Operand::Memory(crate::inst::MemoryOperand {
                segment: prefixes.segment,
                addr_size: address_size,
                base: Some(crate::inst::Gpr { index: 7 }), // DI/EDI/RDI
                index: None,
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
                && operands.first() == operands.get(1)
                && matches!(operands.first(), Some(Operand::Memory(_))) =>
        {
            // Some decoders expose the `0F 18..1F /r` "reserved NOP" encodings as `r/m, r` where the
            // ModRM.reg field selects a register operand. `yaxpeax-x86` currently models these as
            // `mem, mem`; rewrite the second operand to the expected register form.
            let mut reg_index = reg;
            if mode == DecodeMode::Bits64 && prefixes.rex.is_some_and(|r| r.r) {
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
            if operands.len() == 2 && operands.first() == operands.get(1) =>
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
            // In 64-bit mode, only FS/GS segment overrides have architectural effect; CS/DS/ES/SS
            // are accepted but ignored.
            //
            // Importantly, ignored segment prefixes **must not clear** an earlier FS/GS override.
            // Real hardware (and iced-x86) treats CS/DS/ES/SS overrides in long mode as true no-ops.
            // i.e. `FS; DS; <mem-op>` still uses FS, not "no override".
            match (mode, seg) {
                (DecodeMode::Bits64, SegmentReg::FS | SegmentReg::GS) => {
                    prefixes.segment = Some(seg);
                }
                (DecodeMode::Bits64, _) => {
                    // Ignored in long mode; do not update `prefixes.segment`.
                }
                _ => {
                    prefixes.segment = Some(seg);
                }
            }
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
            if prefixes.rex.is_some_and(|r| r.w) {
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

fn parse_opcode(
    bytes: &[u8],
    mode: DecodeMode,
    off: usize,
) -> Result<(OpcodeBytes, usize), DecodeError> {
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
    } else if b0 == 0x8F {
        // XOP shares its first byte with `POP r/m16/32/64` (`0x8F /0`). Disambiguate by checking
        // the `m-mmmm` field in byte 2: for a legacy ModRM with `reg=0`, the low 5 bits can only
        // be 0..=7, while XOP uses values 8..=10.
        let b1 = *bytes.get(off + 1).ok_or(DecodeError::UnexpectedEof)?;
        let is_extended = (b1 & 0x1F) >= 8;
        Ok((
            OpcodeBytes {
                map: if is_extended {
                    OpcodeMap::Extended
                } else {
                    OpcodeMap::Primary
                },
                opcode: b0,
                opcode_ext: None,
            },
            1,
        ))
    } else if matches!(b0, 0xC4 | 0xC5 | 0x62) {
        // VEX/EVEX prefixes share their first byte with legacy opcodes (LES/LDS/BOUND). In 16/32-bit
        // modes, the CPU disambiguates them by requiring the following byte to have ModRM.mod=3,
        // which would make the legacy opcodes invalid (they require a memory operand).
        let b1 = *bytes.get(off + 1).ok_or(DecodeError::UnexpectedEof)?;
        let is_extended = mode == DecodeMode::Bits64 || (b1 & 0xC0) == 0xC0;
        Ok((
            OpcodeBytes {
                map: if is_extended {
                    OpcodeMap::Extended
                } else {
                    OpcodeMap::Primary
                },
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

#[derive(Debug, Clone, Copy)]
struct RelativeImmediateDecodeContext {
    mode: DecodeMode,
    ip: u64,
    prefixes: Prefixes,
    imm_off: usize,
    opcode: OpcodeBytes,
    operand_size: OperandSize,
}

fn decode_relative_immediate(
    bytes: &[u8],
    ctx: RelativeImmediateDecodeContext,
) -> Result<Option<(Vec<Operand>, usize)>, DecodeError> {
    let (imm_len, rel_size) = match (ctx.opcode.map, ctx.opcode.opcode) {
        (OpcodeMap::Primary, 0xE8 | 0xE9) => {
            if ctx.mode == DecodeMode::Bits64 {
                (4usize, OperandSize::Bits32)
            } else if ctx.operand_size == OperandSize::Bits16 {
                (2usize, OperandSize::Bits16)
            } else {
                (4usize, OperandSize::Bits32)
            }
        }
        (OpcodeMap::Map0F, 0x80..=0x8F) => {
            if ctx.mode == DecodeMode::Bits64 {
                (4usize, OperandSize::Bits32)
            } else if ctx.operand_size == OperandSize::Bits16 {
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
    if ctx.prefixes.lock {
        return Err(DecodeError::Invalid);
    }

    let imm_off = ctx.imm_off;
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

    let next_ip = ctx.ip.wrapping_add(inst_len as u64);
    let ip_mask = if ctx.mode == DecodeMode::Bits64 {
        u64::MAX
    } else if ctx.operand_size == OperandSize::Bits16 {
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
        (OpcodeMap::Primary, 0xE9..=0xEB) => flags.is_branch = true,
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

fn decode_with_aero_cpu_decoder(
    bytes: &[u8],
    mode: DecodeMode,
    ip: u64,
    prefixes: Prefixes,
    address_size: AddressSize,
) -> Result<(Vec<Operand>, usize), DecodeError> {
    use aero_cpu_decoder::{decode_instruction, DecodeError as IcedErr, DecodeMode as IcedMode};

    let mode = match mode {
        DecodeMode::Bits16 => IcedMode::Bits16,
        DecodeMode::Bits32 => IcedMode::Bits32,
        DecodeMode::Bits64 => IcedMode::Bits64,
    };

    let bytes = if bytes.len() > MAX_INST_LEN {
        &bytes[..MAX_INST_LEN]
    } else {
        bytes
    };

    let inst = decode_instruction(mode, ip, bytes).map_err(|e| match e {
        IcedErr::EmptyInput | IcedErr::UnexpectedEof => DecodeError::UnexpectedEof,
        IcedErr::InvalidInstruction => DecodeError::Invalid,
    })?;

    let len = inst.len() as usize;
    if len == 0 || len > MAX_INST_LEN {
        return Err(DecodeError::Invalid);
    }
    let next_ip = ip.wrapping_add(len as u64);

    let mut out = Vec::with_capacity(inst.op_count() as usize);
    for i in 0..inst.op_count() {
        out.extend(convert_iced_operand(
            &inst,
            i,
            prefixes,
            address_size,
            next_ip,
        ));
    }

    Ok((out, len))
}

fn convert_iced_operand(
    inst: &aero_cpu_decoder::Instruction,
    idx: u32,
    prefixes: Prefixes,
    address_size: AddressSize,
    next_ip: u64,
) -> Option<Operand> {
    use aero_cpu_decoder::OpKind;

    match inst.op_kind(idx) {
        OpKind::Register => map_iced_register(inst.op_register(idx)),
        OpKind::Memory => Some(Operand::Memory(map_iced_memory(
            inst,
            prefixes,
            address_size,
            next_ip,
        ))),
        OpKind::NearBranch16 => Some(Operand::Relative {
            target: inst.near_branch_target(),
            size: OperandSize::Bits16,
        }),
        OpKind::NearBranch32 => Some(Operand::Relative {
            target: inst.near_branch_target(),
            size: OperandSize::Bits32,
        }),
        OpKind::NearBranch64 => Some(Operand::Relative {
            target: inst.near_branch_target(),
            size: OperandSize::Bits64,
        }),
        OpKind::Immediate8 => Some(Operand::Immediate(Immediate {
            value: inst.immediate8() as u64,
            size: OperandSize::Bits8,
            is_signed: false,
        })),
        OpKind::Immediate8_2nd => Some(Operand::Immediate(Immediate {
            value: inst.immediate8_2nd() as u64,
            size: OperandSize::Bits8,
            is_signed: false,
        })),
        OpKind::Immediate16 => Some(Operand::Immediate(Immediate {
            value: inst.immediate16() as u64,
            size: OperandSize::Bits16,
            is_signed: false,
        })),
        OpKind::Immediate32 => Some(Operand::Immediate(Immediate {
            value: inst.immediate32() as u64,
            size: OperandSize::Bits32,
            is_signed: false,
        })),
        OpKind::Immediate64 => Some(Operand::Immediate(Immediate {
            value: inst.immediate64(),
            size: OperandSize::Bits64,
            is_signed: false,
        })),
        OpKind::Immediate8to16 => Some(Operand::Immediate(Immediate {
            value: inst.immediate8to16() as u16 as u64,
            size: OperandSize::Bits16,
            is_signed: true,
        })),
        OpKind::Immediate8to32 => Some(Operand::Immediate(Immediate {
            value: inst.immediate8to32() as u32 as u64,
            size: OperandSize::Bits32,
            is_signed: true,
        })),
        OpKind::Immediate8to64 => Some(Operand::Immediate(Immediate {
            value: inst.immediate8to64() as u64,
            size: OperandSize::Bits64,
            is_signed: true,
        })),
        OpKind::Immediate32to64 => Some(Operand::Immediate(Immediate {
            value: inst.immediate32to64() as u64,
            size: OperandSize::Bits64,
            is_signed: true,
        })),
        _ => None,
    }
}

fn map_iced_memory(
    inst: &aero_cpu_decoder::Instruction,
    prefixes: Prefixes,
    address_size: AddressSize,
    next_ip: u64,
) -> crate::inst::MemoryOperand {
    use aero_cpu_decoder::Register;

    let base = inst.memory_base();
    let index = inst.memory_index();
    let mut disp = inst.memory_displacement64() as i128;
    let mut rip_relative = false;
    let base_gpr = if base == Register::RIP {
        // iced-x86 represents RIP-relative displacement as an absolute address (next_ip + disp32).
        // Convert back to disp32 so downstream EA calculation can treat it uniformly.
        rip_relative = true;
        disp -= next_ip as i128;
        None
    } else {
        gpr_from_iced_register(base)
    };

    crate::inst::MemoryOperand {
        segment: prefixes.segment,
        addr_size: address_size,
        base: base_gpr,
        index: gpr_from_iced_register(index),
        scale: inst.memory_index_scale() as u8,
        disp: disp as i64,
        rip_relative,
    }
}

fn gpr_from_iced_register(reg: aero_cpu_decoder::Register) -> Option<crate::inst::Gpr> {
    use aero_cpu_decoder::Register;
    let idx = match reg {
        Register::None => return None,
        Register::AL | Register::AX | Register::EAX | Register::RAX => 0,
        Register::CL | Register::CX | Register::ECX | Register::RCX => 1,
        Register::DL | Register::DX | Register::EDX | Register::RDX => 2,
        Register::BL | Register::BX | Register::EBX | Register::RBX => 3,
        Register::SPL | Register::SP | Register::ESP | Register::RSP => 4,
        Register::BPL | Register::BP | Register::EBP | Register::RBP => 5,
        Register::SIL | Register::SI | Register::ESI | Register::RSI => 6,
        Register::DIL | Register::DI | Register::EDI | Register::RDI => 7,
        Register::R8L | Register::R8W | Register::R8D | Register::R8 => 8,
        Register::R9L | Register::R9W | Register::R9D | Register::R9 => 9,
        Register::R10L | Register::R10W | Register::R10D | Register::R10 => 10,
        Register::R11L | Register::R11W | Register::R11D | Register::R11 => 11,
        Register::R12L | Register::R12W | Register::R12D | Register::R12 => 12,
        Register::R13L | Register::R13W | Register::R13D | Register::R13 => 13,
        Register::R14L | Register::R14W | Register::R14D | Register::R14 => 14,
        Register::R15L | Register::R15W | Register::R15D | Register::R15 => 15,
        _ => return None,
    };
    Some(crate::inst::Gpr { index: idx })
}

fn map_iced_register(reg: aero_cpu_decoder::Register) -> Option<Operand> {
    use aero_cpu_decoder::Register::*;
    let (idx, size, high8) = match reg {
        AL => (0, OperandSize::Bits8, false),
        CL => (1, OperandSize::Bits8, false),
        DL => (2, OperandSize::Bits8, false),
        BL => (3, OperandSize::Bits8, false),
        SPL => (4, OperandSize::Bits8, false),
        BPL => (5, OperandSize::Bits8, false),
        SIL => (6, OperandSize::Bits8, false),
        DIL => (7, OperandSize::Bits8, false),
        R8L => (8, OperandSize::Bits8, false),
        R9L => (9, OperandSize::Bits8, false),
        R10L => (10, OperandSize::Bits8, false),
        R11L => (11, OperandSize::Bits8, false),
        R12L => (12, OperandSize::Bits8, false),
        R13L => (13, OperandSize::Bits8, false),
        R14L => (14, OperandSize::Bits8, false),
        R15L => (15, OperandSize::Bits8, false),

        AH => (0, OperandSize::Bits8, true),
        CH => (1, OperandSize::Bits8, true),
        DH => (2, OperandSize::Bits8, true),
        BH => (3, OperandSize::Bits8, true),

        AX => (0, OperandSize::Bits16, false),
        CX => (1, OperandSize::Bits16, false),
        DX => (2, OperandSize::Bits16, false),
        BX => (3, OperandSize::Bits16, false),
        SP => (4, OperandSize::Bits16, false),
        BP => (5, OperandSize::Bits16, false),
        SI => (6, OperandSize::Bits16, false),
        DI => (7, OperandSize::Bits16, false),
        R8W => (8, OperandSize::Bits16, false),
        R9W => (9, OperandSize::Bits16, false),
        R10W => (10, OperandSize::Bits16, false),
        R11W => (11, OperandSize::Bits16, false),
        R12W => (12, OperandSize::Bits16, false),
        R13W => (13, OperandSize::Bits16, false),
        R14W => (14, OperandSize::Bits16, false),
        R15W => (15, OperandSize::Bits16, false),

        EAX => (0, OperandSize::Bits32, false),
        ECX => (1, OperandSize::Bits32, false),
        EDX => (2, OperandSize::Bits32, false),
        EBX => (3, OperandSize::Bits32, false),
        ESP => (4, OperandSize::Bits32, false),
        EBP => (5, OperandSize::Bits32, false),
        ESI => (6, OperandSize::Bits32, false),
        EDI => (7, OperandSize::Bits32, false),
        R8D => (8, OperandSize::Bits32, false),
        R9D => (9, OperandSize::Bits32, false),
        R10D => (10, OperandSize::Bits32, false),
        R11D => (11, OperandSize::Bits32, false),
        R12D => (12, OperandSize::Bits32, false),
        R13D => (13, OperandSize::Bits32, false),
        R14D => (14, OperandSize::Bits32, false),
        R15D => (15, OperandSize::Bits32, false),

        RAX => (0, OperandSize::Bits64, false),
        RCX => (1, OperandSize::Bits64, false),
        RDX => (2, OperandSize::Bits64, false),
        RBX => (3, OperandSize::Bits64, false),
        RSP => (4, OperandSize::Bits64, false),
        RBP => (5, OperandSize::Bits64, false),
        RSI => (6, OperandSize::Bits64, false),
        RDI => (7, OperandSize::Bits64, false),
        R8 => (8, OperandSize::Bits64, false),
        R9 => (9, OperandSize::Bits64, false),
        R10 => (10, OperandSize::Bits64, false),
        R11 => (11, OperandSize::Bits64, false),
        R12 => (12, OperandSize::Bits64, false),
        R13 => (13, OperandSize::Bits64, false),
        R14 => (14, OperandSize::Bits64, false),
        R15 => (15, OperandSize::Bits64, false),

        ES => {
            return Some(Operand::Segment {
                reg: SegmentReg::ES,
            })
        }
        CS => {
            return Some(Operand::Segment {
                reg: SegmentReg::CS,
            })
        }
        SS => {
            return Some(Operand::Segment {
                reg: SegmentReg::SS,
            })
        }
        DS => {
            return Some(Operand::Segment {
                reg: SegmentReg::DS,
            })
        }
        FS => {
            return Some(Operand::Segment {
                reg: SegmentReg::FS,
            })
        }
        GS => {
            return Some(Operand::Segment {
                reg: SegmentReg::GS,
            })
        }

        CR0 => return Some(Operand::Control { index: 0 }),
        CR1 => return Some(Operand::Control { index: 1 }),
        CR2 => return Some(Operand::Control { index: 2 }),
        CR3 => return Some(Operand::Control { index: 3 }),
        CR4 => return Some(Operand::Control { index: 4 }),
        CR5 => return Some(Operand::Control { index: 5 }),
        CR6 => return Some(Operand::Control { index: 6 }),
        CR7 => return Some(Operand::Control { index: 7 }),
        CR8 => return Some(Operand::Control { index: 8 }),
        CR9 => return Some(Operand::Control { index: 9 }),
        CR10 => return Some(Operand::Control { index: 10 }),
        CR11 => return Some(Operand::Control { index: 11 }),
        CR12 => return Some(Operand::Control { index: 12 }),
        CR13 => return Some(Operand::Control { index: 13 }),
        CR14 => return Some(Operand::Control { index: 14 }),
        CR15 => return Some(Operand::Control { index: 15 }),

        DR0 => return Some(Operand::Debug { index: 0 }),
        DR1 => return Some(Operand::Debug { index: 1 }),
        DR2 => return Some(Operand::Debug { index: 2 }),
        DR3 => return Some(Operand::Debug { index: 3 }),
        DR4 => return Some(Operand::Debug { index: 4 }),
        DR5 => return Some(Operand::Debug { index: 5 }),
        DR6 => return Some(Operand::Debug { index: 6 }),
        DR7 => return Some(Operand::Debug { index: 7 }),
        DR8 => return Some(Operand::Debug { index: 8 }),
        DR9 => return Some(Operand::Debug { index: 9 }),
        DR10 => return Some(Operand::Debug { index: 10 }),
        DR11 => return Some(Operand::Debug { index: 11 }),
        DR12 => return Some(Operand::Debug { index: 12 }),
        DR13 => return Some(Operand::Debug { index: 13 }),
        DR14 => return Some(Operand::Debug { index: 14 }),
        DR15 => return Some(Operand::Debug { index: 15 }),

        XMM0 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 0 },
            })
        }
        XMM1 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 1 },
            })
        }
        XMM2 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 2 },
            })
        }
        XMM3 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 3 },
            })
        }
        XMM4 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 4 },
            })
        }
        XMM5 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 5 },
            })
        }
        XMM6 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 6 },
            })
        }
        XMM7 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 7 },
            })
        }
        XMM8 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 8 },
            })
        }
        XMM9 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 9 },
            })
        }
        XMM10 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 10 },
            })
        }
        XMM11 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 11 },
            })
        }
        XMM12 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 12 },
            })
        }
        XMM13 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 13 },
            })
        }
        XMM14 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 14 },
            })
        }
        XMM15 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 15 },
            })
        }
        XMM16 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 16 },
            })
        }
        XMM17 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 17 },
            })
        }
        XMM18 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 18 },
            })
        }
        XMM19 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 19 },
            })
        }
        XMM20 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 20 },
            })
        }
        XMM21 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 21 },
            })
        }
        XMM22 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 22 },
            })
        }
        XMM23 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 23 },
            })
        }
        XMM24 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 24 },
            })
        }
        XMM25 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 25 },
            })
        }
        XMM26 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 26 },
            })
        }
        XMM27 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 27 },
            })
        }
        XMM28 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 28 },
            })
        }
        XMM29 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 29 },
            })
        }
        XMM30 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 30 },
            })
        }
        XMM31 => {
            return Some(Operand::Xmm {
                reg: crate::inst::Xmm { index: 31 },
            })
        }

        YMM0 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 0,
            })
        }
        YMM1 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 1,
            })
        }
        YMM2 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 2,
            })
        }
        YMM3 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 3,
            })
        }
        YMM4 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 4,
            })
        }
        YMM5 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 5,
            })
        }
        YMM6 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 6,
            })
        }
        YMM7 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 7,
            })
        }
        YMM8 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 8,
            })
        }
        YMM9 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 9,
            })
        }
        YMM10 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 10,
            })
        }
        YMM11 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 11,
            })
        }
        YMM12 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 12,
            })
        }
        YMM13 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 13,
            })
        }
        YMM14 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 14,
            })
        }
        YMM15 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 15,
            })
        }
        YMM16 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 16,
            })
        }
        YMM17 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 17,
            })
        }
        YMM18 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 18,
            })
        }
        YMM19 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 19,
            })
        }
        YMM20 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 20,
            })
        }
        YMM21 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 21,
            })
        }
        YMM22 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 22,
            })
        }
        YMM23 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 23,
            })
        }
        YMM24 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 24,
            })
        }
        YMM25 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 25,
            })
        }
        YMM26 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 26,
            })
        }
        YMM27 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 27,
            })
        }
        YMM28 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 28,
            })
        }
        YMM29 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 29,
            })
        }
        YMM30 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 30,
            })
        }
        YMM31 => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Ymm,
                index: 31,
            })
        }

        _ => {
            return Some(Operand::OtherReg {
                class: crate::inst::OtherRegClass::Unknown,
                index: 0,
            })
        }
    };

    Some(Operand::Gpr {
        reg: crate::inst::Gpr { index: idx },
        size,
        high8,
    })
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
        addr_size,
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
        addr_size,
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
            addr_size,
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
            addr_size,
            base: None,
            index: None,
            scale: 1,
            disp,
            rip_relative: true,
        }
    } else {
        crate::inst::MemoryOperand {
            segment: prefixes.segment,
            addr_size,
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
        addr_size,
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
