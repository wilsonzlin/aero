use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use crate::tools;
use std::env;
use std::path::Path;
use std::process::Command;

#[derive(Default, Debug)]
struct InputOpts {
    e2e: bool,
    machine: bool,
    wasm: bool,
    rust_only: bool,
    with_wasm: bool,
    pw_extra_args: Vec<String>,
}

// Keep this list intentionally small to stay in-scope for input/USB changes and to keep CI/dev
// runs fast.
const INPUT_BATCH_MALFORMED_SPEC: &str = "tests/e2e/input_batch_malformed.spec.ts";
const INPUT_E2E_SPECS: &[&str] = &[
    "tests/e2e/input_capture.spec.ts",
    "tests/e2e/input_capture_io_worker.spec.ts",
    INPUT_BATCH_MALFORMED_SPEC,
    "tests/e2e/io_worker_i8042.spec.ts",
    "tests/e2e/io_worker_input_telemetry_drop_counter.spec.ts",
    "tests/e2e/scancodes.spec.ts",
    "tests/e2e/usb_hid_bridge.spec.ts",
    "tests/e2e/virtio_input_backend_switch_keyboard.spec.ts",
    "tests/e2e/virtio_input_backend_switch_keyboard_held.spec.ts",
    "tests/e2e/virtio_input_backend_switch_mouse.spec.ts",
    "tests/e2e/virtio_input_backend_switch_mouse_held.spec.ts",
    "tests/e2e/workers_panel_input_capture.spec.ts",
];

pub fn print_help() {
    println!(
        "\
Run the USB/input-focused test suite (Rust + web) with one command.

Usage:
  cargo xtask input [--e2e] [--machine] [--wasm] [--rust-only] [--with-wasm] [-- <extra playwright args>]

Steps:
  1. cargo test -p aero-devices-input --locked
  2. cargo test -p aero-usb --locked
  3. (optional: --machine) cargo test -p aero-machine --lib --locked
  4. (optional: --wasm) wasm-pack test --node crates/aero-wasm --test webusb_uhci_bridge --locked
  5. (optional: --with-wasm) cargo test -p aero-wasm --test machine_input_backends --locked
  6. (unless --rust-only) npm -w web run test:unit -- src/input
  7. (optional: --e2e, unless --rust-only) npm run test:e2e -- <input-related specs...>
     (defaults to --project=chromium --workers=1; sets AERO_WASM_PACKAGES=core unless already set)

Options:
  --e2e                 Also run a small subset of Playwright E2E tests relevant to input.
  --machine             Also run `aero-machine` unit tests (covers snapshot + device integration).
  --wasm                Also run wasm-pack tests for the WASM USB bridge (does not require `node_modules`).
  --rust-only            Only run the Rust input/USB tests (skips Node + Playwright).
  --with-wasm            Also run the `aero-wasm` input backend integration smoke test.
  -- <args>             Extra Playwright args forwarded to `npm run test:e2e` (requires --e2e).
  -h, --help            Show this help.

Examples:
  cargo xtask input
  cargo xtask input --rust-only
  cargo xtask input --machine
  cargo xtask input --wasm --rust-only
  cargo xtask input --with-wasm
  cargo xtask input --rust-only --with-wasm
  cargo xtask input --e2e
  cargo xtask input --e2e -- --project=chromium
  cargo xtask input --e2e -- --project=chromium --workers=4
  cargo xtask input --e2e -- --project=chromium --project=firefox --project=webkit
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    let Some(opts) = parse_args(args)? else {
        return Ok(());
    };

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&repo_root)
        .args(["test", "-p", "aero-devices-input", "--locked"]);
    runner.run_step("Rust: cargo test -p aero-devices-input --locked", &mut cmd)?;

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&repo_root)
        .args(["test", "-p", "aero-usb", "--locked"]);
    runner.run_step("Rust: cargo test -p aero-usb --locked", &mut cmd)?;

    if opts.machine {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .args(["test", "-p", "aero-machine", "--lib", "--locked"]);
        runner.run_step("Rust: cargo test -p aero-machine --lib --locked", &mut cmd)?;
    }

    if opts.with_wasm {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root).args([
            "test",
            "-p",
            "aero-wasm",
            "--test",
            "machine_input_backends",
            "--locked",
        ]);
        runner.run_step(
            "Rust: cargo test -p aero-wasm --test machine_input_backends --locked",
            &mut cmd,
        )?;
    }

    let needs_node = opts.wasm || !opts.rust_only;
    if needs_node {
        let mut cmd = tools::check_node_version(&repo_root);
        runner.run_step("Node: check version", &mut cmd)?;
    }

    if opts.wasm {
        let mut cmd = Command::new("wasm-pack");
        cmd.current_dir(&repo_root).args([
            "test",
            "--node",
            "crates/aero-wasm",
            "--test",
            "webusb_uhci_bridge",
            "--locked",
        ]);
        runner.run_step(
            "WASM: wasm-pack test --node crates/aero-wasm --test webusb_uhci_bridge --locked",
            &mut cmd,
        )?;
    }

    if opts.rust_only {
        println!();
        println!("==> Rust-only input test steps passed.");
        return Ok(());
    }

    // `npm ci` from the repo root installs workspace deps under `./node_modules/`, but some
    // setups may install within `web/` directly. Accept either so `cargo xtask input` can still
    // provide a helpful missing-deps hint without being overly strict about layout.
    let has_node_modules =
        repo_root.join("node_modules").is_dir() || repo_root.join("web/node_modules").is_dir();
    if !has_node_modules {
        return Err(XtaskError::Message(
            "node_modules is missing; install Node dependencies first (e.g. `npm ci`), \
             or run `cargo xtask input --rust-only` to skip Node/Playwright"
                .to_string(),
        ));
    }

    let mut cmd = tools::npm();
    cmd.current_dir(&repo_root)
        .args(["-w", "web", "run", "test:unit", "--", "src/input"]);
    runner.run_step("Web: npm -w web run test:unit -- src/input", &mut cmd)?;

    if opts.e2e {
        let step_desc = format!(
            "E2E: npm run test:e2e ({}) -- <input specs>",
            e2e_step_detail(&opts.pw_extra_args)
        );
        let mut cmd = build_e2e_cmd(&repo_root, &opts.pw_extra_args);
        runner.run_step(&step_desc, &mut cmd)?;
    }

    println!();
    println!("==> Input test steps passed.");
    Ok(())
}

