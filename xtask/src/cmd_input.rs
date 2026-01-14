use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use crate::tools;
use std::env;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

#[derive(Default, Debug)]
struct InputOpts {
    e2e: bool,
    machine: bool,
    wasm: bool,
    with_wasm: bool,
    usb_all: bool,
    rust_only: bool,
    node_dir: Option<String>,
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

const AERO_USB_FOCUSED_TESTS: &[&str] = &[
    "uhci",
    "uhci_external_hub",
    "ehci",
    "ehci_ports",
    "ehci_snapshot_roundtrip",
    "usb2_companion_routing",
    "usb2_port_mux_speed",
    "usb2_port_mux_remote_wakeup",
    "usb2_mux_non_owner_writes",
    "hid_remote_wakeup",
    "hid_idle_rate",
    "hid_mouse_report_generation",
    "webusb_passthrough_uhci",
    "hid_builtin_snapshot",
    "hid_composite_mouse_snapshot_compat",
    "hid_mouse_snapshot_compat",
    "hid_configuration_snapshot_clamping",
    "hid_consumer_control_snapshot_clamping",
    "hid_gamepad_snapshot_clamping",
    "hid_keyboard_snapshot_sanitization",
    "hid_keyboard_leds",
    "hid_mouse_report_generation",
    "hid_mouse_snapshot_clamping",
    "usb_hub_snapshot_configuration_clamping",
    "attached_device_snapshot_address_clamping",
    "hid_usage_keyboard_fixture",
    "hid_usage_consumer_fixture",
    "webhid_boot_interface",
    "webhid_passthrough",
    "webhid_report_descriptor_synthesis",
    "xhci_enum_smoke",
    "xhci_port_remote_wakeup",
    "xhci_controller_webusb_ep0",
    "xhci_doorbell0",
    "xhci_stop_endpoint_unschedules",
    "xhci_usbcmd_run_gates_transfers",
    "xhci_webusb_passthrough",
];

const AERO_MACHINE_FOCUSED_TESTS: &[&str] = &[
    "machine_i8042_snapshot_pending_bytes",
    "machine_input_batch_ps2_to_usb_backend_switch",
    "machine_input_batch_mouse_all_released_capped",
    "machine_input_batch_consumer_control_backend_switch",
    "machine_input_batch_usb_keyboard_unconfigured",
    "machine_input_batch_usb_mouse_unconfigured",
    "machine_input_batch_usb_gamepad_unconfigured",
    "machine_input_batch_usb_consumer_control_unconfigured",
    "machine_virtio_input",
    "virtio_input_event_delivery",
    "machine_keyboard_backend_switch",
    "machine_mouse_backend_switch",
    "machine_uhci",
    "uhci_snapshot",
    "machine_uhci_snapshot_roundtrip",
    "uhci_usb_topology_api",
    "machine_usb_attach_at_path",
    "machine_ehci",
    "machine_usb2_companion_routing",
    "machine_uhci_synthetic_usb_hid",
    "machine_uhci_synthetic_hid",
    "machine_uhci_synthetic_usb_hid_mouse_buttons",
    "machine_uhci_synthetic_usb_hid_gamepad",
    "machine_uhci_synthetic_usb_hid_reports",
    "machine_xhci",
    "machine_xhci_snapshot",
    "xhci_snapshot",
    "machine_xhci_usb_attach_at_path",
    "usb_snapshot_host_state",
];

const WASM_PACK_TESTS: &[&str] = &[
    "webusb_uhci_bridge",
    "uhci_controller_topology",
    "uhci_runtime_webusb",
    "uhci_runtime_webusb_drain_actions",
    "uhci_runtime_topology",
    "uhci_runtime_external_hub",
    "uhci_runtime_snapshot_roundtrip",
    "ehci_controller_bridge_snapshot_roundtrip",
    "ehci_controller_topology",
    "webusb_ehci_passthrough_harness",
    "xhci_webusb_bridge",
    "xhci_controller_bridge",
    "xhci_controller_bridge_topology",
    "xhci_controller_bridge_webusb",
    "xhci_controller_topology",
    "xhci_topology",
    "xhci_step_frames_clamp",
    "xhci_step_frames_clamping",
    "xhci_bme_event_ring",
    "xhci_webusb_snapshot",
    "xhci_snapshot",
    "usb_bridge_snapshot_roundtrip",
    "usb_snapshot",
    "machine_input_injection_wasm",
    "wasm_machine_ps2_mouse",
    "usb_hid_bridge_keyboard_reports_wasm",
    "usb_hid_bridge_mouse_reports_wasm",
    "usb_hid_bridge_consumer_reports_wasm",
    "webhid_interrupt_out_policy_wasm",
    "webhid_report_descriptor_synthesis_wasm",
];

const AERO_WASM_INPUT_TESTS: &[&str] = &[
    "machine_input_injection",
    "machine_input_backends",
    "machine_defaults_usb_hid",
    "webhid_report_descriptor_synthesis",
    "machine_virtio_input",
];

const WEB_UNIT_TEST_PATHS: &[&str] = &[
    "src/input",
    "src/hid",
    "src/hid/hid_report_ring.test.ts",
    "src/hid/wasm_hid_guest_bridge.test.ts",
    "src/hid/wasm_uhci_hid_guest_bridge.test.ts",
    "src/io/devices/virtio_input_mouse_buttons.test.ts",
    "src/ui/input_diagnostics_panel.test.ts",
    "src/ui/settings_panel.test.ts",
    "src/platform/features.test.ts",
    "src/platform/hid_passthrough_protocol.test.ts",
    "src/platform/webhid_passthrough.test.ts",
    "src/platform/webhid_passthrough_broker.test.ts",
    "src/platform/webusb_protection.test.ts",
    "src/platform/webusb_troubleshooting.test.ts",
    "src/runtime/wasm_loader_uhci_runtime_webhid_types.test.ts",
    "src/runtime/wasm_loader_uhci_runtime_webusb_types.test.ts",
    "src/runtime/wasm_loader_usb_snapshot_types.test.ts",
    "src/runtime/wasm_loader_worker_vm_snapshot_types.test.ts",
    "src/workers/input_batch_recycle_guard.test.ts",
    "src/workers/io_hid_input_ring.test.ts",
    "src/workers/io_hid_output_report_forwarding.test.ts",
    "src/workers/io_hid_passthrough.test.ts",
    "src/workers/io_hid_passthrough_legacy_adapter.test.ts",
    "src/workers/io_hid_topology_mux.test.ts",
    "src/workers/io_input_batch.test.ts",
    "src/workers/io_virtio_input_register.test.ts",
    "src/workers/io_webusb_guest_selection.test.ts",
    "src/workers/io_xhci_init.test.ts",
    "src/workers/machine_cpu.worker_threads.test.ts",
    "src/workers/uhci_runtime_hub_config.test.ts",
    "src/workers/io_worker_vm_snapshot.test.ts",
    "src/workers/usb_snapshot_container.test.ts",
    "src/workers/vm_snapshot_wasm.test.ts",
    "src/usb/usb_guest_controller.test.ts",
    "src/usb/usb_broker.test.ts",
    "src/usb/usb_broker_subport.test.ts",
    "src/usb/usb_broker_panel.test.ts",
    "src/usb/usb_hex.test.ts",
    "src/usb/usb_passthrough_demo_runtime.test.ts",
    "src/usb/webusb_backend.test.ts",
    "src/usb/webusb_executor.test.ts",
    "src/usb/webusb_panel.test.ts",
    "src/usb/webusb_passthrough_runtime.test.ts",
    "src/usb/webusb_harness_runtime.test.ts",
    "src/usb/webusb_ehci_harness_runtime.test.ts",
    "src/usb/webusb_uhci_harness_panel.test.ts",
    "src/usb/webhid_passthrough_runtime.test.ts",
    "src/usb/hid_report_ring.test.ts",
    "src/usb/usb_proxy_protocol.test.ts",
    "src/usb/usb_proxy_ring.test.ts",
    "src/usb/usb_proxy_ring_dispatcher.test.ts",
    "src/usb/usb_proxy_ring_integration.test.ts",
    "src/usb/xhci_webusb_bridge.test.ts",
    "src/usb/xhci_webusb_passthrough_runtime.test.ts",
    "src/usb/uhci_machine_topology_rust_drift.test.ts",
    "src/usb/uhci_webusb_root_port_rust_drift.test.ts",
    "src/usb/ehci_webusb_root_port_rust_drift.test.ts",
    "src/usb/xhci_webusb_root_port_rust_drift.test.ts",
];

pub fn print_help() {
    let aero_usb_focused_flags = format_test_flags(AERO_USB_FOCUSED_TESTS);
    let aero_machine_focused_flags = format_test_flags(AERO_MACHINE_FOCUSED_TESTS);
    let wasm_pack_focused_flags = format_test_flags(WASM_PACK_TESTS);
    let aero_wasm_focused_flags = format_test_flags(AERO_WASM_INPUT_TESTS);
    let web_unit_test_paths = WEB_UNIT_TEST_PATHS.join(" ");

    println!(
        "\
Run the USB/input-focused test suite (Rust + web) with one command.

Usage:
  cargo xtask input [--e2e] [--machine] [--wasm] [--rust-only] [--with-wasm] [--usb-all] [--node-dir <path>] [-- <extra playwright args>]

Steps:
  1. cargo test -p aero-devices-input --locked
  2. cargo test -p aero-usb --locked {aero_usb_focused_flags}
      (or: --usb-all to run the full aero-usb test suite)
  3. (optional: --machine) cargo test -p aero-machine --lib --locked {aero_machine_focused_flags}
  4. (optional: --wasm) wasm-pack test --node crates/aero-wasm {wasm_pack_focused_flags} --locked
  5. (optional: --with-wasm) cargo test -p aero-wasm --locked {aero_wasm_focused_flags}
  6. (unless --rust-only) npm -w web run test:unit -- {web_unit_test_paths}
       (or: set --node-dir/--web-dir web / AERO_NODE_DIR=web (deprecated: AERO_WEB_DIR/WEB_DIR) to run `npm run test:unit` from `web/`)
  7. (optional: --e2e, unless --rust-only) npm run test:e2e -- <input-related specs...>
      (defaults to --project=chromium --workers=1; sets AERO_WASM_PACKAGES=core unless already set)

Options:
  --e2e                 Also run a small subset of Playwright E2E tests relevant to input.
  --machine             Also run targeted `aero-machine` tests (USB: UHCI/EHCI/xHCI + USB2 routing; input: i8042 + virtio-input; plus snapshot/restore).
  --wasm                Also run targeted wasm-pack tests for WASM USB/input regressions (requires Node; does not require `node_modules`).
  --rust-only            Skip npm unit + Playwright steps (does not require `node_modules`).
  --usb-all             Run the full `aero-usb` test suite (all integration tests).
  --with-wasm            Also run host-side `aero-wasm` input integration smoke tests (no wasm-pack; does not require `node_modules`).
  --node-dir <path>     Override the Node workspace directory for the web unit-test step (same as AERO_NODE_DIR / AERO_WEB_DIR / WEB_DIR).
  --web-dir <path>      Alias for --node-dir.
  -- <args>             Extra Playwright args forwarded to `npm run test:e2e` (requires --e2e).
  -h, --help            Show this help.

Environment:
  AERO_NODE_DIR / AERO_WEB_DIR / WEB_DIR
                         Override the Node workspace directory for the web unit-test step.
                         (`AERO_WEB_DIR` and `WEB_DIR` are deprecated aliases.)
                         If set to `web`, step 6 runs `npm run test:unit -- ...` inside `web/`.
  AERO_WASM_PACKAGES    When running `--e2e`, defaults to `core` unless already set.
  AERO_ALLOW_UNSUPPORTED_NODE
                         Set to 1 to bypass Node version enforcement (see `.nvmrc` + `scripts/check-node-version.mjs`).
                         Not recommended, especially with `--wasm` (wasm-pack tooling can hang on unsupported Node majors).

Examples:
  cargo xtask input
  cargo xtask input --rust-only
  cargo xtask input --usb-all
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

    let rust_only_hint = rust_only_hint(&opts);

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&repo_root)
        .args(["test", "-p", "aero-devices-input", "--locked"]);
    runner.run_step("Rust: cargo test -p aero-devices-input --locked", &mut cmd)?;

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&repo_root)
        .args(["test", "-p", "aero-usb", "--locked"]);
    if opts.usb_all {
        runner.run_step("Rust: cargo test -p aero-usb --locked (full)", &mut cmd)?;
    } else {
        for &test in AERO_USB_FOCUSED_TESTS {
            cmd.args(["--test", test]);
        }
        runner.run_step("Rust: cargo test -p aero-usb --locked (focused)", &mut cmd)?;
    }

