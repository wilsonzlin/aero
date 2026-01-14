use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
struct ForbiddenApiUse {
    path: PathBuf,
    line: usize,
    pattern: &'static str,
}

/// Mask Rust comments + string literals so simple substring checks don't produce false positives
/// (or false negatives due to `//` inside string literals like URLs).
///
/// This is not a full Rust lexer; it only understands enough to keep the wasm-check guardrail
/// stable and low-maintenance:
/// - Line comments (`// ...`)
/// - Nested block comments (`/* ... */`)
/// - Normal string literals (`"..."`, `b"..."`)
/// - Raw string literals (`r#"..."#`, `br#"..."#`)
///
/// Everything inside comments/strings is replaced with spaces, except newlines which are preserved
/// to keep line numbers stable.
fn mask_rust_for_scan(src: &str) -> String {
    #[derive(Clone, Copy, Debug)]
    enum Mode {
        Code,
        LineComment,
        BlockComment { depth: u32 },
        String { escaped: bool },
        RawString { hashes: usize },
    }

    let bytes = src.as_bytes();
    let mut out = Vec::<u8>::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut mode = Mode::Code;

    while i < bytes.len() {
        let b = bytes[i];
        match mode {
            Mode::Code => {
                // Line/block comments.
                if b == b'/' && i + 1 < bytes.len() {
                    let next = bytes[i + 1];
                    if next == b'/' {
                        out.extend_from_slice(b"  ");
                        i += 2;
                        mode = Mode::LineComment;
                        continue;
                    }
                    if next == b'*' {
                        out.extend_from_slice(b"  ");
                        i += 2;
                        mode = Mode::BlockComment { depth: 1 };
                        continue;
                    }
                }

                // Raw strings: r"..." / r#"..."# / br"..." etc.
                if b == b'r' || b == b'b' {
                    let mut start = i;
                    if b == b'b' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'r' {
                            start = i + 1;
                        } else if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                            // Byte string literal: b"..."
                            out.extend_from_slice(b"b\"");
                            i += 2;
                            mode = Mode::String { escaped: false };
                            continue;
                        }
                    }

                    if bytes[start] == b'r' && start + 1 < bytes.len() {
                        let mut j = start + 1;
                        let mut hashes = 0usize;
                        while j < bytes.len() && bytes[j] == b'#' {
                            hashes += 1;
                            j += 1;
                        }
                        if j < bytes.len() && bytes[j] == b'"' {
                            // Emit the raw string prefix + delimiter as-is so the output remains
                            // roughly source-shaped.
                            out.extend_from_slice(&bytes[i..j + 1]);
                            i = j + 1;
                            mode = Mode::RawString { hashes };
                            continue;
                        }
                    }
                }

                // Normal string literal.
                if b == b'"' {
                    out.push(b'"');
                    i += 1;
                    mode = Mode::String { escaped: false };
                    continue;
                }

                out.push(b);
                i += 1;
            }
            Mode::LineComment => {
                if b == b'\n' {
                    out.push(b'\n');
                    i += 1;
                    mode = Mode::Code;
                } else {
                    out.push(b' ');
                    i += 1;
                }
            }
            Mode::BlockComment { depth } => {
                if b == b'\n' {
                    out.push(b'\n');
                    i += 1;
                    continue;
                }
                if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    out.extend_from_slice(b"  ");
                    i += 2;
                    mode = Mode::BlockComment { depth: depth + 1 };
                    continue;
                }
                if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    out.extend_from_slice(b"  ");
                    i += 2;
                    if depth == 1 {
                        mode = Mode::Code;
                    } else {
                        mode = Mode::BlockComment { depth: depth - 1 };
                    }
                    continue;
                }
                out.push(b' ');
                i += 1;
            }
            Mode::String { escaped } => {
                if b == b'\n' {
                    out.push(b'\n');
                    i += 1;
                    mode = Mode::String { escaped: false };
                    continue;
                }
                if escaped {
                    out.push(b' ');
                    i += 1;
                    mode = Mode::String { escaped: false };
                    continue;
                }
                if b == b'\\' {
                    out.push(b' ');
                    i += 1;
                    mode = Mode::String { escaped: true };
                    continue;
                }
                if b == b'"' {
                    out.push(b'"');
                    i += 1;
                    mode = Mode::Code;
                    continue;
                }
                out.push(b' ');
                i += 1;
            }
            Mode::RawString { hashes } => {
                if b == b'\n' {
                    out.push(b'\n');
                    i += 1;
                    continue;
                }
                if b == b'"' {
                    // Potential raw string terminator: "### (hashes copies).
                    let mut ok = true;
                    for j in 0..hashes {
                        if i + 1 + j >= bytes.len() || bytes[i + 1 + j] != b'#' {
                            ok = false;
                            break;
                        }
                    }
                    if ok {
                        out.push(b'"');
                        out.extend(std::iter::repeat_n(b'#', hashes));
                        i += 1 + hashes;
                        mode = Mode::Code;
                        continue;
                    }
                }
                out.push(b' ');
                i += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).to_string()
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|e| {
        XtaskError::Message(format!(
            "wasm-check: failed to read dir {}: {e}",
            paths::display_rel_path(dir)
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            XtaskError::Message(format!(
                "wasm-check: failed to read dir entry in {}: {e}",
                paths::display_rel_path(dir)
            ))
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
            continue;
        }
        if path.extension() == Some(OsStr::new("rs")) {
            out.push(path);
        }
    }
    Ok(())
}

