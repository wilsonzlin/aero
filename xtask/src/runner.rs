use crate::error::{Result, XtaskError};
use std::env;
use std::ffi::OsStr;
use std::io;
use std::process::Command;
use std::time::Duration;

pub struct Runner {
    github_actions: bool,
}

impl Runner {
    pub fn new() -> Self {
        let github_actions = env::var("GITHUB_ACTIONS")
            .map(|v| v == "true")
            .unwrap_or(false);
        Self { github_actions }
    }

    pub fn run_step(&self, desc: &str, cmd: &mut Command) -> Result<()> {
        println!();
        if self.github_actions {
            println!("::group::{desc}");
        } else {
            println!("==> {desc}");
        }

        let status = {
            let mut attempts = 0u32;
            loop {
                match cmd.status() {
                    Ok(status) => break Ok(status),
                    Err(err) => {
                        // In highly parallel test runners / sandboxed filesystems, we can
                        // occasionally hit `ETXTBUSY` ("Text file busy") when invoking freshly
                        // written shell stubs (used by xtask's own CLI/argv wiring tests).
                        //
                        // Retrying makes these runs robust without affecting normal operation.
                        #[cfg(unix)]
                        let should_retry = err.raw_os_error() == Some(26); // ETXTBUSY ("Text file busy")
                        #[cfg(not(unix))]
                        let should_retry = false;

                        if should_retry && attempts < 3 {
                            attempts += 1;
                            std::thread::sleep(Duration::from_millis(10 * attempts as u64));
                            continue;
                        }

                        break Err(err);
                    }
                }
            }
        };

        if self.github_actions {
            println!("::endgroup::");
        }

        let status = status.map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => XtaskError::Message(format!(
                "missing required command: {}",
                display(cmd.get_program())
            )),
            _ => XtaskError::Message(format!(
                "failed to run {}: {err}",
                display(cmd.get_program())
            )),
        })?;

        if status.success() {
            Ok(())
        } else {
            Err(XtaskError::CommandFailure {
                desc: desc.to_string(),
                code: status.code(),
            })
        }
    }
}

fn display(value: &OsStr) -> String {
    value.to_string_lossy().to_string()
}
