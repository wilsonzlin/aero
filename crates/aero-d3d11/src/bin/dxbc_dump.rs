use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use aero_d3d11::sm4::decode::{decode_decl, decode_instruction};
use aero_d3d11::sm4::opcode::{
    CUSTOMDATA_CLASS_COMMENT, CUSTOMDATA_CLASS_IMMEDIATE_CONSTANT_BUFFER, OPCODE_CUSTOMDATA,
    OPCODE_EXTENDED_BIT, OPCODE_LEN_MASK, OPCODE_LEN_SHIFT, OPCODE_MASK, OPCODE_NOP,
};
use aero_d3d11::sm4::{FOURCC_SHDR, FOURCC_SHEX};
use aero_d3d11::{DxbcFile, Sm4Decl, Sm4Program};
use anyhow::{bail, Context};

const DEFAULT_HEAD_DWORDS: usize = 32;
const DEFAULT_MAX_TOKEN_DWORDS_PER_OP: usize = 16;
const DECLARATION_OPCODE_MIN: u32 = 0x100;

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
    let mut shader_bytes = shader_chunk.data;
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
    if !shader_bytes.len().is_multiple_of(4) {
        let truncated_len = shader_bytes.len() & !3;
        println!(
            "  (warning: shader chunk length {} is not a multiple of 4; truncating to {} bytes)",
            shader_chunk.data.len(),
            truncated_len
        );
        shader_bytes = &shader_bytes[..truncated_len];
    }

    println!();
    let program = Sm4Program::parse_program_tokens(shader_bytes).with_context(|| {
        format!(
            "failed to parse SM4/SM5 token stream from {}",
            shader_chunk.fourcc
        )
    })?;
    println!(
        "version: stage={:?} model={}.{} (token=0x{:08x})",
        program.stage, program.model.major, program.model.minor, program.tokens[0]
    );
    println!(
        "declared length: {} dwords (available {})",
        program.tokens.len(),
        shader_bytes.len() / 4
    );

    println!();
    println!("opcode stream:");
    println!("  format: <dword_index>: opcode=<id> len=<dwords> token=<opcode_token> <decoded?>");

    let toks = &program.tokens;
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

        // Preserve declaration/instruction splitting rules from the main decoder: `customdata`
        // blocks and `nop`s can appear anywhere and do not terminate the declaration section.
        if opcode == OPCODE_CUSTOMDATA {
            // Custom-data blocks can technically carry extended opcode tokens; skip them to find
            // the class DWORD.
            let mut class_pos = 1usize;
            let mut extended = (opcode_token & OPCODE_EXTENDED_BIT) != 0;
            while extended {
                let Some(ext) = inst_toks.get(class_pos).copied() else {
                    break;
                };
                class_pos += 1;
                extended = (ext & OPCODE_EXTENDED_BIT) != 0;
            }
            let class = inst_toks
                .get(class_pos)
                .copied()
                .unwrap_or(CUSTOMDATA_CLASS_COMMENT);
            let decl = if class == CUSTOMDATA_CLASS_IMMEDIATE_CONSTANT_BUFFER {
                Sm4Decl::ImmediateConstantBuffer {
                    dwords: inst_toks.get(class_pos + 1..).unwrap_or(&[]).to_vec(),
                }
            } else {
                Sm4Decl::CustomData {
                    class,
                    len_dwords: len as u32,
                }
            };
            println!("  => decl {decl:?}");
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