    if opts.machine {
        // Keep this targeted: `aero-machine` has a large integration test suite (GPU/BIOS/etc).
        // For input/USB changes we only need the unit tests plus the UHCI/EHCI/xHCI integration
        // tests that validate device wiring and snapshot/restore behaviour.
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .args(["test", "-p", "aero-machine", "--lib", "--locked"]);
        for &test in AERO_MACHINE_FOCUSED_TESTS {
            cmd.args(["--test", test]);
        }
        runner.run_step(
            "Rust: cargo test -p aero-machine --lib --locked (focused USB wiring)",
            &mut cmd,
        )?;
    }

    if opts.with_wasm {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .args(["test", "-p", "aero-wasm", "--locked"]);
        for &test in AERO_WASM_INPUT_TESTS {
            cmd.args(["--test", test]);
        }
        runner.run_step(
            "Rust: cargo test -p aero-wasm --locked (focused input integration)",
            &mut cmd,
        )?;
    }

    let needs_node = opts.wasm || !opts.rust_only;
    if needs_node {
        let mut cmd = tools::check_node_version(&repo_root);
        if opts.wasm {
            // wasm-pack/wasm-bindgen tooling is sensitive to Node major versions. Keep `input --wasm`
            // aligned with CI's pinned major to avoid hard-to-debug hangs in unsupported releases.
            cmd.env("AERO_ENFORCE_NODE_MAJOR", "1");
        }
        match runner.run_step("Node: check version", &mut cmd) {
            Ok(()) => {}
            Err(XtaskError::Message(msg)) if msg == "missing required command: node" => {
                if opts.wasm {
                    return Err(XtaskError::Message(
                        "missing required command: node\n\nNode is required for `cargo xtask input --wasm`."
                            .to_string(),
                    ));
                }

                return Err(XtaskError::Message(format!(
                    "missing required command: node\n\nInstall Node, or run `{rust_only_hint}` to skip npm + Playwright."
                )));
            }
            Err(err) => return Err(err),
        }
    }