fn parse_args(args: Vec<String>) -> Result<Option<InputOpts>> {
    let mut opts = InputOpts::default();
    let mut iter = args.into_iter().peekable();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "--e2e" => opts.e2e = true,
            "--machine" => opts.machine = true,
            "--wasm" => opts.wasm = true,
            "--rust-only" => opts.rust_only = true,
            "--with-wasm" => opts.with_wasm = true,
            "--" => {
                opts.pw_extra_args = iter.collect();
                break;
            }
            other => {
                return Err(XtaskError::Message(format!(
                    "unknown argument for `input`: `{other}` (run `cargo xtask input --help`)"
                )));
            }
        }
    }

    if opts.rust_only && opts.e2e {
        return Err(XtaskError::Message(
            "--rust-only cannot be combined with --e2e".to_string(),
        ));
    }
    if !opts.e2e && !opts.pw_extra_args.is_empty() {
        return Err(XtaskError::Message(
            "extra Playwright args after `--` require `--e2e`".to_string(),
        ));
    }

    Ok(Some(opts))
}

fn build_e2e_cmd(repo_root: &Path, pw_extra_args: &[String]) -> Command {
    let mut cmd = tools::npm();
    cmd.current_dir(repo_root).args(["run", "test:e2e", "--"]);

    // Playwright runs trigger `pretest:e2e`, which builds the web WASM bundles. The input/USB E2E
    // subset only needs the core `aero-wasm` package, so avoid building unrelated packages unless
    // the caller has already configured their own package selection.
    if !env_var_nonempty("AERO_WASM_PACKAGES") {
        cmd.env("AERO_WASM_PACKAGES", "core");
    }

    // Default to Chromium unless the caller has already selected Playwright projects.
    if !pw_extra_args
        .iter()
        .any(|arg| arg == "--project" || arg.starts_with("--project="))
    {
        cmd.arg("--project=chromium");
    }
    // Default to a single worker for reliability in constrained environments.
    if !pw_extra_args
        .iter()
        .any(|arg| arg == "--workers" || arg.starts_with("--workers="))
    {
        cmd.arg("--workers=1");
    }
    cmd.args(INPUT_E2E_SPECS);
    // Developers can add extra Playwright args after `--`.
    cmd.args(pw_extra_args);
    cmd
}

fn e2e_step_detail(pw_extra_args: &[String]) -> String {
    let project_detail = if pw_extra_args
        .iter()
        .any(|arg| arg == "--project" || arg.starts_with("--project="))
    {
        "projects=custom".to_string()
    } else {
        "project=chromium".to_string()
    };

    let wasm_detail = if env_var_nonempty("AERO_WASM_PACKAGES") {
        "AERO_WASM_PACKAGES=custom".to_string()
    } else {
        "AERO_WASM_PACKAGES=core".to_string()
    };

    let workers_detail = if pw_extra_args
        .iter()
        .any(|arg| arg == "--workers" || arg.starts_with("--workers="))
    {
        "workers=custom".to_string()
    } else {
        "workers=1".to_string()
    };

    format!("{project_detail}, {workers_detail}, {wasm_detail}")
}

