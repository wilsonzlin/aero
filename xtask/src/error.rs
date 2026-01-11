use std::fmt;

#[derive(Debug)]
pub enum XtaskError {
    Message(String),
    CommandFailure { desc: String, code: Option<i32> },
}

impl XtaskError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Message(_) => 1,
            Self::CommandFailure { code, .. } => code.unwrap_or(1),
        }
    }
}

impl fmt::Display for XtaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(msg) => f.write_str(msg),
            Self::CommandFailure { desc, code } => match code {
                Some(code) => write!(f, "{desc} (exit code {code})"),
                None => write!(f, "{desc} (process terminated by signal)"),
            },
        }
    }
}

impl From<String> for XtaskError {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<&str> for XtaskError {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, XtaskError>;
