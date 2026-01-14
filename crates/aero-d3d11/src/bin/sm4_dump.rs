use std::env;
use std::fs;
use std::io::{self, Write};

use anyhow::{bail, Context, Result};

use aero_d3d11::sm4::opcode::{
    OPCODE_EXTENDED_BIT, OPCODE_LEN_MASK, OPCODE_LEN_SHIFT, OPCODE_MASK,
};
use aero_d3d11::sm4::token_dump::tokenize_instructions;
use aero_d3d11::{ShaderStage, Sm4Program};

fn stage_name(stage: ShaderStage) -> String {
    match stage {
        ShaderStage::Vertex => "vertex".to_owned(),
        ShaderStage::Pixel => "pixel".to_owned(),
        ShaderStage::Geometry => "geometry".to_owned(),
        ShaderStage::Hull => "hull".to_owned(),
        ShaderStage::Domain => "domain".to_owned(),
        ShaderStage::Compute => "compute".to_owned(),
        ShaderStage::Unknown(other) => format!("unknown({other})"),
    }
}

fn stage_type(stage: ShaderStage) -> u16 {
    match stage {
        ShaderStage::Pixel => 0,
        ShaderStage::Vertex => 1,
        ShaderStage::Geometry => 2,
        ShaderStage::Hull => 3,
        ShaderStage::Domain => 4,
        ShaderStage::Compute => 5,
        ShaderStage::Unknown(other) => other,
    }
}

fn print_usage() {
    eprintln!("Usage: sm4_dump [--raw] [--json] <path.dxbc>");
    eprintln!();
    eprintln!("  --raw   Only print a hexdump-like list of DWORDs");
    eprintln!("  --json  Emit JSON output (stage/model/tokens/instructions)");
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn print_json(program: &Sm4Program, out: &mut dyn Write) -> Result<()> {
    let declared_len = program.tokens[1] as usize;
    let toks = &program.tokens[..declared_len];
    let insts = tokenize_instructions(&program.tokens)?;

    let stage_name = stage_name(program.stage);
    let stage_ty = stage_type(program.stage);

    writeln!(out, "{{")?;
    writeln!(
        out,
        "  \"stage\": {{ \"type\": {stage_ty}, \"name\": \"{}\" }},",
        json_escape(&stage_name)
    )?;
    writeln!(
        out,
        "  \"model\": {{ \"major\": {}, \"minor\": {} }},",
        program.model.major, program.model.minor
    )?;
    writeln!(out, "  \"declared_length_dwords\": {declared_len},")?;

    // Tokens.
    writeln!(out, "  \"tokens\": [")?;
    for (i, t) in toks.iter().enumerate() {
        let comma = if i + 1 == toks.len() { "" } else { "," };
        writeln!(
            out,
            "    {{ \"index\": {i}, \"value\": \"0x{t:08x}\" }}{comma}"
        )?;
    }
    writeln!(out, "  ],")?;

    // Instruction headers.
    writeln!(out, "  \"instructions\": [")?;
    for (idx, inst) in insts.iter().enumerate() {
        let comma = if idx + 1 == insts.len() { "" } else { "," };
        write!(
            out,
            "    {{ \"start\": {}, \"opcode\": {}, \"len\": {}, \"extended\": {}",
            inst.start,
            inst.opcode,
            inst.len,
            (inst.opcode_token & OPCODE_EXTENDED_BIT) != 0
        )?;

        write!(out, ", \"ext_tokens\": [")?;
        for (j, t) in inst.ext_tokens.iter().enumerate() {
            if j != 0 {
                write!(out, ", ")?;
            }
            write!(out, "\"0x{t:08x}\"")?;
        }
        write!(out, "]")?;

        write!(out, ", \"operand_tokens\": [")?;
        for (j, t) in inst.operand_tokens.iter().enumerate() {
            if j != 0 {
                write!(out, ", ")?;
            }
            write!(out, "\"0x{t:08x}\"")?;
        }
        write!(out, "]")?;

        writeln!(out, " }}{comma}")?;
    }
    writeln!(out, "  ]")?;
    writeln!(out, "}}")?;

    Ok(())
}

fn main_inner() -> Result<()> {
    let mut raw = false;
    let mut json = false;
    let mut path: Option<String> = None;

    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--raw" => raw = true,
            "--json" => json = true,
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            _ if arg.starts_with('-') => bail!("unknown flag: {arg}"),
            _ => {
                if path.is_some() {
                    bail!("unexpected extra argument: {arg}");
                }
                path = Some(arg);
            }
        }
    }

    let path = path.context("missing DXBC file path")?;
    let bytes = fs::read(&path).with_context(|| format!("failed to read {path}"))?;

    let program = Sm4Program::parse_from_dxbc_bytes(&bytes)
        .with_context(|| format!("failed to parse {path} as DXBC shader bytecode"))?;

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    if json {
        print_json(&program, &mut out)?;
        out.flush()?;
        return Ok(());
    }

    let version = program.tokens[0];
    let declared_len = program.tokens[1] as usize;
    let toks = &program.tokens[..declared_len];

    writeln!(
        out,
        "stage: {} (type={})",
        stage_name(program.stage),
        stage_type(program.stage)
    )?;
    writeln!(
        out,
        "model: {}.{}",
        program.model.major, program.model.minor
    )?;
    writeln!(out, "version_token: 0x{version:08x}")?;
    writeln!(out, "declared_length_dwords: {declared_len}")?;
    writeln!(out, "available_dwords: {}", program.tokens.len())?;
    writeln!(out)?;

    writeln!(out, "DWORDS (index -> value):")?;
    for (i, t) in toks.iter().enumerate() {
        writeln!(out, "  {i:04}: 0x{t:08x}")?;
    }
    writeln!(out)?;

    if raw {
        out.flush()?;
        return Ok(());
    }

    writeln!(out, "INSTRUCTIONS (start: opcode len ext operands...):")?;

    let insts = tokenize_instructions(&program.tokens)?;
    for inst in insts {
        let opcode = inst.opcode;
        let len = inst.len;
        let extended = (inst.opcode_token & OPCODE_EXTENDED_BIT) != 0;

        // The decode constants in `sm4::opcode` are useful for exploration, but this tool intentionally
        // prints the raw header fields regardless of whether we know the opcode.
        let len_field = ((inst.opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;
        let opcode_field = inst.opcode_token & OPCODE_MASK;

        write!(
            out,
            "  @{start:04}: opcode={opcode_field:#05x}({opcode}) len={len_field}({len}) ext={extended}",
            start = inst.start
        )?;

        if !inst.ext_tokens.is_empty() {
            write!(out, " ext_toks=[")?;
            for (j, t) in inst.ext_tokens.iter().enumerate() {
                if j != 0 {
                    write!(out, ", ")?;
                }
                write!(out, "0x{t:08x}")?;
            }
            write!(out, "]")?;
        }

        write!(out, " operands=[")?;
        for (j, t) in inst.operand_tokens.iter().enumerate() {
            if j != 0 {
                write!(out, ", ")?;
            }
            write!(out, "0x{t:08x}")?;
        }
        writeln!(out, "]")?;
    }

    out.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    if let Err(err) = main_inner() {
        // Match common CLI behavior: when output is piped to `head` (or any consumer that closes
        // early), stdout writes can fail with `EPIPE`. This should not be treated as an error.
        if err
            .root_cause()
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
        {
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}
