use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use crate::tools;
use std::env;
use std::path::PathBuf;
use std::process::Command;

#[derive(Default)]
struct TestAllOpts {
    skip_fixtures: bool,
    skip_rust: bool,
    skip_wasm: bool,
    skip_ts: bool,
    skip_e2e: bool,
    require_webgpu: Option<bool>,
    wasm_crate_dir: Option<String>,
    node_dir: Option<String>,
    pw_projects: Vec<String>,
    pw_extra_args: Vec<String>,
}

pub fn print_help() {
    println!(
        "\
Run Aero's full test stack (Rust, WASM, TypeScript, Playwright) with one command.

Usage:
  cargo xtask test-all [options] [-- <extra playwright args>]

Note:
  This command validates deterministic binary fixtures and fails fast if they are
  out of date:

    - cargo xtask fixtures --check   (regenerate via: cargo xtask fixtures)
    - cargo xtask bios-rom --check   (regenerate via: cargo xtask bios-rom)

Options:
  --skip-fixtures       Skip deterministic fixture checks (`cargo xtask fixtures --check`, `cargo xtask bios-rom --check`)
  --skip-rust           Skip Rust checks/tests (cargo fmt/clippy/test)
  --skip-wasm           Skip wasm-pack tests
  --skip-ts             Skip TypeScript unit tests (npm run test:unit)
  --skip-e2e            Skip Playwright smoke tests (npm run test:e2e)

  --webgpu              Run tests that require WebGPU (sets AERO_REQUIRE_WEBGPU=1)
  --no-webgpu           Do not require WebGPU (sets AERO_REQUIRE_WEBGPU=0) [default]

  --wasm-crate-dir <path>
                         Path (relative to repo root or absolute) to the wasm-pack crate dir
                         (defaults to $AERO_WASM_CRATE_DIR or a repo-default like crates/aero-wasm)
  --node-dir <path>     Path (relative to repo root or absolute) containing package.json
                         (defaults to $AERO_NODE_DIR; deprecated aliases: $AERO_WEB_DIR, $WEB_DIR; or an auto-detected location)
  --web-dir <path>      Alias for --node-dir.

  --pw-project <name>   Select a Playwright project (repeatable).
                         Example: --pw-project chromium --pw-project firefox

  -h, --help            Show this help.

Environment:
  AERO_REQUIRE_WEBGPU   If unset, defaults to 0 (to keep CI/dev behavior consistent).
  AERO_WASM_CRATE_DIR   Default wasm-pack crate directory (same as --wasm-crate-dir).
  AERO_NODE_DIR         Default Node workspace directory (same as --node-dir).
  AERO_WEB_DIR / WEB_DIR
                        Deprecated aliases for AERO_NODE_DIR.
  AERO_ISOLATE_CARGO_HOME
                         If set, isolate Cargo state to `./.cargo-home` (or a custom dir) to avoid
                         \"Blocking waiting for file lock on package cache\" on shared hosts.

Examples:
  cargo xtask test-all
  cargo xtask test-all --skip-e2e
  cargo xtask test-all --webgpu --pw-project chromium
  cargo xtask test-all --pw-project chromium -- --grep smoke
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    let Some(opts) = parse_args(args)? else {
        return Ok(());
    };

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    if !opts.skip_fixtures {
        let xtask_exe = std::env::current_exe().map_err(|e| {
            XtaskError::Message(format!(
                "failed to resolve current xtask executable path: {e}"
            ))
        })?;
        let mut cmd = Command::new(&xtask_exe);
        cmd.current_dir(&repo_root).args(["fixtures", "--check"]);
        runner.run_step("Fixtures: cargo xtask fixtures --check", &mut cmd)?;

        let mut cmd = Command::new(&xtask_exe);
        cmd.current_dir(&repo_root).args(["bios-rom", "--check"]);
        runner.run_step("BIOS ROM: cargo xtask bios-rom --check", &mut cmd)?;
    }

    let needs_node = !opts.skip_wasm || !opts.skip_ts || !opts.skip_e2e;
    if needs_node {
        let mut cmd = tools::check_node_version(&repo_root);
        if !opts.skip_wasm {
            // wasm-pack/wasm-bindgen tooling is sensitive to Node major versions. Keep `test-all`
            // aligned with CI's pinned major to avoid hard-to-debug hangs in unsupported releases.
            cmd.env("AERO_ENFORCE_NODE_MAJOR", "1");
        }
        runner.run_step("Node: check version", &mut cmd)?;
    }

    let require_webgpu = match opts.require_webgpu {
        Some(true) => "1".to_string(),
        Some(false) => "0".to_string(),
        None => env::var("AERO_REQUIRE_WEBGPU").unwrap_or_else(|_| "0".to_string()),
    };

    let cargo_locked = repo_root.join("Cargo.lock").is_file();
    let cargo_locked_args: [&str; 1] = ["--locked"];

    if !opts.skip_rust {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .env("AERO_REQUIRE_WEBGPU", &require_webgpu)
            .args(["fmt", "--all", "--", "--check"]);
        runner.run_step("Rust: cargo fmt --all -- --check", &mut cmd)?;

        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .env("AERO_REQUIRE_WEBGPU", &require_webgpu)
            .arg("clippy");
        if cargo_locked {
            cmd.args(cargo_locked_args);
        }
        cmd.args([
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ]);
        runner.run_step("Rust: cargo clippy", &mut cmd)?;

        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .env("AERO_REQUIRE_WEBGPU", &require_webgpu)
            .arg("test");
        if cargo_locked {
            cmd.args(cargo_locked_args);
        }
        cmd.args(["--workspace", "--all-features"]);
        runner.run_step("Rust: cargo test", &mut cmd)?;
    }

    if !opts.skip_wasm {
        let wasm_crate_dir =
            paths::resolve_wasm_crate_dir(&repo_root, opts.wasm_crate_dir.as_deref())?;

        let mut cmd = Command::new("wasm-pack");
        cmd.current_dir(&wasm_crate_dir)
            .env("AERO_REQUIRE_WEBGPU", &require_webgpu)
            .args(["test", "--node", "--"]);
        if cargo_locked {
            cmd.args(cargo_locked_args);
        }
        runner.run_step(
            &format!("WASM: wasm-pack test --node ({})", wasm_crate_dir.display()),
            &mut cmd,
        )?;

        // Compile-check key crates for wasm32 (even though they are not the wasm-pack entrypoint).
        //
        // This catches `Send`/threading bound regressions that can break browser storage backends
        // (e.g. mounting a JS-backed `VirtualDisk` as an ATAPI CD-ROM).
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .env("AERO_REQUIRE_WEBGPU", &require_webgpu)
            .arg("check");
        if cargo_locked {
            cmd.args(cargo_locked_args);
        }
        cmd.args([
            "--target",
            "wasm32-unknown-unknown",
            "-p",
            "aero-devices-storage",
        ]);
        runner.run_step("WASM: cargo check (wasm32) aero-devices-storage", &mut cmd)?;

        // Compile-check wasm32 compatibility for the AeroGPU device integration surface.
        //
        // This is intentionally separated from the wasm-pack entrypoint so we can validate
        // `aero-devices-gpu` + `aero-machine` + `aero-wasm` build for wasm32 without requiring any
        // JS runtime.
        let xtask_exe = std::env::current_exe().map_err(|e| {
            XtaskError::Message(format!(
                "failed to resolve current xtask executable path: {e}"
            ))
        })?;
        let mut cmd = Command::new(&xtask_exe);
        cmd.current_dir(&repo_root)
            .env("AERO_REQUIRE_WEBGPU", &require_webgpu)
            .arg("wasm-check");
        runner.run_step("WASM: cargo xtask wasm-check", &mut cmd)?;
    }

    let mut resolved_node_dir: Option<PathBuf> = None;

    if !opts.skip_ts {
        if resolved_node_dir.is_none() {
            resolved_node_dir = Some(paths::resolve_node_dir(
                &repo_root,
                opts.node_dir.as_deref(),
            )?);
        }
        let node_dir = resolved_node_dir
            .as_ref()
            .expect("node dir should be resolved when TS tests are enabled");

        let mut cmd = tools::npm();
        cmd.current_dir(node_dir)
            .env("AERO_REQUIRE_WEBGPU", &require_webgpu)
            .args(["run", "test:unit"]);
        runner.run_step(
            &format!(
                "TS: npm run test:unit ({}; AERO_REQUIRE_WEBGPU={require_webgpu})",
                node_dir.display()
            ),
            &mut cmd,
        )?;
    }

    if !opts.skip_e2e {
        if resolved_node_dir.is_none() {
            resolved_node_dir = Some(paths::resolve_node_dir(
                &repo_root,
                opts.node_dir.as_deref(),
            )?);
        }
        let node_dir = resolved_node_dir
            .as_ref()
            .expect("node dir should be resolved when E2E tests are enabled");

        let mut cmd = tools::npm();
        cmd.current_dir(node_dir)
            .env("AERO_REQUIRE_WEBGPU", &require_webgpu)
            .args(["run", "test:e2e", "--"]);

        for project in &opts.pw_projects {
            cmd.arg(format!("--project={project}"));
        }
        cmd.args(&opts.pw_extra_args);

        runner.run_step(
            &format!(
                "E2E: npm run test:e2e ({}; AERO_REQUIRE_WEBGPU={require_webgpu})",
                node_dir.display()
            ),
            &mut cmd,
        )?;
    }

    println!();
    println!("==> All requested test steps passed.");
    Ok(())
}

fn parse_args(args: Vec<String>) -> Result<Option<TestAllOpts>> {
    let mut opts = TestAllOpts::default();
    let mut iter = args.into_iter().peekable();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "--skip-fixtures" => opts.skip_fixtures = true,
            "--skip-rust" => opts.skip_rust = true,
            "--skip-wasm" => opts.skip_wasm = true,
            "--skip-ts" | "--skip-unit" => opts.skip_ts = true,
            "--skip-e2e" => opts.skip_e2e = true,
            "--webgpu" | "--require-webgpu" => opts.require_webgpu = Some(true),
            "--no-webgpu" | "--no-require-webgpu" => opts.require_webgpu = Some(false),
            "--wasm-crate-dir" | "--wasm-dir" => {
                opts.wasm_crate_dir = Some(next_value(&mut iter, &arg)?);
            }
            val if val.starts_with("--wasm-crate-dir=") => {
                opts.wasm_crate_dir = Some(val["--wasm-crate-dir=".len()..].to_string());
            }
            val if val.starts_with("--wasm-dir=") => {
                opts.wasm_crate_dir = Some(val["--wasm-dir=".len()..].to_string());
            }
            "--node-dir" | "--web-dir" => {
                opts.node_dir = Some(next_value(&mut iter, &arg)?);
            }
            val if val.starts_with("--node-dir=") => {
                opts.node_dir = Some(val["--node-dir=".len()..].to_string());
            }
            val if val.starts_with("--web-dir=") => {
                opts.node_dir = Some(val["--web-dir=".len()..].to_string());
            }
            "--pw-project" | "--project" => {
                opts.pw_projects.push(next_value(&mut iter, &arg)?);
            }
            val if val.starts_with("--pw-project=") => {
                opts.pw_projects
                    .push(val["--pw-project=".len()..].to_string());
            }
            val if val.starts_with("--project=") => {
                opts.pw_projects.push(val["--project=".len()..].to_string());
            }
            "--" => {
                opts.pw_extra_args = iter.collect();
                break;
            }
            other => {
                return Err(XtaskError::Message(format!(
                    "unknown argument for `test-all`: `{other}` (run `cargo xtask test-all --help`)"
                )));
            }
        }
    }

    Ok(Some(opts))
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
