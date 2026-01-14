use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use crate::tools;
use std::path::Path;
use std::process::Command;

#[derive(Default)]
struct InputOpts {
    e2e: bool,
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
  cargo xtask input [--e2e] [-- <extra playwright args>]

Steps:
  1. cargo test -p aero-devices-input --locked
  2. cargo test -p aero-usb --locked
  3. npm -w web run test:unit -- src/input
  4. (optional) npm run test:e2e -- <input-related specs...>

Options:
  --e2e                 Also run a small subset of Playwright E2E tests relevant to input.
  -h, --help            Show this help.

Examples:
  cargo xtask input
  cargo xtask input --e2e
  cargo xtask input --e2e -- --project=chromium
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

    let mut cmd = tools::check_node_version(&repo_root);
    runner.run_step("Node: check version", &mut cmd)?;

    // `npm ci` from the repo root installs workspace deps under `./node_modules/`, but some
    // setups may install within `web/` directly. Accept either so `cargo xtask input` can still
    // provide a helpful missing-deps hint without being overly strict about layout.
    let has_node_modules =
        repo_root.join("node_modules").is_dir() || repo_root.join("web/node_modules").is_dir();
    if !has_node_modules {
        return Err(XtaskError::Message(
            "node_modules is missing; install Node dependencies first (e.g. `npm ci`)".to_string(),
        ));
    }

    let mut cmd = tools::npm();
    cmd.current_dir(&repo_root)
        .args(["-w", "web", "run", "test:unit", "--", "src/input"]);
    runner.run_step("Web: npm -w web run test:unit -- src/input", &mut cmd)?;

    if opts.e2e {
        let mut cmd = build_e2e_cmd(&repo_root, &opts.pw_extra_args);
        runner.run_step("E2E: npm run test:e2e -- <input specs>", &mut cmd)?;
    } else if !opts.pw_extra_args.is_empty() {
        return Err(XtaskError::Message(
            "extra Playwright args after `--` require `--e2e`".to_string(),
        ));
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

    Ok(Some(opts))
}

fn build_e2e_cmd(repo_root: &Path, pw_extra_args: &[String]) -> Command {
    let mut cmd = tools::npm();
    cmd.current_dir(repo_root).args(["run", "test:e2e", "--"]);
    cmd.args(INPUT_E2E_SPECS);
    // Developers can add extra Playwright args after `--`.
    cmd.args(pw_extra_args);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn curated_e2e_specs_include_input_batch_malformed() {
        assert!(
            INPUT_E2E_SPECS.contains(&INPUT_BATCH_MALFORMED_SPEC),
            "expected input_batch_malformed spec to be part of the input e2e subset"
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
        let malformed_batch = idx("tests/e2e/input_batch_malformed.spec.ts");
        let i8042 = idx("tests/e2e/io_worker_i8042.spec.ts");

        assert!(
            capture_io_worker < malformed_batch && malformed_batch < i8042,
            "expected input_batch_malformed.spec.ts to stay adjacent to IO-worker input specs \
             (after input_capture_io_worker and before io_worker_i8042)"
        );
    }
}
