use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use std::env;
use std::process::Command;

const DEFAULT_CASES: usize = 512;
// `xtask conformance` can trigger a cold build of the entire `conformance` crate and its
// dependencies. On shared CI runners / fresh workspaces this can exceed `safe-run.sh`'s default
// 10 minute timeout, causing the harness to fail before it even starts executing test cases.
//
// Keep this scoped to `xtask conformance` rather than increasing the global `safe-run.sh` default
// timeout; other commands benefit from a shorter "hang" threshold.
const DEFAULT_TIMEOUT_SECS: u64 = 1800;
// Building the conformance harness (and its host reference) can be extremely slow when forced to
// `-j1` (the default under `safe-run.sh`). Use a slightly higher default parallelism so the
// conformance smoke test (and first-run developer experience) doesn't routinely time out on cold
// builds. Keep this conservative for shared/contended hosts.
const DEFAULT_CARGO_BUILD_JOBS: u32 = 2;

#[derive(Debug)]
struct ConformanceOpts {
    cases: usize,
    seed: Option<u64>,
    filter: Option<String>,
    report_path: Option<String>,
    reference_isolate: Option<bool>,
    test_args: Vec<String>,
}

pub fn print_help() {
    println!(
        "\
Run the instruction conformance / differential test harness.

This is a small wrapper around:
  bash ./scripts/safe-run.sh cargo test -p conformance --locked

Usage:
  cargo xtask conformance [options] [-- <test args>]

Options:
  --cases <n>            Number of randomized instruction cases to run.
                         (default: $AERO_CONFORMANCE_CASES or {DEFAULT_CASES})
  --seed <u64>           RNG seed (decimal or 0x... hex).
                         (default: $AERO_CONFORMANCE_SEED or conformance default)
  --filter <expr>        Filter expression (sets AERO_CONFORMANCE_FILTER)
  --report <path>        Alias for --report-path
  --report-path <path>   Write JSON report to this path (AERO_CONFORMANCE_REPORT_PATH)

  --reference-isolate    Force reference backend isolation (AERO_CONFORMANCE_REFERENCE_ISOLATE=1)
  --no-reference-isolate Disable reference backend isolation (AERO_CONFORMANCE_REFERENCE_ISOLATE=0)

  -h, --help             Show this help.

Environment:
  AERO_CONFORMANCE_CASES
  AERO_CONFORMANCE_SEED
  AERO_CONFORMANCE_FILTER
  AERO_CONFORMANCE_REFERENCE
  AERO_CONFORMANCE_REPORT_PATH
  AERO_CONFORMANCE_REFERENCE_ISOLATE

Examples:
  cargo xtask conformance --cases 32
  cargo xtask conformance --cases 5000 --seed 0x52c671d9a4f231b9 --filter add --report target/conformance.json
  cargo xtask conformance --cases 32 -- --nocapture
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    let (args, test_args) = split_args(args);

    // Help should work on any host.
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }

    // The conformance harness compares Aero semantics against native host execution.
    // Running on other hosts would just build and then skip at runtime.
    if !cfg!(all(unix, target_arch = "x86_64")) {
        println!(
            "\
xtask conformance: skipped (supported on unix x86_64 only).

The conformance harness compares Aero semantics against native host execution, which currently
requires a unix x86_64 host."
        );
        return Ok(());
    }

    let mut opts = parse_args(args)?;
    opts.test_args = test_args;

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    let mut cmd = Command::new("bash");
    cmd.current_dir(&repo_root)
        .arg("./scripts/safe-run.sh")
        .arg("cargo")
        .args(["test", "-p", "conformance", "--locked"]);

    // Give conformance runs a more forgiving default timeout on cold builds, while still allowing
    // callers to override via `AERO_TIMEOUT`.
    if env::var_os("AERO_TIMEOUT").is_none() {
        cmd.env("AERO_TIMEOUT", DEFAULT_TIMEOUT_SECS.to_string());
    }

    // `safe-run.sh` defaults to `-j1` for reliability. That's great for keeping sandboxes stable,
    // but it makes the `conformance` crate (which pulls in a full CPU stack + iced-x86) take
    // prohibitively long to build from scratch. Nudge the default up slightly unless the caller
    // has explicitly chosen their own parallelism.
    if env::var_os("AERO_CARGO_BUILD_JOBS").is_none() && env::var_os("CARGO_BUILD_JOBS").is_none() {
        cmd.env(
            "AERO_CARGO_BUILD_JOBS",
            DEFAULT_CARGO_BUILD_JOBS.to_string(),
        );
    }

    // Ensure the child process gets a fully-specified set of conformance knobs.
    cmd.env("AERO_CONFORMANCE_CASES", opts.cases.to_string());

    // Clear vars to avoid surprising inheritance from the parent process. We then re-add
    // the ones selected by CLI args (or explicitly passed through from env).
    cmd.env_remove("AERO_CONFORMANCE_SEED");
    cmd.env_remove("AERO_CONFORMANCE_FILTER");
    cmd.env_remove("AERO_CONFORMANCE_REPORT_PATH");
    cmd.env_remove("AERO_CONFORMANCE_REFERENCE_ISOLATE");

    if let Some(seed) = opts.seed {
        cmd.env("AERO_CONFORMANCE_SEED", seed.to_string());
    }
    if let Some(filter) = &opts.filter {
        cmd.env("AERO_CONFORMANCE_FILTER", filter);
    }
    if let Some(path) = &opts.report_path {
        cmd.env("AERO_CONFORMANCE_REPORT_PATH", path);
    }
    if let Some(isolate) = opts.reference_isolate {
        cmd.env(
            "AERO_CONFORMANCE_REFERENCE_ISOLATE",
            if isolate { "1" } else { "0" },
        );
    }

    if !opts.test_args.is_empty() {
        cmd.arg("--");
        cmd.args(&opts.test_args);
    }

    runner.run_step("Conformance: cargo test -p conformance --locked", &mut cmd)?;
    Ok(())
}

