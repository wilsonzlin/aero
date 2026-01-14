use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use crate::tools;

pub fn print_help() {
    println!(
        "\
Run common web (Node/Vite) workflows.

Usage:
  cargo xtask web <dev|build|preview> [--node-dir <path>] [-- <extra args>]

Options:
  --node-dir <path>     Override the Node workspace directory (contains package.json; same as AERO_NODE_DIR; deprecated aliases: AERO_WEB_DIR, WEB_DIR).
  --web-dir <path>      Alias for --node-dir.
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    let Some(opts) = parse_args(args)? else {
        return Ok(());
    };

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    let mut cmd = tools::check_node_version(&repo_root);
    runner.run_step("Node: check version", &mut cmd)?;

    let node_dir = paths::resolve_node_dir(&repo_root, opts.node_dir.as_deref())?;

    let script = match opts.action.as_str() {
        "dev" | "build" | "preview" => opts.action,
        other => {
            return Err(XtaskError::Message(format!(
                "unknown web action `{other}` (expected: dev|build|preview)"
            )));
        }
    };

    let mut cmd = tools::npm();
    cmd.current_dir(&node_dir).args(["run", &script]);
    if !opts.extra_args.is_empty() {
        cmd.arg("--");
        cmd.args(&opts.extra_args);
    }

    runner.run_step(
        &format!("Web: npm run {script} ({})", node_dir.display()),
        &mut cmd,
    )?;

    Ok(())
}

struct WebOpts {
    action: String,
    node_dir: Option<String>,
    extra_args: Vec<String>,
}

fn parse_args(args: Vec<String>) -> Result<Option<WebOpts>> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(None);
    }

    let mut node_dir: Option<String> = None;
    let mut action: Option<String> = None;
    let mut extra_args: Vec<String> = Vec::new();

    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--node-dir" | "--web-dir" => {
                node_dir = Some(next_value(&mut iter, &arg)?);
            }
            val if val.starts_with("--node-dir=") => {
                node_dir = Some(val["--node-dir=".len()..].to_string());
            }
            val if val.starts_with("--web-dir=") => {
                node_dir = Some(val["--web-dir=".len()..].to_string());
            }
            "--" => {
                extra_args = iter.collect();
                break;
            }
            other if action.is_none() => {
                action = Some(other.to_string());
            }
            other => {
                return Err(XtaskError::Message(format!(
                    "unknown argument for `web`: `{other}` (run `cargo xtask web --help`)"
                )));
            }
        }
    }

    let Some(action) = action else {
        return Err(XtaskError::Message(
            "missing action (expected: dev|build|preview)".to_string(),
        ));
    };

    Ok(Some(WebOpts {
        action,
        node_dir,
        extra_args,
    }))
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