fn check_aero_devices_gpu_no_host_only_apis(repo_root: &Path) -> Result<()> {
    // `aero-devices-gpu` is used by both native hosts and the browser runtime. Some "host-like"
    // std APIs technically compile on wasm32 but rely on JS shims or have semantics that do not
    // match Aero's externally supplied deterministic time base.
    //
    // Guardrail: ensure the core device model sources do not use obviously host-only APIs.
    // (Tests may still use them behind cfg gates.)
    let src_dir = repo_root.join("crates/aero-devices-gpu/src");
    if !src_dir.is_dir() {
        return Err(XtaskError::Message(format!(
            "wasm-check: expected directory missing: {}",
            paths::display_rel_path(&src_dir)
        )));
    }

    // Keep the list small to avoid false positives.
    const FORBIDDEN: &[&str] = &[
        "std::time::Instant",
        "Instant::now",
        "std::thread",
        "std::fs",
    ];

    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files)?;
    files.sort();

    let mut hits: Vec<ForbiddenApiUse> = Vec::new();
    for file in files {
        let contents = fs::read_to_string(&file).map_err(|e| {
            XtaskError::Message(format!(
                "wasm-check: failed to read {}: {e}",
                paths::display_rel_path(&file)
            ))
        })?;
        let masked = mask_rust_for_scan(&contents);
        for (idx, line) in masked.lines().enumerate() {
            for &needle in FORBIDDEN {
                if line.contains(needle) {
                    hits.push(ForbiddenApiUse {
                        path: file.clone(),
                        line: idx + 1,
                        pattern: needle,
                    });
                }
            }
        }
    }

    if hits.is_empty() {
        return Ok(());
    }

    let mut msg = String::new();
    msg.push_str(
        "wasm-check: forbidden host-only std APIs detected in aero-devices-gpu sources:\n",
    );
    for hit in hits {
        msg.push_str(&format!(
            "- {}:{}: `{}`\n",
            paths::display_rel_path(&hit.path),
            hit.line,
            hit.pattern
        ));
    }
    msg.push_str("\nThis crate is used by the browser runtime; keep the default feature set free of host-only APIs (Instant/threads/file I/O).\n");
    Err(XtaskError::Message(msg))
}

pub fn print_help() {
    println!(
        "\
Compile-check wasm32 compatibility for selected crates.

Usage:
  cargo xtask wasm-check [--locked]

Currently checked:
  - aero-devices-gpu
  - aero-wasm (standalone, to avoid feature unification masking)
  - aero-machine + aero-wasm (together)
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }
    let mut force_locked = false;
    for arg in args {
        match arg.as_str() {
            "--locked" => force_locked = true,
            other => {
                return Err(XtaskError::Message(format!(
                    "unexpected argument for `wasm-check`: `{other}` (run `cargo xtask wasm-check --help`)"
                )));
            }
        }
    }

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    check_aero_devices_gpu_no_host_only_apis(&repo_root)?;

    let cargo_locked = force_locked || repo_root.join("Cargo.lock").is_file();
    {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .arg("check")
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("-p")
            .arg("aero-devices-gpu");
        if cargo_locked {
            cmd.arg("--locked");
        }
        runner.run_step("Rust: cargo check (wasm32, aero-devices-gpu)", &mut cmd)?;
    }

    {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .arg("check")
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("-p")
            .arg("aero-wasm");
        if cargo_locked {
            cmd.arg("--locked");
        }
        runner.run_step("Rust: cargo check (wasm32, aero-wasm)", &mut cmd)?;
    }

    {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .arg("check")
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("-p")
            .arg("aero-machine")
            .arg("-p")
            .arg("aero-wasm");
        if cargo_locked {
            cmd.arg("--locked");
        }
        runner.run_step(
            "Rust: cargo check (wasm32, aero-machine, aero-wasm)",
            &mut cmd,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::mask_rust_for_scan;

    #[test]
    fn mask_rust_for_scan_removes_comment_and_string_contents() {
        let src = r#"
// std::time::Instant
let a = "Instant::now";
/* nested /* std::fs */ comment */
let ok = 123;
"#;
        let masked = mask_rust_for_scan(src);
        assert!(!masked.contains("std::time::Instant"));
        assert!(!masked.contains("Instant::now"));
        assert!(!masked.contains("std::fs"));
        assert!(masked.contains("let ok = 123;"));
    }

    #[test]
    fn mask_rust_for_scan_does_not_treat_double_slash_in_strings_as_comments() {
        let src = r#"let url = "http://example.com"; std::thread::spawn(|| {});"#;
        let masked = mask_rust_for_scan(src);
        // Ensure the forbidden pattern after the string is still visible (so the scan can find it).
        assert!(masked.contains("std::thread::spawn"));
        // Ensure the string literal contents were masked out.
        assert!(!masked.contains("http://example.com"));
    }

    #[test]
    fn mask_rust_for_scan_handles_raw_strings_and_block_comments() {
        let src = r##"
let s = r#"std::fs"#;
/* std::thread */
let x = 0;
"##;
        let masked = mask_rust_for_scan(src);
        assert!(!masked.contains("std::fs"));
        assert!(!masked.contains("std::thread"));
        assert!(masked.contains("let x = 0;"));
    }
}