    if opts.wasm {
        let mut cmd = Command::new("wasm-pack");
        cmd.current_dir(&repo_root)
            .args(["test", "--node", "crates/aero-wasm"]);
        for &test in WASM_PACK_TESTS {
            cmd.args(["--test", test]);
        }
        cmd.arg("--locked");
        let wasm_pack_focused_flags = format_test_flags(WASM_PACK_TESTS);
        let step_desc = format!(
            "WASM: wasm-pack test --node crates/aero-wasm {wasm_pack_focused_flags} --locked"
        );
        runner.run_step(&step_desc, &mut cmd)
            .map_err(|err| match err {
                XtaskError::Message(msg) if msg == "missing required command: wasm-pack" => {
                    XtaskError::Message(
                        "missing required command: wasm-pack\n\nInstall wasm-pack (https://rustwasm.github.io/wasm-pack/installer/) or omit `--wasm`."
                            .to_string(),
                    )
                }
                other => other,
            })?;
    }

    if opts.rust_only {
        println!();
        if opts.wasm {
            println!("==> Input test steps passed (--rust-only; skipped npm + Playwright).");
        } else {
            println!("==> Rust-only input test steps passed.");
        }
        return Ok(());
    }

    let node_dir = resolve_node_dir_for_input(&repo_root, opts.node_dir.as_deref())?;

