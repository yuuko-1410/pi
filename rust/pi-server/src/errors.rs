//! Pi server errors, port of `packages/server/src/errors.ts`.

use pi_protocol::cbor::Value;

pub const INTERNAL_SERVER_ERROR_MESSAGE: &str = "Internal server error";
pub const NOT_IMPLEMENTED_MESSAGE: &str = "Operation is not implemented";

/// A service/runtime error that can safely cross the protocol boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct PiServerError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl PiServerError {
    pub fn new(code: &str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            details,
        }
    }
    pub fn not_found() -> Self {
        Self::new("not_found", "Session was not found", None)
    }
}

impl std::fmt::Display for PiServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for PiServerError {}

pub fn session_busy_error(details: Option<Value>) -> PiServerError {
    PiServerError::new("busy", "Session is busy", details)
}

pub fn session_locked_error(details: Option<Value>) -> PiServerError {
    PiServerError::new("session_locked", "Session is locked", details)
}

pub fn session_not_found_error(details: Option<Value>) -> PiServerError {
    PiServerError::new("not_found", "Session was not found", details)
}

pub fn not_implemented_error() -> PiServerError {
    PiServerError::new("not_implemented", NOT_IMPLEMENTED_MESSAGE, None)
}
