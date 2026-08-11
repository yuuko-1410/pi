//! Assistant-call retry policy, port of `packages/ai/src/utils/retry.ts`.

use regex::Regex;

use crate::types::{AssistantMessage, StopReason};

fn build_provider_error_pattern(patterns: &[&str]) -> Regex {
    regex::RegexBuilder::new(&patterns.join("|"))
        .case_insensitive(true)
        .build()
        .expect("static retry patterns are valid")
}

fn non_retryable_provider_limit_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        build_provider_error_pattern(&[
            // OpenCode Go/free-tier limits returned as 429 JSON error types.
            "GoUsageLimitError",
            "FreeUsageLimitError",
            // OpenCode Go subscription-limit text.
            "Monthly usage limit reached",
            "available balance",
            // Generic quota/budget/billing exhaustion.
            "insufficient_quota",
            "out of budget",
            "quota exceeded",
            "billing",
        ])
    })
}

fn retryable_provider_pattern() -> &'static Regex {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        build_provider_error_pattern(&[
            // Generic provider load, HTTP status, and server-side transients.
            "overloaded",
            "rate.?limit",
            "too many requests",
            "429",
            "500",
            "502",
            "503",
            "504",
            "524",
            "service.?unavailable",
            "server.?error",
            "internal.?error",
            // Wrapper/provider text for transient upstream failures.
            "provider.?returned.?error",
            "exceeded request buffer limit while retrying upstream",
            // Network, proxy, and fetch transport failures.
            "network.?error",
            "connection.?error",
            "connection.?refused",
            "connection.?lost",
            "other side closed",
            "fetch failed",
            "getaddrinfo",
            "ENOTFOUND",
            "EAI_AGAIN",
            "upstream.?connect",
            "reset before headers",
            "socket hang up",
            "socket connection was closed",
            "timed? out",
            "timeout",
            "terminated",
            // WebSocket transports.
            "websocket.?closed",
            "websocket.?error",
            // Premature stream endings.
            "ended without",
            "stream ended before message_stop",
            "stream ended before a terminal response event",
            "http2 request did not get a response",
            // Provider-requested retry delay cap failures.
            "retry delay",
            // Explicit retry guidance emitted mid-stream.
            "you can retry your request",
            "try your request again",
            "please retry your request",
            // gRPC based providers (e.g. NVIDIA NIM).
            "ResourceExhausted",
        ])
    })
}

/// Retry policy: bounded attempts with exponential backoff
/// (`baseDelayMs * 2^(attempt-1)`).
#[derive(Clone, Debug, PartialEq)]
pub struct RetryPolicy {
    pub enabled: bool,
    /// Max retry attempts (0 = no retries). The initial call never counts as
    /// a retry.
    pub max_retries: u64,
    /// Base delay in ms. Per-attempt delay is `baseDelayMs * 2^(attempt-1)`
    /// before jitter.
    pub base_delay_ms: u64,
}

/// Optional callbacks emitted around each retry.
#[derive(Default)]
pub struct RetryCallbacks {
    /// Emitted before the backoff sleep of each retry attempt (1-indexed).
    pub on_retry_scheduled: Option<Box<dyn FnMut(u64, u64, u64, &str)>>,
    /// Emitted after the backoff sleep, immediately before the retried call
    /// starts.
    pub on_retry_attempt_start: Option<Box<dyn FnMut()>>,
    /// Emitted once when the loop ends.
    pub on_retry_finished: Option<Box<dyn FnMut(bool, u64, Option<&str>)>>,
}

