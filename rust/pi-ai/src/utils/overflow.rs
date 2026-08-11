//! Context overflow detection, port of `packages/ai/src/utils/overflow.ts`.

use regex::Regex;

use crate::types::{AssistantMessage, StopReason};

fn build_patterns(patterns: &[&str]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|pattern| {
            regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .expect("static overflow pattern is valid")
        })
        .collect()
}

fn overflow_patterns() -> &'static [Regex] {
    static PATTERNS: std::sync::OnceLock<Vec<Regex>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| build_patterns(OVERFLOW_PATTERNS))
}

fn non_overflow_patterns() -> &'static [Regex] {
    static PATTERNS: std::sync::OnceLock<Vec<Regex>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| build_patterns(NON_OVERFLOW_PATTERNS))
}

const OVERFLOW_PATTERNS: &[&str] = &[
    r"prompt is too long",                                                       // Anthropic token overflow
    r"request_too_large",                                                        // Anthropic request byte-size overflow (HTTP 413)
    r"input is too long for requested model",                                    // Amazon Bedrock
    r"exceeds the context window",                                               // OpenAI (Completions & Responses API)
    r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))", // OpenAI-compatible proxies (LiteLLM)
    r"input token count.*exceeds the maximum",                                   // Google (Gemini)
    r"maximum prompt length is \d+",                                             // xAI (Grok)
    r"reduce the length of the messages",                                        // Groq
    r"maximum context length is \d+ tokens",                                     // OpenRouter (most backends)
    r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",          // OpenRouter/Poolside
    r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", // Together AI
    r"exceeds the limit of \d+",                                                 // GitHub Copilot
    r"exceeds the available context size",                                       // llama.cpp server
    r"greater than the context length",                                          // LM Studio
    r"context window exceeds limit",                                             // MiniMax
    r"exceeded model token limit",                                               // Kimi For Coding
    r"too large for model with \d+ maximum context length",                      // Mistral
    r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?", // DS4 server
    r"model_context_window_exceeded",                                            // z.ai non-standard finish_reason surfaced as error text
    r"prompt too long; exceeded (?:max )?context length",                        // Ollama explicit overflow error
    r"range of input length should be",                                          // DashScope / Qwen Token Plan
    r"context[_ ]length[_ ]exceeded",                                           // Generic fallback
    r"too many tokens",                                                          // Generic fallback
    r"token limit exceeded",                                                     // Generic fallback
    r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)",                             // Cerebras: 400/413 with no body
];

const NON_OVERFLOW_PATTERNS: &[&str] = &[
    r"^(Throttling error|Service unavailable):", // AWS Bedrock non-overflow errors
    r"rate limit",                               // Generic rate limiting
    r"too many requests",                        // Generic HTTP 429 style
];

/// Check if an assistant message represents a context overflow error.
///
/// Handles three cases:
/// 1. Error-based overflow: stopReason "error" with a matching message.
/// 2. Silent overflow: successful but usage.input exceeds the context window.
/// 3. Length-stop overflow: stopReason "length" with zero output and input
///    filling the context window.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<f64>) -> bool {
    // Case 1: error message patterns.
    if message.stop_reason == StopReason::Error {
        if let Some(error_message) = &message.error_message {
            let is_non_overflow = non_overflow_patterns().iter().any(|pattern| pattern.is_match(error_message));
            if !is_non_overflow && overflow_patterns().iter().any(|pattern| pattern.is_match(error_message)) {
                return true;
            }
        }
    }

    // Case 2: silent overflow (z.ai style).
    if let Some(context_window) = context_window {
        if message.stop_reason == StopReason::Stop {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if input_tokens > context_window {
                return true;
            }
        }

        // Case 3: length-stop overflow (Xiaomi MiMo style).
        if message.stop_reason == StopReason::Length && message.usage.output == 0.0 {
            let input_tokens = message.usage.input + message.usage.cache_read;
            if input_tokens >= context_window * 0.99 {
                return true;
            }
        }
    }

    false
}

/// Check whether a length stop ended below the caller or model's intended
/// output limit. `desired_max_output` must be the original limit before any
/// context-based clamping.
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: f64) -> bool {
    message.stop_reason == StopReason::Length
        && desired_max_output > 0.0
        && message.usage.output < desired_max_output
}

/// Get the compiled overflow patterns for testing purposes.
pub fn get_overflow_patterns() -> Vec<Regex> {
    build_patterns(OVERFLOW_PATTERNS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Usage, UsageCost};

    fn message(stop_reason: StopReason, error_message: Option<&str>, input: f64, output: f64) -> AssistantMessage {
        AssistantMessage {
            content: vec![],
            api: "test".to_string(),
            provider: "test".to_string(),
            model: "m".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage {
                input,
                output,
                cache_read: 0.0,
                cache_write: 0.0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: input + output,
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
    fn detects_error_based_overflow() {
        assert!(is_context_overflow(
            &message(StopReason::Error, Some("prompt is too long: 213462 tokens > 200000 maximum"), 0.0, 0.0),
            None
        ));
        assert!(is_context_overflow(
            &message(StopReason::Error, Some("Your input exceeds the context window of this model"), 0.0, 0.0),
            None
        ));
        assert!(!is_context_overflow(
            &message(StopReason::Error, Some("Throttling error: Too many tokens, please wait"), 0.0, 0.0),
            None
        ));
        assert!(!is_context_overflow(
            &message(StopReason::Error, Some("rate limit exceeded"), 0.0, 0.0),
            None
        ));
    }

    #[test]
    fn detects_silent_and_length_stop_overflow() {
        assert!(is_context_overflow(&message(StopReason::Stop, None, 100_001.0, 1.0), Some(100_000.0)));
        assert!(!is_context_overflow(&message(StopReason::Stop, None, 99_999.0, 1.0), Some(100_000.0)));
        assert!(is_context_overflow(&message(StopReason::Length, None, 99_500.0, 0.0), Some(100_000.0)));
        assert!(!is_context_overflow(&message(StopReason::Length, None, 99_500.0, 10.0), Some(100_000.0)));
    }

    #[test]
    fn detects_recoverable_length() {
        assert!(is_recoverable_length(&message(StopReason::Length, None, 0.0, 50.0), 200.0));
        assert!(!is_recoverable_length(&message(StopReason::Length, None, 0.0, 200.0), 200.0));
        assert!(!is_recoverable_length(&message(StopReason::Stop, None, 0.0, 50.0), 200.0));
        assert!(!is_recoverable_length(&message(StopReason::Length, None, 0.0, 50.0), 0.0));
    }
}
