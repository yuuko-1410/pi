//! Provider HTTP error normalization, port of
//! `packages/ai/src/utils/error-body.ts`.
//!
//! The JS version probes SDK error objects for status/body fields; Rust
//! models the extracted shape directly (the SDK field probing is a JS
//! runtime concern). `message_carries_body` is caller-supplied.

pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedProviderError {
    /// HTTP status code, when one could be extracted.
    pub status: Option<u16>,
    /// Raw HTTP body reason, already trimmed and truncated to the cap.
    pub body: Option<String>,
    /// `error.message`.
    pub message: String,
    /// True when `message` already contains the body.
    pub message_carries_body: bool,
}

impl NormalizedProviderError {
    /// Mirrors `normalizeProviderError` for an SDK error with known fields.
    pub fn new(message: String, status: Option<u16>, body: Option<String>) -> Self {
        let body = body
            .map(|body| body.trim().to_string())
            .filter(|trimmed| !trimmed.is_empty())
            .map(|trimmed| truncate_error_text(&trimmed, MAX_PROVIDER_ERROR_BODY_CHARS));
        let message_carries_body = match &body {
            None => true,
            Some(body) => message.contains(body.as_str()),
        };
        Self {
            status,
            body,
            message,
            message_carries_body,
        }
    }

    /// Mirrors `normalizeProviderError` for a thrown (non-Error) value: the
    /// message is the stringified value and there is no body to add.
    pub fn thrown(message: String) -> Self {
        Self {
            status: None,
            body: None,
            message,
            message_carries_body: false,
        }
    }
}

/// Compose a display string from a normalized error. When the message
/// already carries the body or no body/status was extracted, the message is
/// returned unchanged. Otherwise the status and body are surfaced, with an
/// optional provider prefix.
///
/// - no prefix: `"<status>: <body>"`
/// - prefix:    `"<prefix> (<status>): <body>"`
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    if norm.message_carries_body || norm.status.is_none() || norm.body.is_none() {
        return match (prefix, norm.status) {
            (Some(prefix), Some(status)) => format!("{prefix} ({status}): {}", norm.message),
            _ => norm.message.clone(),
        };
    }
    let status = norm.status.expect("checked above");
    let body = norm.body.clone().expect("checked above");
    match prefix {
        Some(prefix) => format!("{prefix} ({status}): {body}"),
        None => format!("{status}: {body}"),
    }
}

pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    let extra = text.chars().count() - max_chars;
    format!("{truncated}... [truncated {extra} chars]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_with_and_without_body() {
        let norm = NormalizedProviderError::new(
            "boom".to_string(),
            Some(403),
            Some("{\"error\":\"forbidden\"}".to_string()),
        );
        assert_eq!(format_provider_error(&norm, None), "403: {\"error\":\"forbidden\"}");
        assert_eq!(format_provider_error(&norm, Some("OpenAI")), "OpenAI (403): {\"error\":\"forbidden\"}");
    }

    #[test]
    fn keeps_message_when_it_carries_the_body() {
        let norm = NormalizedProviderError::new(
            "Request failed: {\"error\":\"forbidden\"}".to_string(),
            Some(403),
            Some("{\"error\":\"forbidden\"}".to_string()),
        );
        assert_eq!(
            format_provider_error(&norm, None),
            "Request failed: {\"error\":\"forbidden\"}"
        );
    }

    #[test]
    fn truncates_long_bodies() {
        let long = "x".repeat(5000);
        let norm = NormalizedProviderError::new("err".to_string(), Some(500), Some(long));
        let body = norm.body.expect("body present");
        assert!(body.starts_with(&"x".repeat(4000)));
        assert!(body.ends_with("[truncated 1000 chars]"));
    }
}
