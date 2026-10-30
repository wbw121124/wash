// Unified error type for wash.
//
// Phase 0 keeps it simple: a human-readable message plus an exit code. As we
// approach bash compatibility, the message format will be aligned with bash
// ("bash: line N: ...") so our oracle tests stay comparable.

use std::fmt;

#[derive(Debug, Clone)]
pub struct WashError {
    pub message: String,
    #[allow(dead_code)]
    pub exit_code: i32,
}

pub type Result<T> = std::result::Result<T, WashError>;

impl WashError {
    pub fn new(message: impl Into<String>, exit_code: i32) -> Self {
        WashError {
            message: message.into(),
            exit_code,
        }
    }

    pub fn syntax(message: impl Into<String>) -> Self {
        WashError::new(message, 2)
    }
}

impl fmt::Display for WashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WashError {}