fn parse_args(args: Vec<String>) -> Result<ConformanceOpts> {
    let env_cases = env::var("AERO_CONFORMANCE_CASES")
        .ok()
        .and_then(|v| parse_usize(&v).ok());
    let env_seed = env::var("AERO_CONFORMANCE_SEED")
        .ok()
        .and_then(|v| parse_u64(&v).ok());

    let mut opts = ConformanceOpts {
        cases: env_cases.unwrap_or(DEFAULT_CASES),
        seed: env_seed,
        filter: env_var_nonempty("AERO_CONFORMANCE_FILTER"),
        report_path: env_var_nonempty("AERO_CONFORMANCE_REPORT_PATH"),
        reference_isolate: env::var("AERO_CONFORMANCE_REFERENCE_ISOLATE")
            .ok()
            .map(|v| v != "0"),
        test_args: Vec::new(),
    };

    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--cases" => {
                let raw = next_value(&mut iter, &arg)?;
                opts.cases = parse_usize(&raw).map_err(|_| {
                    XtaskError::Message(format!("invalid --cases value `{raw}` (expected integer)"))
                })?;
            }
            val if val.starts_with("--cases=") => {
                let raw = &val["--cases=".len()..];
                opts.cases = parse_usize(raw).map_err(|_| {
                    XtaskError::Message(format!("invalid --cases value `{raw}` (expected integer)"))
                })?;
            }
            "--seed" => {
                let raw = next_value(&mut iter, &arg)?;
                opts.seed = Some(parse_u64(&raw).map_err(|_| {
                    XtaskError::Message(format!(
                        "invalid --seed value `{raw}` (expected integer: decimal or 0x.. hex)"
                    ))
                })?);
            }
            val if val.starts_with("--seed=") => {
                let raw = &val["--seed=".len()..];
                opts.seed = Some(parse_u64(raw).map_err(|_| {
                    XtaskError::Message(format!(
                        "invalid --seed value `{raw}` (expected integer: decimal or 0x.. hex)"
                    ))
                })?);
            }
            "--filter" => {
                let raw = next_value(&mut iter, &arg)?;
                opts.filter = if raw.trim().is_empty() {
                    None
                } else {
                    Some(raw)
                };
            }
            val if val.starts_with("--filter=") => {
                let raw = val["--filter=".len()..].to_string();
                opts.filter = if raw.trim().is_empty() {
                    None
                } else {
                    Some(raw)
                };
            }
            "--report" | "--report-path" => {
                let raw = next_value(&mut iter, &arg)?;
                opts.report_path = if raw.trim().is_empty() {
                    None
                } else {
                    Some(raw)
                };
            }
            val if val.starts_with("--report=") => {
                let raw = val["--report=".len()..].to_string();
                opts.report_path = if raw.trim().is_empty() {
                    None
                } else {
                    Some(raw)
                };
            }
            val if val.starts_with("--report-path=") => {
                let raw = val["--report-path=".len()..].to_string();
                opts.report_path = if raw.trim().is_empty() {
                    None
                } else {
                    Some(raw)
                };
            }
            "--reference-isolate" => opts.reference_isolate = Some(true),
            "--no-reference-isolate" => opts.reference_isolate = Some(false),
            other => {
                return Err(XtaskError::Message(format!(
                    "unknown argument for `conformance`: `{other}` (run `cargo xtask conformance --help`)"
                )));
            }
        }
    }

    Ok(opts)
}

fn split_args(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    let Some(pos) = args.iter().position(|a| a == "--") else {
        return (args, Vec::new());
    };
    let test_args = args[pos + 1..].to_vec();
    let args = args[..pos].to_vec();
    (args, test_args)
}

fn next_value(
    iter: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
    flag: &str,
) -> Result<String> {
    match iter.next() {
        Some(v) => Ok(v),
        None => Err(XtaskError::Message(format!("{flag} requires a value"))),
    }
}

fn env_var_nonempty(key: &str) -> Option<String> {
    let value = env::var(key).ok()?;
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn parse_usize(raw: &str) -> std::result::Result<usize, ()> {
    let cleaned = raw.trim().replace('_', "");
    cleaned.parse::<usize>().map_err(|_| ())
}

fn parse_u64(raw: &str) -> std::result::Result<u64, ()> {
    let cleaned = raw.trim().replace('_', "");
    let cleaned = cleaned.as_str();
    if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).map_err(|_| ())
    } else {
        cleaned.parse::<u64>().map_err(|_| ())
    }
}
