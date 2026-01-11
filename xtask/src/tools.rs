use std::path::Path;
use std::process::Command;

pub fn npm() -> Command {
    // On Windows, npm is typically installed as `npm.cmd` (a cmd shim), and spawning `npm`
    // directly via `CreateProcess` does not resolve `.cmd` via PATHEXT.
    if cfg!(windows) {
        Command::new("npm.cmd")
    } else {
        Command::new("npm")
    }
}

pub fn check_node_version(repo_root: &Path) -> Command {
    let script = repo_root.join("scripts/check-node-version.mjs");
    let mut cmd = Command::new("node");
    cmd.current_dir(repo_root).arg(script);
    cmd
}