fn env_var_nonempty(key: &str) -> bool {
    match env::var(key) {
        Ok(value) => !value.trim().is_empty(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn parse_args_rejects_rust_only_with_e2e() {
        let err = parse_args(vec!["--rust-only".into(), "--e2e".into()])
            .expect_err("expected parse_args to reject incompatible flags");
        assert!(
            err.to_string()
                .contains("--rust-only cannot be combined with --e2e"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn parse_args_rejects_extra_playwright_args_without_e2e() {
        let err = parse_args(vec!["--".into(), "--project=chromium".into()])
            .expect_err("expected parse_args to reject stray Playwright args");
        assert!(
            err.to_string()
                .contains("extra Playwright args after `--` require `--e2e`"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn parse_args_accepts_with_wasm() {
        let opts = parse_args(vec!["--with-wasm".into()]).expect("parse_args").expect("opts");
        assert!(opts.with_wasm);
        assert!(!opts.rust_only);
        assert!(!opts.e2e);
    }

    #[test]
    fn curated_e2e_specs_include_input_batch_malformed() {
        assert!(
            INPUT_E2E_SPECS.contains(&INPUT_BATCH_MALFORMED_SPEC),
            "expected input_batch_malformed spec to be part of the input e2e subset"
        );
    }

    #[test]
    fn cmd_input_source_mentions_malformed_spec_once() {
        let src_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd_input.rs");
        let src =
            fs::read_to_string(&src_path).unwrap_or_else(|err| panic!("read {src_path:?}: {err}"));
        let occurrences = src.matches(INPUT_BATCH_MALFORMED_SPEC).count();
        assert_eq!(
            occurrences, 1,
            "expected {INPUT_BATCH_MALFORMED_SPEC} to appear exactly once in {src_path:?}"
        );
    }

    #[test]
    fn e2e_cmd_appends_extra_args_after_curated_specs() {
        let extra_args = vec!["--project=chromium".to_string()];
        let cmd = build_e2e_cmd(Path::new("."), &extra_args);
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let spec_start = args
            .iter()
            .position(|arg| arg == INPUT_E2E_SPECS[0])
            .expect("expected curated specs to be present in the command args");
        for (i, spec) in INPUT_E2E_SPECS.iter().enumerate() {
            assert_eq!(args[spec_start + i], *spec);
        }

        assert_eq!(
            args[spec_start + INPUT_E2E_SPECS.len()],
            "--project=chromium"
        );
    }

    #[test]
    fn e2e_cmd_defaults_to_chromium_when_no_projects_specified() {
        let cmd = build_e2e_cmd(Path::new("."), &[]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let project_pos = args
            .iter()
            .position(|arg| arg == "--project=chromium")
            .expect("expected --project=chromium to be present by default");
        let spec_pos = args
            .iter()
            .position(|arg| arg == INPUT_E2E_SPECS[0])
            .expect("expected curated specs to be present in the command args");
        assert!(
            project_pos < spec_pos,
            "expected --project=chromium to appear before curated spec paths"
        );
    }

    #[test]
    fn e2e_cmd_defaults_to_one_worker_when_no_workers_specified() {
        let cmd = build_e2e_cmd(Path::new("."), &[]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let workers_pos = args
            .iter()
            .position(|arg| arg == "--workers=1")
            .expect("expected --workers=1 to be present by default");
        let spec_pos = args
            .iter()
            .position(|arg| arg == INPUT_E2E_SPECS[0])
            .expect("expected curated specs to be present in the command args");
        assert!(
            workers_pos < spec_pos,
            "expected --workers=1 to appear before curated spec paths"
        );
    }

    #[test]
    fn input_e2e_specs_are_deduped() {
        let mut seen = HashSet::new();
        for &spec in INPUT_E2E_SPECS {
            assert!(
                seen.insert(spec),
                "duplicate Playwright spec path in INPUT_E2E_SPECS: {spec}"
            );
        }
    }

    #[test]
    fn curated_e2e_specs_keep_malformed_batch_near_io_worker_input_specs() {
        fn idx(spec: &str) -> usize {
            INPUT_E2E_SPECS
                .iter()
                .position(|&s| s == spec)
                .unwrap_or_else(|| panic!("expected {spec} to exist in INPUT_E2E_SPECS"))
        }

        let capture_io_worker = idx("tests/e2e/input_capture_io_worker.spec.ts");
        let malformed_batch = idx(INPUT_BATCH_MALFORMED_SPEC);
        let i8042 = idx("tests/e2e/io_worker_i8042.spec.ts");

        assert!(
            capture_io_worker < malformed_batch && malformed_batch < i8042,
            "expected {INPUT_BATCH_MALFORMED_SPEC} to stay adjacent to IO-worker input specs \
             (after input_capture_io_worker and before io_worker_i8042)"
        );
    }
}
