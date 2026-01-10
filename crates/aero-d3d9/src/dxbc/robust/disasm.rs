use std::fmt::Write as _;

use super::ShaderType;

pub fn disassemble_sm2_sm3(shader_type: ShaderType, tokens: &[u32]) -> String {
    let mut out = String::new();

    if tokens.is_empty() {
        out.push_str("<empty>");
        return out;
    }

    let version = tokens[0];
    let major = ((version >> 8) & 0xff) as u8;
    let minor = (version & 0xff) as u8;
    let _ = writeln!(
        out,
        "{}_{}_{} ; {} tokens",
        shader_type.short(),
        major,
        minor,
        tokens.len()
    );
    let _ = writeln!(out, "0000: version 0x{version:08x}");

    let mut i = 1usize;
    while i < tokens.len() {
        let token = tokens[i];
        let opcode = (token & 0xffff) as u16;

        if opcode == 0xffff {
            let _ = writeln!(out, "{:04x}: end", i);
            i += 1;
            continue;
        }

        let mut len = ((token >> 24) & 0x0f) as usize;

        // Comment tokens have a separate length encoding; decode conservatively.
        if opcode == 0xfffe {
            // For D3D9 bytecode, the comment length is stored in bits 16..30 and represents the
            // number of DWORDs that follow the comment token.
            let dwords = ((token >> 16) & 0x7fff) as usize;
            len = dwords.saturating_add(1);
        } else if len == 0 {
            len = 1;
        }

        if i + len > tokens.len() {
            len = tokens.len() - i;
        }

        let op_name = opcode_name(opcode)
            .map(|s| s.to_owned())
            .unwrap_or_else(|| format!("op_{opcode:04x}"));

        let _ = write!(out, "{:04x}: {op_name}", i);
        if len > 1 {
            for (arg_index, &arg) in tokens[i + 1..i + len].iter().enumerate() {
                let sep = if arg_index == 0 { " " } else { ", " };
                let _ = write!(out, "{sep}{}", format_operand(shader_type, arg));
            }
        }
        let _ = writeln!(out, "");

        i += len;
    }

    out
}

fn opcode_name(op: u16) -> Option<&'static str> {
    Some(match op {
        0 => "nop",
        1 => "mov",
        2 => "add",
        3 => "sub",
        4 => "mad",
        5 => "mul",
        6 => "rcp",
        7 => "rsq",
        8 => "dp3",
        9 => "dp4",
        10 => "min",
        11 => "max",
        12 => "slt",
        13 => "sge",
        14 => "exp",
        15 => "log",
        18 => "lrp",
        19 => "frc",
        20 => "m4x4",
        21 => "m4x3",
        22 => "m3x4",
        23 => "m3x3",
        24 => "m3x2",
        27 => "loop",
        29 => "endloop",
        31 => "dcl",
        35 => "abs",
        38 => "rep",
        39 => "endrep",
        40 => "if",
        41 => "ifc",
        42 => "else",
        43 => "endif",
        44 => "break",
        45 => "breakc",
        66 => "texld",
        _ => return None,
    })
}

fn format_operand(shader_type: ShaderType, token: u32) -> String {
    // Try to decode register type/number; if it's not a parameter token, fall back to hex.
    if let Some(reg) = decode_register(shader_type, token) {
        reg
    } else {
        format!("0x{token:08x}")
    }
}

fn decode_register(shader_type: ShaderType, token: u32) -> Option<String> {
    let reg_num = token & 0x7ff;
    let reg_type = ((token >> 28) & 0x7) | ((token >> 8) & 0x18);

    let name = match reg_type {
        0 => format!("r{reg_num}"),
        1 => format!("v{reg_num}"),
        2 => format!("c{reg_num}"),
        3 => match shader_type {
            ShaderType::Vertex => format!("a{reg_num}"),
            ShaderType::Pixel => format!("t{reg_num}"),
        },
        4 => match reg_num {
            0 => "oPos".to_owned(),
            1 => "oFog".to_owned(),
            2 => "oPts".to_owned(),
            _ => format!("oRast{reg_num}"),
        },
        5 => format!("oD{reg_num}"),
        6 => format!("oT{reg_num}"),
        7 => format!("i{reg_num}"),
        8 => format!("oC{reg_num}"),
        9 => "oDepth".to_owned(),
        10 => format!("s{reg_num}"),
        14 => format!("b{reg_num}"),
        15 => "aL".to_owned(),
        18 => format!("l{reg_num}"),
        19 => format!("p{reg_num}"),
        _ => return None,
    };

    Some(name)
}