/// Run a single assistant-producing call with bounded retry on transient
/// errors. Mirrors `retryAssistantCall`:
/// - success returns immediately; aborts are terminal but reported as
///   unsuccessful if a retry was scheduled;
/// - non-retryable errors return immediately;
/// - otherwise retries up to `max_retries` with exponential backoff,
///   normalizing aborts during the backoff to an aborted AssistantMessage.
///
/// When `policy` is None or disabled, the first response is returned
/// unchanged.
pub fn retry_assistant_call<F>(
    mut produce: F,
    policy: Option<&RetryPolicy>,
    token: Option<&crate::utils::abort::CancellationToken>,
    mut callbacks: Option<RetryCallbacks>,
) -> AssistantMessage
where
    F: FnMut() -> AssistantMessage,
{
    let max_attempts = match policy {
        Some(policy) if policy.enabled => policy.max_retries,
        _ => 0,
    };

    let mut attempt = 0u64;
    let mut last_retry: Option<(u64, String)> = None;
    loop {
        let response = produce();

        // Abort: terminal but not successful. Never retry an aborted message.
        if response.stop_reason == StopReason::Aborted {
            if let Some(callbacks) = callbacks.as_mut() {
                if let Some((attempt, _)) = last_retry {
                    if let Some(on_finished) = callbacks.on_retry_finished.as_mut() {
                        on_finished(false, attempt, None);
                    }
                }
            }
            return response;
        }

        // Success: non-error, non-abort responses return as-is.
        if response.stop_reason != StopReason::Error {
            if let Some(callbacks) = callbacks.as_mut() {
                if let Some((attempt, _)) = last_retry {
                    if let Some(on_finished) = callbacks.on_retry_finished.as_mut() {
                        on_finished(true, attempt, None);
                    }
                }
            }
            return response;
        }

        // Non-retryable, or budget exhausted: return the final error message.
        if attempt >= max_attempts || !is_retryable_assistant_error(&response) {
            if let Some(callbacks) = callbacks.as_mut() {
                if let Some((attempt, _)) = last_retry {
                    if let Some(on_finished) = callbacks.on_retry_finished.as_mut() {
                        on_finished(false, attempt, response.error_message.as_deref());
                    }
                }
            }
            return response;
        }

        attempt += 1;
        let error_message = response
            .error_message
            .clone()
            .unwrap_or_else(|| "Unknown error".to_string());
        last_retry = Some((attempt, error_message.clone()));
        let delay_ms = policy
            .expect("enabled policy above")
            .base_delay_ms
            .saturating_mul(1u64 << (attempt - 1).min(62));

        if let Some(callbacks) = callbacks.as_mut() {
            if let Some(on_scheduled) = callbacks.on_retry_scheduled.as_mut() {
                on_scheduled(attempt, max_attempts, delay_ms, &error_message);
            }
        }

        // Normalize aborts during retry backoff to an aborted AssistantMessage.
        match crate::utils::abort::retry_sleep(delay_ms, token) {
            Ok(()) => {}
            Err(_) => {
                if let Some(callbacks) = callbacks.as_mut() {
                    if let Some(on_finished) = callbacks.on_retry_finished.as_mut() {
                        on_finished(false, attempt, Some(&error_message));
                    }
                }
                let mut aborted = response;
                aborted.stop_reason = StopReason::Aborted;
                aborted.error_message = None;
                return aborted;
            }
        }

        if let Some(callbacks) = callbacks.as_mut() {
            if let Some(on_start) = callbacks.on_retry_attempt_start.as_mut() {
                on_start();
            }
        }
    }
}

/// Classifies whether a failed assistant message looks like a transient
/// provider or transport error.
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error_message) = &message.error_message else {
        return false;
    };
    if non_retryable_provider_limit_pattern().is_match(error_message) {
        return false;
    }
    retryable_provider_pattern().is_match(error_message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Usage, UsageCost};

    fn message(stop_reason: StopReason, error_message: Option<&str>) -> AssistantMessage {
        AssistantMessage {
            content: vec![],
            api: "test".to_string(),
            provider: "test".to_string(),
            model: "m".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 0.0,
                cost: UsageCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason,
            deferred: None,
            error_message: error_message.map(|s| s.to_string()),
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1.0,
        }
    }

    #[test]
    fn classifies_retryable_and_non_retryable_errors() {
        assert!(is_retryable_assistant_error(&message(StopReason::Error, Some("rate limit exceeded"))));
        assert!(is_retryable_assistant_error(&message(StopReason::Error, Some("Connection refused"))));
        assert!(!is_retryable_assistant_error(&message(StopReason::Error, Some("insufficient_quota"))));
        assert!(!is_retryable_assistant_error(&message(StopReason::Error, Some("billing problem"))));
        assert!(!is_retryable_assistant_error(&message(StopReason::Stop, Some("rate limit"))));
        assert!(!is_retryable_assistant_error(&message(StopReason::Error, None)));
    }

    #[test]
    fn returns_first_response_when_policy_disabled() {
        let mut calls = 0;
        let result = retry_assistant_call(
            || {
                calls += 1;
                message(StopReason::Error, Some("rate limit"))
            },
            Some(&RetryPolicy {
                enabled: false,
                max_retries: 3,
                base_delay_ms: 1,
            }),
            None,
            None,
        );
        assert_eq!(calls, 1);
        assert_eq!(result.stop_reason, StopReason::Error);
    }

    #[test]
    fn returns_immediately_on_success_and_abort() {
        let mut calls = 0;
        let policy = Some(&RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 1,
        });
        let result = retry_assistant_call(
            || {
                calls += 1;
                message(StopReason::Stop, None)
            },
            policy,
            None,
            None,
        );
        assert_eq!(calls, 1);
        assert_eq!(result.stop_reason, StopReason::Stop);

        let result = retry_assistant_call(
            || message(StopReason::Aborted, None),
            policy,
            None,
            None,
        );
        assert_eq!(result.stop_reason, StopReason::Aborted);
    }

    #[test]
    fn retries_transient_errors_up_to_budget() {
        let mut calls = 0;
        let result = retry_assistant_call(
            || {
                calls += 1;
                message(StopReason::Error, Some("overloaded"))
            },
            Some(&RetryPolicy {
                enabled: true,
                max_retries: 2,
                base_delay_ms: 1,
            }),
            None,
            None,
        );
        assert_eq!(calls, 3, "initial call + 2 retries");
        assert_eq!(result.stop_reason, StopReason::Error);
    }

    #[test]
    fn retries_then_succeeds() {
        let mut calls = 0;
        let result = retry_assistant_call(
            || {
                calls += 1;
                if calls < 3 {
                    message(StopReason::Error, Some("500"))
                } else {
                    message(StopReason::Stop, None)
                }
            },
            Some(&RetryPolicy {
                enabled: true,
                max_retries: 5,
                base_delay_ms: 1,
            }),
            None,
            None,
        );
        assert_eq!(calls, 3);
        assert_eq!(result.stop_reason, StopReason::Stop);
    }
}
