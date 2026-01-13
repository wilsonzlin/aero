use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use aero_d3d11::sm4::decode::{decode_decl, decode_instruction};
use aero_d3d11::sm4::opcode::{
    OPCODE_CUSTOMDATA, OPCODE_LEN_MASK, OPCODE_LEN_SHIFT, OPCODE_MASK, OPCODE_NOP,
};
use aero_d3d11::sm4::{FOURCC_SHDR, FOURCC_SHEX};
use aero_d3d11::{DxbcFile, Sm4Program};
use anyhow::{bail, Context};

const DEFAULT_HEAD_DWORDS: usize = 32;
const DEFAULT_MAX_TOKEN_DWORDS_PER_OP: usize = 16;
const DECLARATION_OPCODE_MIN: u32 = 0x100;
const CUSTOMDATA_CLASS_COMMENT: u32 = 0;

fn usage() -> &'static str {
    "\
dxbc_dump: dump DXBC chunk structure and SM4/SM5 token streams

USAGE:
    cargo run -p aero-d3d11 --bin dxbc_dump -- <path.dxbc> [--head N] [--full-tokens]

FLAGS:
    --head N          Number of DWORDs to print from the start of the shader chunk (default 32)
    --full-tokens     Print full token lists for each opcode (default: truncate long ops)
"
}

fn main() {
    if let Err(err) = real_main() {
        eprintln!("error: {err:#}");
        process::exit(1);
    }
}

fn real_main() -> anyhow::Result<()> {
    let mut path: Option<PathBuf> = None;
    let mut head_dwords = DEFAULT_HEAD_DWORDS;
    let mut full_tokens = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{}", usage());
                return Ok(());
            }
            "--head" => {
                let Some(v) = args.next() else {
                    bail!("--head requires a value");
                };
                head_dwords = v
                    .parse::<usize>()
                    .with_context(|| format!("invalid --head value {v:?}"))?;
            }
            "--full-tokens" => full_tokens = true,
            _ if arg.starts_with("--head=") => {
                let v = &arg["--head=".len()..];
                head_dwords = v
                    .parse::<usize>()
                    .with_context(|| format!("invalid --head value {v:?}"))?;
            }
            _ if arg.starts_with('-') => {
                bail!("unknown option {arg:?}\n\n{}", usage());
            }
            _ => {
                if path.is_some() {
                    bail!("unexpected positional argument {arg:?}\n\n{}", usage());
                }
                path = Some(PathBuf::from(arg));
            }
        }
    }

    let Some(path) = path else {
        bail!("missing DXBC input path\n\n{}", usage());
    };

    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let dxbc = DxbcFile::parse(&bytes)
        .with_context(|| format!("failed to parse {} as DXBC", path.display()))?;

    let header = dxbc.header();
    println!(
        "DXBC total_size={} chunk_count={} checksum={:02x?}",
        header.total_size, header.chunk_count, header.checksum
    );
    println!("chunks:");
    for (idx, chunk) in dxbc.chunks().enumerate() {
        println!("  [{idx:02}] {} {} bytes", chunk.fourcc, chunk.data.len());
    }

    let shader_chunk = dxbc
        .get_chunk(FOURCC_SHEX)
        .or_else(|| dxbc.get_chunk(FOURCC_SHDR))
        .or_else(|| dxbc.find_first_shader_chunk())
        .context("DXBC is missing SHDR/SHEX shader chunk")?;

    println!();
    println!(
        "shader chunk: {} ({} bytes)",
        shader_chunk.fourcc,
        shader_chunk.data.len()
    );
    println!("first {head_dwords} dwords:");
    for (idx, dword_bytes) in shader_chunk
        .data
        .chunks_exact(4)
        .take(head_dwords)
        .enumerate()
    {
        let v = u32::from_le_bytes([
            dword_bytes[0],
            dword_bytes[1],
            dword_bytes[2],
            dword_bytes[3],
        ]);
        println!("  [{idx:04}] 0x{v:08x}");
    }
    if !shader_chunk.data.len().is_multiple_of(4) {
        println!(
            "  (note: shader chunk length {} is not a multiple of 4; trailing bytes ignored)",
            shader_chunk.data.len()
        );
    }

    println!();
    let program = Sm4Program::parse_program_tokens(shader_chunk.data).with_context(|| {
        format!(
            "failed to parse SM4/SM5 token stream from {}",
            shader_chunk.fourcc
        )
    })?;
    println!(
        "version: stage={:?} model={}.{} (token=0x{:08x})",
        program.stage,
        program.model.major,
        program.model.minor,
        program.tokens.get(0).copied().unwrap_or(0)
    );

    let declared_len_raw = program.tokens.get(1).copied().unwrap_or(0) as usize;
    let declared_len = declared_len_raw.min(program.tokens.len());
    if declared_len != declared_len_raw {
        println!(
            "declared length: {} dwords (clamped from {}, available {})",
            declared_len,
            declared_len_raw,
            program.tokens.len()
        );
    } else {
        println!(
            "declared length: {} dwords (available {})",
            declared_len,
            program.tokens.len()
        );
    }

    if declared_len < 2 {
        println!("token stream too short to contain any opcodes");
        return Ok(());
    }

    println!();
    println!("opcode stream:");
    println!("  format: <dword_index>: opcode=<id> len=<dwords> token=<opcode_token> <decoded?>");

    let toks = &program.tokens[..declared_len];
    let mut i = 2usize;
    let mut in_decls = true;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & OPCODE_MASK;
        let len = ((opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;

        print!("  {i:04}: opcode={opcode:04x} len={len:04} token=0x{opcode_token:08x}",);

        if len == 0 {
            println!("  !! invalid length 0");
            break;
        }

        let end = match i.checked_add(len) {
            Some(v) => v,
            None => {
                println!("  !! length overflow");
                break;
            }
        };
        if end > toks.len() {
            println!(
                "  !! instruction overruns token stream (end={end}, available={})",
                toks.len()
            );
            break;
        }

        let inst_toks = &toks[i..end];

        // Preserve declaration/instruction splitting rules from the main decoder: comment blocks and
        // nops can appear anywhere and do not terminate the declaration section.
        let is_comment = opcode == OPCODE_CUSTOMDATA
            && inst_toks.get(1).copied() == Some(CUSTOMDATA_CLASS_COMMENT);
        if is_comment {
            println!("  ; customdata(comment)");
        } else if opcode == OPCODE_NOP {
            println!("  ; nop");
        } else if in_decls && opcode >= DECLARATION_OPCODE_MIN {
            match decode_decl(opcode, inst_toks, i) {
                Ok(decl) => println!("  => decl {decl:?}"),
                Err(err) => println!("  !! decl decode error: {err}"),
            }
        } else {
            in_decls = false;
            match decode_instruction(opcode, inst_toks, i) {
                Ok(inst) => println!("  => inst {inst:?}"),
                Err(err) => println!("  !! inst decode error: {err}"),
            }
        }

        if full_tokens || len <= DEFAULT_MAX_TOKEN_DWORDS_PER_OP {
            print!("       toks:");
            for &t in inst_toks {
                print!(" 0x{t:08x}");
            }
            println!();
        } else {
            print!("       toks:");
            for &t in &inst_toks[..DEFAULT_MAX_TOKEN_DWORDS_PER_OP] {
                print!(" 0x{t:08x}");
            }
            println!(" ... (+{} dwords)", len - DEFAULT_MAX_TOKEN_DWORDS_PER_OP);
        }

        i = end;
    }

    Ok(())
}
