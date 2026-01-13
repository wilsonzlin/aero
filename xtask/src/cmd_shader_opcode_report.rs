use std::collections::BTreeMap;
use std::fs;

use aero_d3d9::sm3;
use aero_d3d9::sm3::decode::Opcode;
use aero_dxbc::DxbcFile;

use crate::error::{Result, XtaskError};

pub fn print_help() {
    println!(
        "\
Report D3D9 SM2/3 opcode usage (for coverage/telemetry).

Usage:
  cargo xtask shader-opcode-report [--deny-unsupported] <files...>

Input:
  Each file may be either:
    - a DXBC container (\"DXBC\" magic), in which case the first SHDR/SHEX chunk is analyzed
    - a raw D3D9 token stream (little-endian u32 words)

Output:
  Stable text report (suitable for CI artifacts) containing:
    - shader stage/model
    - opcode frequency histogram
    - unsupported opcode list for the current SM2/3 translator

Flags:
  --deny-unsupported, --fail-on-unsupported
      Exit non-zero if any unsupported opcodes are found in any input file.
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }

    let mut deny_unsupported = false;
    let mut files = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--deny-unsupported" | "--fail-on-unsupported" => deny_unsupported = true,
            other if other.starts_with('-') => {
                return Err(XtaskError::Message(format!(
                    "unknown flag for `shader-opcode-report`: `{other}`"
                )))
            }
            _ => files.push(arg),
        }
    }

    if files.is_empty() {
        return Err(XtaskError::Message(
            "usage: cargo xtask shader-opcode-report [--deny-unsupported] <files...>".to_string(),
        ));
    }

    // Sort paths so output is stable regardless of shell glob ordering.
    files.sort();

    println!("shader-opcode-report v1");
    println!("files: {}", files.len());
    println!("deny_unsupported: {deny_unsupported}");
    println!();

    let mut aggregate_opcodes: BTreeMap<u16, u64> = BTreeMap::new();
    let mut aggregate_unsupported: BTreeMap<u16, u64> = BTreeMap::new();
    let mut total_instructions = 0u64;
    let mut files_ok = 0u64;
    let mut files_failed = 0u64;

    let mut any_unsupported = false;
    let mut any_error = false;

    for path in &files {
        println!("file: {path}");
        match analyze_file(path) {
            Ok(report) => {
                files_ok += 1;
                total_instructions += report.instruction_count;
                merge_counts(&mut aggregate_opcodes, &report.opcodes);
                merge_counts(&mut aggregate_unsupported, &report.unsupported_opcodes);

                println!("  source: {}", report.source);
                println!(
                    "  shader: {}",
                    format_shader_version(&report.shader.version)
                );
                println!("  instructions: {}", report.instruction_count);

                println!("  opcodes:");
                print_histogram(&report.opcodes, 4);

                println!("  unsupported_opcodes:");
                if report.unsupported_opcodes.is_empty() {
                    println!("    <none>");
                } else {
                    any_unsupported = true;
                    print_histogram(&report.unsupported_opcodes, 4);
                }
            }
            Err(err) => {
                files_failed += 1;
                any_error = true;
                println!("  error: {err}");
            }
        }
        println!();
    }

    println!("aggregate:");
    println!("  files_ok: {files_ok}");
    println!("  files_failed: {files_failed}");
    println!("  instructions: {total_instructions}");
    println!("  opcodes:");
    print_histogram(&aggregate_opcodes, 4);
    println!("  unsupported_opcodes:");
    if aggregate_unsupported.is_empty() {
        println!("    <none>");
    } else {
        print_histogram(&aggregate_unsupported, 4);
    }

    if any_error {
        return Err(XtaskError::Message(
            "one or more shaders failed to decode".to_string(),
        ));
    }
    if deny_unsupported && any_unsupported {
        return Err(XtaskError::Message("unsupported opcodes found".to_string()));
    }

    Ok(())
}

struct FileReport {
    source: String,
    shader: sm3::DecodedShader,
    instruction_count: u64,
    opcodes: BTreeMap<u16, u64>,
    unsupported_opcodes: BTreeMap<u16, u64>,
}

fn analyze_file(path: &str) -> Result<FileReport> {
    let bytes = fs::read(path).map_err(|e| XtaskError::Message(format!("read {path:?}: {e}")))?;

    let (source, token_bytes) = if bytes.starts_with(b"DXBC") {
        let dxbc = DxbcFile::parse(&bytes)
            .map_err(|e| XtaskError::Message(format!("parse dxbc container: {e}")))?;
        let Some(chunk) = dxbc.find_first_shader_chunk() else {
            return Err(XtaskError::Message(
                "DXBC container missing shader chunk (expected SHDR or SHEX)".to_string(),
            ));
        };
        (
            format!("dxbc (chunk {})", chunk.fourcc),
            chunk.data.to_vec(),
        )
    } else {
        ("raw".to_string(), bytes)
    };

    let shader = sm3::decode_u8_le_bytes(&token_bytes)
        .map_err(|e| XtaskError::Message(format!("sm2/3 decode: {e}")))?;

    let mut opcodes: BTreeMap<u16, u64> = BTreeMap::new();
    let mut unsupported: BTreeMap<u16, u64> = BTreeMap::new();

    for inst in &shader.instructions {
        let raw = inst.opcode.raw();
        *opcodes.entry(raw).or_insert(0) += 1;
        if !sm3_translator_supports_opcode(&inst.opcode) {
            *unsupported.entry(raw).or_insert(0) += 1;
        }
    }

    Ok(FileReport {
        source,
        instruction_count: shader.instructions.len() as u64,
        shader,
        opcodes,
        unsupported_opcodes: unsupported,
    })
}

fn merge_counts(dst: &mut BTreeMap<u16, u64>, src: &BTreeMap<u16, u64>) {
    for (op, count) in src {
        *dst.entry(*op).or_insert(0) += count;
    }
}

fn print_histogram(map: &BTreeMap<u16, u64>, indent: usize) {
    if map.is_empty() {
        println!("{:indent$}<none>", "", indent = indent);
        return;
    }

    let mut entries: Vec<(u16, u64)> = map.iter().map(|(&op, &count)| (op, count)).collect();
    entries.sort_by(|(a_op, a_count), (b_op, b_count)| {
        b_count.cmp(a_count).then_with(|| a_op.cmp(b_op))
    });

    for (op, count) in entries {
        let opcode = Opcode::from_raw(op);
        println!(
            "{:indent$}0x{op:04x} {:<8} {count}",
            "",
            opcode.name(),
            indent = indent
        );
    }
}

fn format_shader_version(v: &sm3::ShaderVersion) -> String {
    let stage = match v.stage {
        sm3::ShaderStage::Vertex => "vs",
        sm3::ShaderStage::Pixel => "ps",
    };
    format!("{stage}_{}_{}", v.major, v.minor)
}

fn sm3_translator_supports_opcode(op: &Opcode) -> bool {
    // For telemetry, treat an opcode as supported if it can be decoded and lowered into the SM2/3
    // IR used by the in-progress translator (`crates/aero-d3d9/src/sm3/*`).
    //
    // This is intentionally conservative: `Unknown` opcodes and control-flow constructs that
    // aren't yet lowered should be surfaced.
    !matches!(
        op,
        Opcode::Call | Opcode::Ret | Opcode::Unknown(_)
    )
}
