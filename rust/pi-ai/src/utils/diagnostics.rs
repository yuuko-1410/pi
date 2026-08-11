//! Port of `packages/ai/src/utils/diagnostics.ts`.
//!
//! JS inspects `error instanceof Error` at runtime; Rust callers pass either
//! an `std::error::Error` (mapped like the JS Error branch) or a thrown
//! string-like value (mapped like the JS non-Error branch).

use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticErrorInfo {
    pub name: Option<String>,
    pub message: String,
    pub stack: Option<String>,
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssistantMessageDiagnostic {
    pub type_: String,
    pub timestamp: f64,
    pub error: Option<DiagnosticErrorInfo>,
    pub details: Option<Vec<(String, crate::types::JsonValue)>>,
}

impl AssistantMessageDiagnostic {
    pub fn new(
        type_: impl Into<String>,
        error: Option<DiagnosticErrorInfo>,
        details: Option<Vec<(String, crate::types::JsonValue)>>,
    ) -> Self {
        Self {
            type_: type_.into(),
            timestamp: now_ms(),
            error,
            details,
        }
    }
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

/// Mirrors `formatThrownValue` for a non-Error value.
pub fn format_thrown_value(value: &dyn Display) -> String {
    value.to_string()
}

/// Mirrors `extractDiagnosticError` for an `Error`-like value.
pub fn extract_diagnostic_error(error: &dyn std::error::Error) -> DiagnosticErrorInfo {
    let message = error.to_string();
    DiagnosticErrorInfo {
        name: Some("Error".to_string()),
        message,
        stack: None,
        code: None,
    }
}

/// Mirrors `extractDiagnosticError` for a thrown (non-Error) value.
pub fn extract_thrown_diagnostic(value: &dyn Display) -> DiagnosticErrorInfo {
    DiagnosticErrorInfo {
        name: Some("ThrownValue".to_string()),
        message: format_thrown_value(value),
        stack: None,
        code: None,
    }
}

/// Mirrors `createAssistantMessageDiagnostic`.
pub fn create_assistant_message_diagnostic(
    type_: impl Into<String>,
    error: Option<DiagnosticErrorInfo>,
    details: Option<Vec<(String, crate::types::JsonValue)>>,
) -> AssistantMessageDiagnostic {
    AssistantMessageDiagnostic::new(type_, error, details)
}

/// Mirrors `appendAssistantMessageDiagnostic` (Rust returns the new list).
pub fn append_assistant_message_diagnostic(
    diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    diagnostic: AssistantMessageDiagnostic,
) -> Vec<AssistantMessageDiagnostic> {
    let mut result = diagnostics.unwrap_or_default();
    result.push(diagnostic);
    result
}
