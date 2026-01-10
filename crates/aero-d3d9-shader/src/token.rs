use crate::reg::{
    DstParam, Register, RegisterType, RelativeAddress, SrcModifier, SrcParam, Swizzle,
};

const REGNUM_MASK: u32 = 0x0000_07FF;
const REGTYPE_MASK: u32 = 0x7000_0000;
const REGTYPE_MASK2: u32 = 0x0000_1800;
const REGTYPE_SHIFT: u32 = 28;
const REGTYPE_SHIFT2: u32 = 8;

const DST_WRITEMASK_SHIFT: u32 = 16;
const DST_WRITEMASK_MASK: u32 = 0x000F_0000;

const DSTMOD_SHIFT: u32 = 20;
const DSTMOD_MASK: u32 = 0x00F0_0000;

const SRC_SWIZZLE_SHIFT: u32 = 16;
const SRC_SWIZZLE_MASK: u32 = 0x00FF_0000;

const SRCMOD_SHIFT: u32 = 24;
const SRCMOD_MASK: u32 = 0x0F00_0000;

const ADDR_MODE_RELATIVE: u32 = 0x0000_2000;

fn decode_reg(token: u32) -> Register {
    let num = (token & REGNUM_MASK) as u16;
    let ty_raw = (((token & REGTYPE_MASK) >> REGTYPE_SHIFT)
        | ((token & REGTYPE_MASK2) >> REGTYPE_SHIFT2)) as u8;
    Register {
        ty: RegisterType::from_raw(ty_raw),
        num,
    }
}

pub fn decode_dst_param(token: u32) -> DstParam {
    let reg = decode_reg(token);
    let write_mask = ((token & DST_WRITEMASK_MASK) >> DST_WRITEMASK_SHIFT) as u8;
    let dstmod = ((token & DSTMOD_MASK) >> DSTMOD_SHIFT) as u8;
    DstParam {
        reg,
        write_mask,
        saturate: dstmod & 0x1 != 0,
        partial_precision: dstmod & 0x2 != 0,
        centroid: dstmod & 0x4 != 0,
    }
}

pub fn decode_src_param(tokens: &[u32], idx: &mut usize) -> SrcParam {
    if *idx >= tokens.len() {
        return SrcParam::Immediate(0);
    }
    let token = tokens[*idx];
    *idx += 1;

    // Parameter tokens have bit31 set. If it's not set, treat it as an immediate literal.
    if token & 0x8000_0000 == 0 {
        return SrcParam::Immediate(token);
    }

    let reg = decode_reg(token);
    let swz = ((token & SRC_SWIZZLE_MASK) >> SRC_SWIZZLE_SHIFT) as u8;
    let swizzle = Swizzle::from_byte(swz);
    let mod_raw = ((token & SRCMOD_MASK) >> SRCMOD_SHIFT) as u8;
    let modifier = SrcModifier::from_raw(mod_raw);

    let mut relative = None;
    if token & ADDR_MODE_RELATIVE != 0 {
        if *idx < tokens.len() {
            let rel_token = tokens[*idx];
            *idx += 1;
            if rel_token & 0x8000_0000 != 0 {
                let rel_reg = decode_reg(rel_token);
                let rel_swz = ((rel_token & SRC_SWIZZLE_MASK) >> SRC_SWIZZLE_SHIFT) as u8;
                let rel_swizzle = Swizzle::from_byte(rel_swz);
                relative = Some(RelativeAddress {
                    reg: rel_reg,
                    component: rel_swizzle.x,
                });
            } else {
                // Relative token wasn't a register token; step back so it can be treated as an
                // immediate by the caller.
                *idx -= 1;
            }
        }
    }

    SrcParam::Register {
        reg,
        swizzle,
        modifier,
        relative,
    }
}
