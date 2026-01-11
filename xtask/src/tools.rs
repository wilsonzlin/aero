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