    // `npm ci` from the repo root installs workspace deps under `./node_modules/`, but some
    // setups may install within `web/` directly. Accept either so `cargo xtask input` can still
    // provide a helpful missing-deps hint without being overly strict about layout.
    let has_node_modules = repo_root.join("node_modules").is_dir()
        || repo_root.join("web/node_modules").is_dir()
        || node_dir.join("node_modules").is_dir();
    if !has_node_modules {
        return Err(XtaskError::Message(format!(
            "node_modules is missing; install Node dependencies first (e.g. `npm ci`), \
                 or run `{rust_only_hint}` to skip npm + Playwright"
        )));
    }

    let mut cmd = tools::npm();
    let step_desc = if node_dir == repo_root {
        cmd.current_dir(&repo_root)
            .args(["-w", "web", "run", "test:unit", "--"]);
        "Web: npm -w web run test:unit -- src/input src/hid src/platform/* src/workers/* (plus WebUSB/WebHID topology guards)"
            .to_string()
    } else {
        cmd.current_dir(&node_dir).args(["run", "test:unit", "--"]);
        let node_dir_display = paths::display_rel_path(&node_dir);
        format!(
            "Web: npm run test:unit -- src/input src/hid src/platform/* src/workers/* (plus WebUSB/WebHID topology guards; node dir: {node_dir_display})"
        )
    };
    cmd.args(WEB_UNIT_TEST_PATHS.iter().copied());
    match runner.run_step(&step_desc, &mut cmd) {
        Ok(()) => {}
        Err(XtaskError::Message(msg)) if msg.starts_with("missing required command: npm") => {
            return Err(XtaskError::Message(format!(
                "{msg}\n\nInstall Node tooling, or run `{rust_only_hint}` to skip npm + Playwright."
            )));
        }
        Err(err) => return Err(err),
    }

