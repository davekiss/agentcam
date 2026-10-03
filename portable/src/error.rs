use serde::{Deserialize, Serialize};
use std::fmt;

/// The `{"code", "message"}` pair every failure carries, on stdout and over the control socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecError {
    pub code: String,
    pub message: String,
}

impl RecError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        RecError {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn io(context: &str, err: std::io::Error) -> Self {
        RecError::new("io", format!("{context}: {err}"))
    }
}

impl fmt::Display for RecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RecError {}

pub type Result<T> = std::result::Result<T, RecError>;
