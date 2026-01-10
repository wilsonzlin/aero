use crate::bus::CpuBus;
use crate::dispatch::{DecodedInst, OpcodeKind};
use crate::interpreter::Exception;

const MAX_INST_LEN: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockEndKind {
    Fallthrough,
    Branch,
    Exit,
}

#[derive(Debug, Clone)]
pub struct DecodedBlock {
    pub start_rip: u64,
    pub insts: Vec<DecodedInst>,
    pub end_kind: BlockEndKind,
    /// (page_base, version) pairs for invalidation.
    pub page_versions: Vec<(u64, u64)>,
}

impl DecodedBlock {
    pub fn is_still_valid<B: CpuBus>(&self, bus: &B) -> bool {
        self.page_versions
            .iter()
            .all(|(page_base, version)| bus.page_version(*page_base) == *version)
    }
}

pub struct Decoder;

impl Decoder {
    pub fn decode_inst<B: CpuBus>(bus: &B, rip: u64) -> Result<DecodedInst, Exception> {
        let mut bytes = [0u8; MAX_INST_LEN];
        for i in 0..MAX_INST_LEN {
            bytes[i] = bus.read_u8(rip + i as u64)?;
        }

        let mut idx = 0;
        // REX prefix.
        let mut rex_w = false;
        let mut rex_b = false;
        if (0x40..=0x4F).contains(&bytes[idx]) {
            let rex = bytes[idx];
            rex_w = (rex & 0x08) != 0;
            rex_b = (rex & 0x01) != 0;
            idx += 1;
        }

        let b0 = bytes[idx];
        match b0 {
            0x90 => Ok(DecodedInst::new_simple(OpcodeKind::Nop, 1 + idx as u8)),
            0xF4 => Ok(DecodedInst::new_simple(OpcodeKind::Hlt, 1 + idx as u8)),
            0xFB => Ok(DecodedInst::new_simple(OpcodeKind::Sti, 1 + idx as u8)),
            0x75 => {
                let disp = bytes[idx + 1] as i8 as i32;
                Ok(DecodedInst::new_jcc(
                    OpcodeKind::JnzRel,
                    2 + idx as u8,
                    disp,
                ))
            }
            0x0F => {
                let b1 = bytes[idx + 1];
                match b1 {
                    0x85 => {
                        let disp = i32::from_le_bytes([
                            bytes[idx + 2],
                            bytes[idx + 3],
                            bytes[idx + 4],
                            bytes[idx + 5],
                        ]);
                        Ok(DecodedInst::new_jcc(
                            OpcodeKind::JnzRel,
                            6 + idx as u8,
                            disp,
                        ))
                    }
                    _ => Err(Exception::DecodeError { rip }),
                }
            }
            0xF3 => {
                let b1 = bytes[idx + 1];
                match b1 {
                    0xA4 => Ok(DecodedInst::new_simple(OpcodeKind::RepMovsb, 2 + idx as u8)),
                    _ => Err(Exception::DecodeError { rip }),
                }
            }
            0xFF => {
                // Group 5: FF /1 = DEC r/m64 (when REX.W=1).
                if !rex_w {
                    return Err(Exception::DecodeError { rip });
                }

                let modrm = bytes[idx + 1];
                let mod_bits = modrm >> 6;
                let reg_bits = (modrm >> 3) & 0x7;
                let rm_bits = modrm & 0x7;
                if mod_bits != 0b11 || reg_bits != 0b001 {
                    return Err(Exception::DecodeError { rip });
                }
                let reg_index = rm_bits | ((rex_b as u8) << 3);
                Ok(DecodedInst::new_reg(
                    OpcodeKind::DecReg,
                    2 + idx as u8,
                    reg_index,
                ))
            }
            0x8E => {
                // MOV Sreg, r/m16. We only model MOV SS, AX as the
                // interrupt-shadowing instruction.
                let modrm = bytes[idx + 1];
                let mod_bits = modrm >> 6;
                let reg_bits = (modrm >> 3) & 0x7;
                let rm_bits = modrm & 0x7;
                if mod_bits == 0b11 && reg_bits == 0b010 && rm_bits == 0b000 {
                    // MOV SS, AX
                    Ok(DecodedInst::new_simple(OpcodeKind::MovSsAx, 2 + idx as u8))
                } else {
                    Err(Exception::DecodeError { rip })
                }
            }
            _ => Err(Exception::DecodeError { rip }),
        }
    }

    pub fn decode_block<B: CpuBus>(
        bus: &B,
        start_rip: u64,
        max_insts: usize,
    ) -> Result<DecodedBlock, Exception> {
        let mut insts = Vec::new();
        let mut rip = start_rip;
        let mut end_kind = BlockEndKind::Fallthrough;

        for _ in 0..max_insts {
            let inst = Self::decode_inst(bus, rip)?;
            let opcode = inst.opcode;
            let len = inst.len as u64;
            insts.push(inst);

            rip = rip.wrapping_add(len);

            match opcode {
                OpcodeKind::JnzRel => {
                    end_kind = BlockEndKind::Branch;
                    break;
                }
                OpcodeKind::Hlt => {
                    end_kind = BlockEndKind::Exit;
                    break;
                }
                OpcodeKind::Invalid => return Err(Exception::DecodeError { rip: start_rip }),
                _ => {}
            }
        }

        let end_rip = rip;
        // Snapshot versions for all pages spanned by the block bytes.
        let mut page_versions = Vec::new();
        if end_rip > start_rip {
            let start_page = start_rip & !(crate::bus::PAGE_SIZE - 1);
            let end_page = (end_rip - 1) & !(crate::bus::PAGE_SIZE - 1);
            let mut page = start_page;
            loop {
                page_versions.push((page, bus.page_version(page)));
                if page == end_page {
                    break;
                }
                page = page.wrapping_add(crate::bus::PAGE_SIZE);
            }
        }

        Ok(DecodedBlock {
            start_rip,
            insts,
            end_kind,
            page_versions,
        })
    }
}
