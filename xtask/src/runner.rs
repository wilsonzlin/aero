use crate::error::{Result, XtaskError};
use std::env;
use std::ffi::OsStr;
use std::io;
use std::process::Command;

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

        let status = cmd.status();

        if self.github_actions {
            println!("::endgroup::");
        }

        let status = status.map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                XtaskError::Message(format!("missing required command: {}", display(cmd.get_program())))
            }
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
