use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use std::process::Command;

#[derive(Default)]
struct ConformanceOpts {
    cases: Option<String>,
    seed: Option<String>,
    filter: Option<String>,
    report: Option<String>,
}

pub fn print_help() {
    println!(
        "\
Run the instruction conformance / differential test harness.

This is a small wrapper around `cargo test -p conformance` that configures the run via
environment variables to keep dev/CI invocations consistent.

Usage:
  cargo xtask conformance [options]

Options:
  --cases <n>            Number of generated test cases (sets AERO_CONFORMANCE_CASES)
  --seed <n>             RNG seed (sets AERO_CONFORMANCE_SEED)
  --filter <expr>        Filter corpus/templates (sets AERO_CONFORMANCE_FILTER)
  --report <path>        Write a JSON report (sets AERO_CONFORMANCE_REPORT_PATH)

  -h, --help             Show this help.

Examples:
  cargo xtask conformance --cases 32
  cargo xtask conformance --cases 5000 --seed 0x52c671d9a4f231b9 --filter add --report target/conformance.json
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    // `cargo xtask conformance` should be safe to run anywhere. The conformance harness relies on
    // native x86_64 host execution (and typically `fork()` isolation), so on unsupported platforms
    // we print a friendly message and exit 0.
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }
    if !cfg!(all(unix, target_arch = "x86_64")) {
        println!(
            "\
xtask conformance: skipped (supported on unix x86_64 only).

The conformance harness compares Aero semantics against native host execution, which currently
requires a unix x86_64 host."
        );
        return Ok(());
    }

    let opts = parse_args(args)?;

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&repo_root)
        .args(["test", "-p", "conformance", "--locked"]);

    if let Some(cases) = opts.cases {
        cmd.env("AERO_CONFORMANCE_CASES", cases);
    }
    if let Some(seed) = opts.seed {
        cmd.env("AERO_CONFORMANCE_SEED", seed);
    }
    if let Some(filter) = opts.filter {
        cmd.env("AERO_CONFORMANCE_FILTER", filter);
    }
    if let Some(report) = opts.report {
        cmd.env("AERO_CONFORMANCE_REPORT_PATH", report);
    }

    runner.run_step("Conformance: cargo test -p conformance --locked", &mut cmd)?;
    Ok(())
}

fn parse_args(args: Vec<String>) -> Result<ConformanceOpts> {
    let mut opts = ConformanceOpts::default();
    let mut iter = args.into_iter().peekable();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--cases" => opts.cases = Some(next_value(&mut iter, &arg)?),
            val if val.starts_with("--cases=") => {
                opts.cases = Some(val["--cases=".len()..].to_string());
            }
            "--seed" => opts.seed = Some(next_value(&mut iter, &arg)?),
            val if val.starts_with("--seed=") => {
                opts.seed = Some(val["--seed=".len()..].to_string());
            }
            "--filter" => opts.filter = Some(next_value(&mut iter, &arg)?),
            val if val.starts_with("--filter=") => {
                opts.filter = Some(val["--filter=".len()..].to_string());
            }
            "--report" | "--report-path" => opts.report = Some(next_value(&mut iter, &arg)?),
            val if val.starts_with("--report=") => {
                opts.report = Some(val["--report=".len()..].to_string());
            }
            val if val.starts_with("--report-path=") => {
                opts.report = Some(val["--report-path=".len()..].to_string());
            }
            other => {
                return Err(XtaskError::Message(format!(
                    "unknown argument for `conformance`: `{other}` (run `cargo xtask conformance --help`)"
                )));
            }
        }
    }

    Ok(opts)
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
