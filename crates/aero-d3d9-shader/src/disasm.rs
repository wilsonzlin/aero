use crate::reg::{
    Decl, DstParam, Instruction, Register, RegisterType, SamplerTextureType, ShaderStage,
    SrcModifier, SrcParam, Swizzle, Usage,
};
use crate::D3d9Shader;

pub fn disassemble(shader: &D3d9Shader) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}_{}_{}\n",
        match shader.stage {
            ShaderStage::Vertex => "vs",
            ShaderStage::Pixel => "ps",
        },
        shader.model.major,
        shader.model.minor
    ));

    for decl in &shader.declarations {
        out.push_str(&format!("{}\n", disasm_decl(shader, decl)));
    }

    for inst in &shader.instructions {
        out.push_str(&format!("{}\n", disasm_inst(shader, inst)));
    }

    out
}

fn disasm_decl(shader: &D3d9Shader, decl: &Decl) -> String {
    match decl {
        Decl::Dcl {
            reg,
            usage,
            usage_index,
        } => {
            let mut s = String::new();
            s.push_str("dcl_");
            s.push_str(usage.mnemonic());
            match usage {
                Usage::TexCoord | Usage::Color => {
                    s.push_str(&usage_index.to_string());
                }
                _ => {}
            }
            s.push(' ');
            s.push_str(&format_register(shader, *reg));
            s
        }
        Decl::Sampler { reg, texture_type } => {
            format!(
                "dcl_{} {}",
                match texture_type {
                    SamplerTextureType::Texture2D => "2d",
                    SamplerTextureType::Cube => "cube",
                    SamplerTextureType::Volume => "volume",
                    SamplerTextureType::Unknown(_) => "unknown",
                },
                format_register(shader, *reg)
            )
        }
    }
}

fn disasm_inst(shader: &D3d9Shader, inst: &Instruction) -> String {
    match inst {
        Instruction::Unknown { opcode_raw, tokens } => {
            format!("unknown_0x{opcode_raw:04x} {:?}", tokens)
        }
        Instruction::Op {
            opcode,
            predicate,
            dst,
            src,
            ..
        } => {
            let mut s = String::new();
            if let Some(pred) = predicate {
                s.push('(');
                s.push_str(&format_src(shader, pred));
                s.push_str(") ");
            }

            s.push_str(opcode.mnemonic());

            if let Some(dst) = dst {
                if dst.saturate {
                    s.push_str("_sat");
                }
                if dst.partial_precision {
                    s.push_str("_pp");
                }
                if dst.centroid {
                    s.push_str("_centroid");
                }
            }

            let mut first = true;
            if let Some(dst) = dst {
                s.push(' ');
                s.push_str(&format_dst(shader, dst));
                first = false;
            }
            for src in src {
                if first {
                    s.push(' ');
                    first = false;
                } else {
                    s.push_str(", ");
                }
                s.push_str(&format_src(shader, src));
            }
            s
        }
    }
}

fn format_register(shader: &D3d9Shader, reg: Register) -> String {
    match reg.ty {
        RegisterType::Temp => format!("r{}", reg.num),
        RegisterType::Input => format!("v{}", reg.num),
        RegisterType::Const => format!("c{}", reg.num),
        RegisterType::ConstInt => format!("i{}", reg.num),
        RegisterType::ConstBool => format!("b{}", reg.num),
        RegisterType::Sampler => format!("s{}", reg.num),
        RegisterType::Addr => format!("a{}", reg.num),
        RegisterType::Loop => "aL".to_string(),
        RegisterType::Predicate => format!("p{}", reg.num),
        RegisterType::Label => format!("l{}", reg.num),
        RegisterType::RastOut => match reg.num {
            0 => "oPos".to_string(),
            1 => "oFog".to_string(),
            2 => "oPts".to_string(),
            n => format!("oRast{}", n),
        },
        RegisterType::AttrOut => format!("oD{}", reg.num),
        RegisterType::TexCoordOutOrOutput => {
            if shader.stage == ShaderStage::Vertex && shader.model.major >= 3 {
                format!("o{}", reg.num)
            } else {
                format!("oT{}", reg.num)
            }
        }
        RegisterType::ColorOut => format!("oC{}", reg.num),
        RegisterType::DepthOut => "oDepth".to_string(),
        RegisterType::Unknown(raw) => format!("reg{}[{}]", raw, reg.num),
        RegisterType::Const2 => format!("c2_{}", reg.num),
        RegisterType::Const3 => format!("c3_{}", reg.num),
        RegisterType::Const4 => format!("c4_{}", reg.num),
        RegisterType::TempFloat16 => format!("r16f{}", reg.num),
        RegisterType::MiscType => format!("misc{}", reg.num),
    }
}

fn format_mask(mask: u8) -> String {
    let mut s = String::new();
    if mask & 0x1 != 0 {
        s.push('x');
    }
    if mask & 0x2 != 0 {
        s.push('y');
    }
    if mask & 0x4 != 0 {
        s.push('z');
    }
    if mask & 0x8 != 0 {
        s.push('w');
    }
    s
}

fn format_dst(shader: &D3d9Shader, dst: &DstParam) -> String {
    let mut s = format_register(shader, dst.reg);
    if dst.write_mask != 0xF {
        s.push('.');
        s.push_str(&format_mask(dst.write_mask));
    }
    s
}

fn format_swizzle(swz: Swizzle) -> String {
    format!(
        "{}{}{}{}",
        swz.x.as_char(),
        swz.y.as_char(),
        swz.z.as_char(),
        swz.w.as_char()
    )
}

fn format_src(shader: &D3d9Shader, src: &SrcParam) -> String {
    match src {
        SrcParam::Immediate(v) => format!("0x{v:08x}"),
        SrcParam::Register {
            reg,
            swizzle,
            modifier,
            relative,
        } => {
            let mut s = String::new();

            match modifier {
                SrcModifier::None => {}
                SrcModifier::Neg => s.push('-'),
                SrcModifier::Abs => s.push('|'),
                SrcModifier::AbsNeg => {
                    s.push('-');
                    s.push('|');
                }
                SrcModifier::Unknown(_) => {}
            }

            if let Some(rel) = relative {
                match reg.ty {
                    RegisterType::Temp => s.push('r'),
                    RegisterType::Input => s.push('v'),
                    RegisterType::Const => s.push('c'),
                    RegisterType::ConstInt => s.push('i'),
                    RegisterType::ConstBool => s.push('b'),
                    RegisterType::Sampler => s.push('s'),
                    RegisterType::Addr => s.push('a'),
                    RegisterType::Predicate => s.push('p'),
                    RegisterType::Label => s.push('l'),
                    _ => {
                        // Unknown/rare; fall back to the fully formatted register.
                        s.push_str(&format_register(shader, *reg));
                    }
                }
                s.push('[');
                s.push_str(&format_register(shader, rel.reg));
                s.push('.');
                s.push(rel.component.as_char());
                if reg.num != 0 {
                    s.push_str(&format!("+{}", reg.num));
                }
                s.push(']');
            } else {
                s.push_str(&format_register(shader, *reg));
            }

            if !swizzle.is_identity() {
                s.push('.');
                s.push_str(&format_swizzle(*swizzle));
            }

            match modifier {
                SrcModifier::Abs | SrcModifier::AbsNeg => s.push('|'),
                _ => {}
            }

            s
        }
    }
}