    if opts.e2e {
        let step_desc = format!(
            "E2E: npm run test:e2e ({}) -- <input specs>",
            e2e_step_detail(&opts.pw_extra_args)
        );
        let mut cmd = build_e2e_cmd(&repo_root, &opts.pw_extra_args);
        match runner.run_step(&step_desc, &mut cmd) {
            Ok(()) => {}
            Err(XtaskError::Message(msg)) if msg.starts_with("missing required command: npm") => {
                return Err(XtaskError::Message(format!(
                    "{msg}\n\nInstall Node tooling, or run `{rust_only_hint}` to skip npm + Playwright."
                )));
            }
            Err(err) => return Err(err),
        }
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
            "--usb-all" => opts.usb_all = true,
            "--node-dir" | "--web-dir" => {
                opts.node_dir = Some(next_value(&mut iter, &arg)?);
            }
            val if val.starts_with("--node-dir=") => {
                let value = val["--node-dir=".len()..].to_string();
                if value.trim().is_empty() {
                    return Err(XtaskError::Message(
                        "--node-dir requires a value".to_string(),
                    ));
                }
                opts.node_dir = Some(value);
            }
            val if val.starts_with("--web-dir=") => {
                let value = val["--web-dir=".len()..].to_string();
                if value.trim().is_empty() {
                    return Err(XtaskError::Message(
                        "--web-dir requires a value".to_string(),
                    ));
                }
                opts.node_dir = Some(value);
            }
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

fn next_value(
    iter: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
    flag: &str,
) -> Result<String> {
    match iter.next() {
        Some(v) => {
            if v.trim().is_empty() {
                return Err(XtaskError::Message(format!("{flag} requires a value")));
            }
            Ok(v)
        }
        None => Err(XtaskError::Message(format!("{flag} requires a value"))),
    }
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

fn format_test_flags(tests: &[&str]) -> String {
    tests
        .iter()
        .map(|test| format!("--test {test}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolve_node_dir_for_input(repo_root: &Path, cli_override: Option<&str>) -> Result<PathBuf> {
    fn env_nonempty(key: &str) -> Option<String> {
        let value = env::var(key).ok()?;
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    }

    // `cargo xtask test-all` uses `paths::resolve_node_dir`, which can run a Node-based resolver
    // for consistency with CI. `cargo xtask input` intentionally stays sandbox-friendly and avoids
    // requiring Node to execute *additional* detection scripts in test mode, so we resolve the
    // node dir using a simple Rust fallback here.
    let override_dir = cli_override
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .or_else(|| env_nonempty("AERO_NODE_DIR"))
        .or_else(|| env_nonempty("AERO_WEB_DIR"))
        .or_else(|| env_nonempty("WEB_DIR"));
    if let Some(dir) = override_dir {
        let path = PathBuf::from(&dir);
        let resolved = if path.is_absolute() {
            path
        } else {
            repo_root.join(path)
        };
        let resolved = paths::clean_path(&resolved);
        if resolved.join("package.json").is_file() {
            return Ok(resolved);
        }
        return Err(XtaskError::Message(format!(
            "package.json not found in node dir override `{dir}` (set --node-dir/--web-dir or AERO_NODE_DIR/AERO_WEB_DIR/WEB_DIR to a directory that contains package.json)"
        )));
    }

    for candidate in [
        repo_root.to_path_buf(),
        repo_root.join("frontend"),
        repo_root.join("web"),
    ] {
        if candidate.join("package.json").is_file() {
            return Ok(candidate);
        }
    }

    Err(XtaskError::Message(
        "unable to locate package.json; pass --node-dir/--web-dir or set AERO_NODE_DIR/AERO_WEB_DIR/WEB_DIR to the Node workspace directory"
            .to_string(),
    ))
}

fn rust_only_hint(opts: &InputOpts) -> String {
    let mut args = vec!["cargo xtask input".to_string()];
    if opts.usb_all {
        args.push("--usb-all".into());
    }
    if opts.machine {
        args.push("--machine".into());
    }
    if opts.wasm {
        args.push("--wasm".into());
    }
    if opts.with_wasm {
        args.push("--with-wasm".into());
    }
    if let Some(node_dir) = &opts.node_dir {
        args.push("--node-dir".into());
        args.push(node_dir.clone());
    }
    args.push("--rust-only".into());
    args.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    fn assert_deduped(label: &str, items: &[&'static str]) {
        assert!(!items.is_empty(), "{label} should not be empty");
        let mut seen = HashSet::new();
        for &item in items {
            assert!(seen.insert(item), "duplicate entry in {label}: {item}");
        }
    }

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
    fn parse_args_accepts_wasm_flag() {
        let opts = parse_args(vec!["--wasm".into()])
            .expect("parse_args should accept --wasm")
            .expect("expected opts for non-help invocation");
        assert!(opts.wasm);
    }

    #[test]
    fn parse_args_accepts_with_wasm() {
        let opts = parse_args(vec!["--with-wasm".into()])
            .expect("parse_args")
            .expect("opts");
        assert!(opts.with_wasm);
        assert!(!opts.rust_only);
        assert!(!opts.e2e);
    }

    #[test]
    fn parse_args_accepts_usb_all_flag() {
        let opts = parse_args(vec!["--usb-all".into()])
            .expect("parse_args should accept --usb-all")
            .expect("expected Some(opts)");
        assert!(opts.usb_all, "expected usb_all to be true");
    }

    #[test]
    fn parse_args_accepts_node_dir_flag() {
        let opts = parse_args(vec!["--node-dir".into(), "web".into()])
            .expect("parse_args should accept --node-dir")
            .expect("expected Some(opts)");
        assert_eq!(opts.node_dir.as_deref(), Some("web"));
    }

    #[test]
    fn parse_args_accepts_node_dir_equals_form() {
        let opts = parse_args(vec!["--node-dir=web".into()])
            .expect("parse_args should accept --node-dir=web")
            .expect("expected Some(opts)");
        assert_eq!(opts.node_dir.as_deref(), Some("web"));
    }

    #[test]
    fn parse_args_accepts_web_dir_alias() {
        let opts = parse_args(vec!["--web-dir=web".into()])
            .expect("parse_args should accept --web-dir=web")
            .expect("expected Some(opts)");
        assert_eq!(opts.node_dir.as_deref(), Some("web"));
    }

    #[test]
    fn parse_args_rejects_empty_node_dir_equals() {
        let err = parse_args(vec!["--node-dir=".into()])
            .expect_err("expected parse_args to reject empty --node-dir value");
        assert!(
            err.to_string().contains("--node-dir requires a value"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn parse_args_rejects_empty_web_dir_equals() {
        let err = parse_args(vec!["--web-dir=".into()])
            .expect_err("expected parse_args to reject empty --web-dir value");
        assert!(
            err.to_string().contains("--web-dir requires a value"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn parse_args_rejects_blank_node_dir_value() {
        let err = parse_args(vec!["--node-dir".into(), " ".into()])
            .expect_err("expected parse_args to reject blank --node-dir value");
        assert!(
            err.to_string().contains("--node-dir requires a value"),
            "unexpected error message: {err}"
        );
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

        // Derive the file name from the curated spec path so we don't introduce a second literal
        // occurrence of the spec's basename in this source file.
        let spec_basename = INPUT_BATCH_MALFORMED_SPEC
            .rsplit('/')
            .next()
            .expect("spec path should have at least one segment");
        let occurrences = src.matches(spec_basename).count();
        assert_eq!(
            occurrences, 1,
            "expected {spec_basename} to appear exactly once in {src_path:?}"
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
    fn focused_xtask_input_lists_are_deduped() {
        assert_deduped("AERO_USB_FOCUSED_TESTS", AERO_USB_FOCUSED_TESTS);
        assert!(
            AERO_USB_FOCUSED_TESTS.contains(&"usb2_companion_routing"),
            "expected usb2_companion_routing to remain part of the focused aero-usb subset"
        );
        assert!(
            AERO_USB_FOCUSED_TESTS.contains(&"webusb_passthrough_uhci"),
            "expected webusb_passthrough_uhci to remain part of the focused aero-usb subset"
        );
        assert!(
            AERO_USB_FOCUSED_TESTS.contains(&"xhci_webusb_passthrough"),
            "expected xhci_webusb_passthrough to remain part of the focused aero-usb subset"
        );
        assert!(
            AERO_USB_FOCUSED_TESTS.contains(&"xhci_usbcmd_run_gates_transfers"),
            "expected xhci_usbcmd_run_gates_transfers to remain part of the focused aero-usb subset"
        );
        assert!(
            AERO_USB_FOCUSED_TESTS.contains(&"xhci_stop_endpoint_unschedules"),
            "expected xhci_stop_endpoint_unschedules to remain part of the focused aero-usb subset"
        );

        assert_deduped("AERO_MACHINE_FOCUSED_TESTS", AERO_MACHINE_FOCUSED_TESTS);
        assert_deduped("WASM_PACK_TESTS", WASM_PACK_TESTS);
        assert_deduped("AERO_WASM_INPUT_TESTS", AERO_WASM_INPUT_TESTS);
        assert_deduped("WEB_UNIT_TEST_PATHS", WEB_UNIT_TEST_PATHS);
    }

    #[test]
    fn focused_xtask_input_references_exist_on_disk() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/CARGO_MANIFEST_DIR should have a parent");

        fn assert_integration_tests_exist(repo_root: &Path, crate_dir: &str, tests: &[&str]) {
            for &test in tests {
                let path = repo_root
                    .join(crate_dir)
                    .join("tests")
                    .join(format!("{test}.rs"));
                assert!(
                    path.is_file(),
                    "expected integration test target `{test}` to exist at {path:?}"
                );
            }
        }

        assert_integration_tests_exist(repo_root, "crates/aero-usb", AERO_USB_FOCUSED_TESTS);
        assert_integration_tests_exist(
            repo_root,
            "crates/aero-machine",
            AERO_MACHINE_FOCUSED_TESTS,
        );
        assert_integration_tests_exist(repo_root, "crates/aero-wasm", AERO_WASM_INPUT_TESTS);
        assert_integration_tests_exist(repo_root, "crates/aero-wasm", WASM_PACK_TESTS);

        for &path in WEB_UNIT_TEST_PATHS {
            let full = repo_root.join("web").join(path);
            assert!(
                full.exists(),
                "expected web unit test path `{path}` to exist at {full:?}"
            );
        }

        for &spec in INPUT_E2E_SPECS {
            let full = repo_root.join(spec);
            assert!(
                full.is_file(),
                "expected Playwright spec `{spec}` to exist at {full:?}"
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
